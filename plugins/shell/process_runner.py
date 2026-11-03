"""Calling-thread process I/O with bounded native invocation-group cleanup."""

from __future__ import annotations

import os
import queue
import subprocess
import time
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

from process_lifecycle import PipeReader, ProcessLifecycle, finish_readers


class CleanupUnconfirmedError(RuntimeError):
    """Cleanup or observation was uncertain; retain only captured provider output."""

    def __init__(self, stdout: str = "", stderr: str = "") -> None:
        """Store captured text without exposing native observation/signal errors."""
        super().__init__("Process cleanup could not be confirmed")
        self.stdout = stdout
        self.stderr = stderr


def run_process(
    command: Sequence[str | os.PathLike[str]] | str,
    *,
    cwd: str | Path | None = None,
    timeout: float | None = None,
    input: str | None = None,
    env: Mapping[str, str] | None = None,
    on_line: Callable[[str, str], None] | None = None,
    on_tick: Callable[[], None] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run in the inherited runtime group, returning captured text and exit code.

    All callbacks run on the caller's thread, including during cleanup. Input is
    finite text delivered nonblockingly alongside output collection. Timeout is
    a monotonic execution deadline; cleanup adds at most four seconds of native
    TERM/KILL waiting and one second of reader drain. Callback/launch exceptions
    propagate after confirmed cleanup; uncertainty takes precedence and exposes
    only captured stdout/stderr. No new process group or session is created.
    """
    output: queue.Queue[tuple[str, str]] = queue.Queue()
    captured: dict[str, list[str]] = {"stdout": [], "stderr": []}
    readers: list[PipeReader] = []
    process: subprocess.Popen[str] | None = None
    lifecycle: ProcessLifecycle | None = None
    error: BaseException | None = None
    uncertain = False
    callbacks = True
    deadline = None if timeout is None else time.monotonic() + timeout
    pending = b""
    offset = 0

    def drain(until: float | None = None) -> None:
        """Collect a bounded batch, disabling callbacks after their first error."""
        nonlocal uncertain, error, callbacks
        for _ in range(128):
            if until is not None and time.monotonic() >= until:
                break
            try:
                stream, line = output.get_nowait()
            except queue.Empty:
                break
            if stream == "reader_error":
                uncertain = True
                callbacks = False
                continue
            captured[stream].append(line)
            if callbacks and on_line is not None:
                try:
                    on_line(stream, line)
                except BaseException as exc:
                    error = error or exc
                    callbacks = False

    def tick(until: float | None = None) -> None:
        """Drain without allowing user exceptions to interrupt native cleanup."""
        nonlocal error, callbacks
        drain(until)
        if callbacks and on_tick is not None:
            try:
                on_tick()
            except BaseException as exc:
                error = error or exc
                callbacks = False

    try:
        try:
            # This snapshot MUST precede launch, even for test doubles.
            lifecycle = ProcessLifecycle()
        except Exception:
            uncertain = True
        if not uncertain:
            process = subprocess.Popen(
                command,
                cwd=cwd,
                env=env,
                stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                for name in ("stdout", "stderr"):
                    reader = PipeReader(getattr(process, name), output, name)
                    readers.append(reader)
                    reader.start()
                if input is not None:
                    pending = input.encode(process.stdin.encoding or "utf-8")
                    os.set_blocking(process.stdin.fileno(), False)
            except Exception:
                # Partial reader/stdin setup still requires full group cleanup.
                uncertain = True
            while not uncertain and error is None:
                tick()
                if uncertain or error is not None or process.poll() is not None:
                    break
                if deadline is not None and time.monotonic() >= deadline:
                    error = subprocess.TimeoutExpired(command, timeout)
                    break
                if process.stdin is not None and not process.stdin.closed:
                    if offset < len(pending):
                        try:
                            # One bounded write per turn: never wait for stdin
                            # while the provider is blocked writing its output.
                            offset += os.write(
                                process.stdin.fileno(), pending[offset : offset + 16384]
                            )
                        except BlockingIOError:
                            pass
                        except BrokenPipeError:
                            process.stdin.close()
                    if offset == len(pending) and not process.stdin.closed:
                        process.stdin.close()
                time.sleep(0.005)
    except BaseException as exc:
        error = error or exc
        callbacks = False
    finally:
        # Closing stdin cannot wait: all writes bypass the text stream buffer.
        if process is not None and process.stdin is not None:
            try:
                process.stdin.close()
            except Exception:
                uncertain = True
        if lifecycle is not None:
            try:
                uncertain |= not lifecycle.cleanup(tick)
            except BaseException:
                uncertain = True
        drain_deadline = time.monotonic() + 1
        try:
            # Drain EOF fully before requesting stop; inherited writers get only
            # this shared one-second budget, never an unbounded communicate().
            while any(reader.thread.is_alive() for reader in readers):
                tick(drain_deadline)
                if time.monotonic() >= drain_deadline - 0.1:
                    break
                time.sleep(0.005)
        finally:
            try:
                uncertain |= not finish_readers(readers, drain_deadline)
            except BaseException:
                uncertain = True
            while not output.empty() and time.monotonic() < drain_deadline:
                drain(drain_deadline)
            if not output.empty():
                uncertain = True
                callbacks = False
            if process is not None:
                # Cover constructor/start failures that left a pipe unowned.
                for name in ("stdout", "stderr"):
                    stream = getattr(process, name)
                    if stream is not None and not stream.closed:
                        try:
                            stream.close()
                        except Exception:
                            uncertain = True
                try:
                    process.wait(timeout=max(0, drain_deadline - time.monotonic()))
                except Exception:
                    uncertain = True

    stdout = "".join(captured["stdout"])
    stderr = "".join(captured["stderr"])
    if uncertain:
        raise CleanupUnconfirmedError(stdout, stderr) from None
    if isinstance(error, subprocess.TimeoutExpired):
        error.output = stdout
        error.stderr = stderr
    if error is not None:
        raise error
    assert process is not None
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
