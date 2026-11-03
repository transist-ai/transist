"""Bounded invocation-group cleanup using only native OS metadata and stdlib.

No command lines or environments are inspected. The runtime owns group isolation;
this module never creates a session/group and never signals a group wholesale.
"""

from __future__ import annotations

import codecs
import ctypes
import errno
import io
import os
import queue
import select
import signal
import sys
import threading
import time
from dataclasses import dataclass
from typing import Callable


@dataclass(frozen=True)
class Identity:
    """Native process identity, parent, group, and liveness snapshot."""

    pid: int
    birth: tuple[int, ...]
    ppid: int
    pgid: int
    state: int | str

    @property
    def live(self) -> bool:
        """Exclude exited/zombie processes, which cannot execute or hold pipes."""
        return self.state not in ("Z", "X", 5)


class BSDInfo(ctypes.Structure):
    """Darwin proc_bsdinfo ABI (PROC_PIDTBSDINFO), including birth time."""

    _fields_ = (
        [
            (name, ctypes.c_uint32)
            for name in (
                "flags",
                "status",
                "xstatus",
                "pid",
                "ppid",
                "uid",
                "gid",
                "ruid",
                "rgid",
                "svuid",
                "svgid",
                "rfu1",
            )
        ]
        + [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
        + [
            (name, ctypes.c_uint32)
            for name in (
                "nfiles",
                "pgid",
                "pjobc",
                "tdev",
                "tpgid",
                "nice",
            )
        ]
        + [("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64)]
    )


class NativeProcesses:
    """Read process identities and signal verified members on Linux or Darwin."""

    def __init__(self) -> None:
        self.lib = None
        if sys.platform == "darwin":
            self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            self.lib.proc_pidinfo.argtypes = [
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_uint64,
                ctypes.c_void_p,
                ctypes.c_int,
            ]
            self.lib.proc_pidinfo.restype = ctypes.c_int
            self.lib.proc_listpids.argtypes = [
                ctypes.c_uint32,
                ctypes.c_uint32,
                ctypes.c_void_p,
                ctypes.c_int,
            ]
            self.lib.proc_listpids.restype = ctypes.c_int
        elif sys.platform != "linux":
            raise OSError("unsupported process observation platform")

    def read(self, pid: int) -> Identity | None:
        """Return native metadata, None only for confirmed disappearance."""
        if self.lib is not None:
            info = BSDInfo()
            ctypes.set_errno(0)
            size = self.lib.proc_pidinfo(
                pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)
            )
            if size != ctypes.sizeof(info):
                error = ctypes.get_errno()
                if error == errno.ESRCH:
                    return None
                raise OSError(error, "process metadata unavailable")
            return Identity(
                pid,
                (info.start_sec, info.start_usec),
                info.ppid,
                info.pgid,
                info.status,
            )
        try:
            with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
                # comm may contain spaces and closing parentheses; never retain it.
                fields = handle.read().rsplit(")", 1)[1].split()
            return Identity(
                pid, (int(fields[19]),), int(fields[1]), int(fields[2]), fields[0]
            )
        except FileNotFoundError:
            return None

    def members(self, pgid: int) -> list[Identity]:
        """Enumerate group members; uncertain/denied observation raises."""
        if self.lib is None:
            pids = [int(name) for name in os.listdir("/proc") if name.isdecimal()]
        else:
            # Retry a full buffer: silently truncated enumeration is not proof.
            capacity = 256
            while True:
                buffer = (ctypes.c_int * capacity)()
                size = self.lib.proc_listpids(2, pgid, buffer, ctypes.sizeof(buffer))
                if size <= 0:
                    raise OSError("process enumeration unavailable")
                if size < ctypes.sizeof(buffer):
                    pids = list(buffer)[: size // ctypes.sizeof(ctypes.c_int)]
                    break
                capacity *= 2
                if capacity > 1048576:
                    raise OSError("process enumeration incomplete")
        result = []
        for pid in pids:
            if pid <= 0:
                continue
            info = self.read(pid)
            if info is not None and info.pgid == pgid:
                result.append(info)
        return result

    def ancestor(self, pid: int) -> tuple[int, tuple[int, ...] | None]:
        """Read a protected ancestor's parent; denied birth never permits a kill.

        Darwin exposes short parent/group metadata for system ancestors whose
        full BSD info is privileged. Such ancestors are protected by PID forever,
        without claiming a birth identity or using this fallback for candidates.
        """
        try:
            info = self.read(pid)
        except PermissionError:
            if self.lib is None:
                raise
            buffer = (ctypes.c_uint32 * 16)()
            size = self.lib.proc_pidinfo(pid, 13, 0, buffer, ctypes.sizeof(buffer))
            if size != ctypes.sizeof(buffer) or buffer[0] != pid:
                raise OSError("ancestor observation unavailable")
            return int(buffer[1]), None
        if info is None:
            raise OSError("ancestor observation unavailable")
        return info.ppid, info.birth

    def send(self, expected: Identity, sig: int) -> bool:
        """Signal only a still-live matching identity/group; Linux uses pidfd."""
        fd = None
        try:
            if self.lib is None:
                fd = os.pidfd_open(expected.pid)
            current = self.read(expected.pid)
            if current is None:
                return True
            if current.birth != expected.birth or current.pgid != expected.pgid:
                return False
            if not current.live:
                return True
            if fd is not None:
                signal.pidfd_send_signal(fd, sig)
            else:
                # Darwin has no pidfd: verify birth and group immediately before kill.
                os.kill(expected.pid, sig)
            return True
        except ProcessLookupError:
            return True
        except (OSError, AttributeError):
            return False
        finally:
            if fd is not None:
                os.close(fd)


class ProcessLifecycle:
    """Protect pre-launch ancestors/members and finalize the inherited group."""

    def __init__(self) -> None:
        self.native = NativeProcesses()
        self.pgid = os.getpgrp()
        self.protected = {}
        self.owned = {}
        self.confirmed = True
        # Protect PIDs as well as identities: reuse must never authorize a kill.
        pid = os.getpid()
        while pid:
            if pid in self.protected:
                raise OSError("ancestor observation incomplete")
            # PID 1 is the OS init root, not a candidate invocation member.
            # Darwin may deny its birth metadata even outside a sandbox. Protect
            # its PID unconditionally rather than requiring permission to read it.
            if pid == 1:
                self.protected[pid] = None
                break
            parent, birth = self.native.ancestor(pid)
            self.protected[pid] = birth
            pid = parent
        for info in self.native.members(self.pgid):
            self.protected[info.pid] = info.birth

    def remaining(self) -> list[Identity]:
        """Rescan owned live members, rejecting PID reuse and observation errors."""
        result = []
        for info in self.native.members(self.pgid):
            if info.pid in self.protected:
                if (
                    self.protected[info.pid] is not None
                    and info.birth != self.protected[info.pid]
                ):
                    self.confirmed = False
                continue
            birth = self.owned.setdefault(info.pid, info.birth)
            if birth != info.birth:
                self.confirmed = False
                continue
            if info.live:
                result.append(info)
        return result

    def cleanup(self, tick: Callable[[], None]) -> bool:
        """TERM then KILL with two total two-second budgets; rescan for proof.

        tick drains provider events and keeps APP heartbeat active during cleanup.
        Uncertainty is sticky and never permits an unverified signal.
        """
        try:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                deadline = time.monotonic() + 2
                sent = set()
                while True:
                    members = self.remaining()
                    if not members:
                        return self.confirmed
                    for info in members:
                        key = (info.pid, info.birth)
                        if key not in sent:
                            self.confirmed &= self.native.send(info, sig)
                            sent.add(key)
                    tick()
                    left = deadline - time.monotonic()
                    if left <= 0:
                        break
                    time.sleep(min(0.025, left))
            return not self.remaining() and self.confirmed
        except Exception:
            return False


class PipeReader:
    """Interruptible nonblocking pipe reader; it only queues text, never APP frames."""

    def __init__(self, stream: io.TextIOBase, output: queue.Queue, name: str) -> None:
        self.stream = stream
        self.output = output
        self.name = name
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._read, daemon=True)

    def _read(self) -> None:
        try:
            # In-memory smoke fixtures have no OS pipe; their reads cannot block.
            if isinstance(self.stream, io.StringIO):
                for line in self.stream:
                    self.output.put((self.name, line))
                return
            fd = self.stream.fileno()
            os.set_blocking(fd, False)
            decoder = codecs.getincrementaldecoder(self.stream.encoding or "utf-8")(
                errors="replace"
            )
            pending = ""
            while not self.stop.is_set():
                if not select.select([fd], [], [], 0.025)[0]:
                    continue
                try:
                    chunk = os.read(fd, 65536)
                except BlockingIOError:
                    continue
                pending += decoder.decode(chunk, final=not chunk)
                while "\n" in pending:
                    line, pending = pending.split("\n", 1)
                    self.output.put((self.name, line + "\n"))
                if not chunk:
                    break
            if pending:
                self.output.put((self.name, pending))
        except Exception:
            self.output.put(("reader_error", ""))
        finally:
            self.stream.close()

    def start(self) -> None:
        """Start the reader; the caller owns bounded stopping/joining."""
        self.thread.start()


def finish_readers(readers: list[PipeReader], deadline: float | None = None) -> bool:
    """Allow EOF drain, then stop/join all readers within one total second."""
    deadline = min(deadline or float("inf"), time.monotonic() + 1)
    # Give ordinary EOF a short opportunity without waiting on inherited pipes.
    for reader in readers:
        if reader.thread.ident is not None:
            reader.thread.join(max(0, min(0.05, deadline - time.monotonic())))
    for reader in readers:
        reader.stop.set()
    for reader in readers:
        if reader.thread.ident is not None:
            reader.thread.join(max(0, deadline - time.monotonic()))
        else:
            reader.stream.close()
    return all(not reader.thread.is_alive() for reader in readers)
