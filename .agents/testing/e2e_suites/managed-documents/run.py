#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise real managed-document execution in disposable signed homes."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import tomllib

SUITE = Path(__file__).resolve().parent
RUNTIME_VERSION = tomllib.loads((SUITE.parents[3] / "Cargo.toml").read_text())[
    "workspace"
]["package"]["version"]


def command(
    args: list[str], cwd: Path, environment: dict[str, str] | None = None
) -> str:
    """Run a required command, preserving diagnostics on any nonzero exit."""
    result = subprocess.run(
        args, cwd=cwd, env=environment, capture_output=True, text=True, timeout=90
    )
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stdout}\n{result.stderr}")
    return result.stdout


def read_json(path: Path) -> dict:
    """Load one actual runtime record."""
    return json.loads(path.read_text())


def inspect_run(root: Path) -> None:
    """Assert exact business bytes, complete publications and nonpersistent staging."""
    state = read_json(root / "state.json")
    assert state["status"] == "success", json.dumps(state, indent=2)
    assert state["cleanup"]["state"] == "succeeded"
    heads = read_json(root / "documents" / "heads.json")
    assert set(heads) == {"file", "tree", "final", "invocation"}
    metadata = read_json(
        root / "documents" / "blobs" / heads["invocation"]["blob"]["sha256"]
    )
    assert metadata == {
        "runtime": {"version": RUNTIME_VERSION},
        "workflow": {"id": "managed.documents"},
        "run": {"id": state["run_id"]},
        "plugins": [{"name": "fixture.documents", "version": "0.1.0"}],
    }, metadata
    print("METADATA " + json.dumps(metadata, sort_keys=True), flush=True)
    for name, expected in [("file", b"initialchanged"), ("final", b"cleanup")]:
        blob = heads[name]["blob"]
        content = (root / "documents" / "blobs" / blob["sha256"]).read_bytes()
        assert content == expected
        assert hashlib.sha256(content).hexdigest() == blob["sha256"]
    initial = read_json(root / "documents" / "initial.json")["document_revisions"][0]
    assert (
        root / "documents" / "blobs" / initial["blob"]["sha256"]
    ).read_bytes() == b"initial"
    consume = state["steps"]["consume"]
    assert consume["last_request"]["input"]["ops"][0]["path"] == {
        "$document": "file",
        "access": "read",
    }
    assert consume["last_result"]["output"]["paths"][0]["exact"] == {
        "$document": "file",
        "access": "read",
    }
    assert consume["last_result"]["output"]["paths"][0]["nested"] == [
        "prefix=$document:file/child suffix"
    ]
    for path in root.rglob("*"):
        if path.is_file():
            data = path.read_bytes()
            assert b"transist-runtime-temp-" not in data, path
            assert b".transist-staging/invoke-" not in data, path


def run_case(runtime: str, admin: str, base: Path, mode: str) -> None:
    """Install committed fixtures, execute and replay in one new isolated home."""
    root = base / mode
    root.mkdir()
    home = root / "home"
    workspace = root / "workspace"
    workspace.mkdir()
    source = root / "source"
    source.mkdir()
    shutil.copyfile(SUITE / "workflow.yaml", source / "workflow.yaml")
    shutil.copytree(SUITE / "modules", source / "modules")
    plugin = root / "plugin"
    shutil.copytree(SUITE / "plugin", plugin)
    (plugin / "plugin.py").chmod(0o755)
    command([runtime, "--home", str(home), "home", "init", "--mode", mode], workspace)
    if mode == "governed":
        config = f"""version: v1
runtime: {{mode: governed}}
policy:
  profile: allowlist
  grant_sets:
    documents:
      plugin_grants:
        - plugin: fixture.documents@0.1.0
          actions: [call]
      capabilities:
        - capability: filesystem
          resource: 'workspace://{workspace}'
          operations: [list, read]
        - capability: filesystem
          resource: 'run://artifacts'
          operations: [create, write]
        - capability: filesystem
          resource: 'run://evidence'
          operations: [create, write]
  workflow_grants:
    - {{workflow: managed.documents@0.1.0, use: [documents]}}
"""
        (home / "config.yaml").write_text(config)
        key = root / "test.key"
        public = root / "test.pub"
        command(
            [
                admin,
                "key",
                "generate",
                "--name",
                "documents-e2e",
                "--private-key-file",
                str(key),
            ],
            workspace,
        )
        command(
            [
                admin,
                "key",
                "export-public",
                "--private-key-file",
                str(key),
                "--trust-root-file",
                str(public),
            ],
            workspace,
        )
        command(
            [runtime, "--home", str(home), "trust", "import", str(public)], workspace
        )
        command(
            [
                admin,
                "plugin",
                "sign",
                str(plugin),
                "--private-key-file",
                str(key),
                "--output",
                str(root / "plugin.sig"),
            ],
            workspace,
        )
        package = root / "package"
        command(
            [
                admin,
                "workflow",
                "package",
                str(source / "workflow.yaml"),
                "--version",
                "0.1.0",
                "--private-key-file",
                str(key),
                "--output",
                str(package),
            ],
            workspace,
        )
        key.unlink()
        trusted = ["--trusted-execution"]
        install = ["--package", str(package)]
    else:
        trusted = []
        install = ["--path", str(source / "workflow.yaml")]
    prefix = [runtime, "--home", str(home)]
    command(
        prefix
        + ["plugins", "install", "--path", str(plugin / "plugin.yaml")]
        + trusted,
        workspace,
    )
    command(prefix + ["workflows", "install"] + install + ["--approve"], workspace)
    inputs = root / "inputs.json"
    inputs.write_text(
        json.dumps(
            {"repo_path": str(workspace), "home_probe": str(home / "config.yaml")}
        )
    )
    command(
        prefix
        + [
            "workflows",
            "check",
            "managed.documents@0.1.0",
            "--inputs-file",
            str(inputs),
        ]
        + trusted,
        workspace,
    )
    # CLI mutation uses a real detached runner. Poll persisted terminal state with
    # a finite deadline; absence, failure and timeout are all hard failures.
    command(
        prefix
        + [
            "workflows",
            "run",
            "managed.documents@0.1.0",
            "--run-ref",
            "source",
            "--inputs-file",
            str(inputs),
            "--ui",
            "plain",
        ]
        + trusted,
        workspace,
    )
    for run_id in ["source", "replay"]:
        if run_id == "replay":
            command(
                prefix
                + ["runs", "replay", "source", "--run-ref", "replay", "--ui", "plain"]
                + trusted,
                workspace,
            )
        run_root = home / "runs" / f"run_{run_id}"
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            state_file = run_root / "state.json"
            if state_file.exists():
                state = read_json(state_file)
                if state["status"] in {"success", "failed", "canceled"} and state.get(
                    "cleanup", {}
                ).get("state") in {"succeeded", "failed", "skipped"}:
                    break
            time.sleep(0.1)
        else:
            raise TimeoutError(f"run did not finish: {run_root}")
        inspect_run(run_root)
        command(prefix + ["runs", "show", run_id], workspace)
        wait_for_runners(base)
    first = read_json(home / "runs/run_source/documents/heads.json")
    second = read_json(home / "runs/run_replay/documents/heads.json")
    assert first["file"]["revision_id"] != second["file"]["revision_id"]
    assert not (workspace / "forbidden").exists()
    print(
        f"OK: {mode} file/tree/update/modules/cleanup/read-only/home isolation/replay",
        flush=True,
    )


def run_workspace_alias_case(runtime: str, base: Path) -> None:
    """Run real APP with TMPDIR inside a full workspace and home outside it."""
    root = base / "workspace-alias"
    workspace = root / "workspace"
    temporary = workspace / "tmp"
    temporary.mkdir(parents=True)
    home = root / "home"
    plugin = root / "plugin"
    shutil.copytree(SUITE / "plugin", plugin)
    (plugin / "plugin.py").chmod(0o755)
    prefix = [runtime, "--home", str(home)]
    environment = dict(os.environ, TMPDIR=str(temporary))
    command(prefix + ["home", "init", "--mode", "standalone"], workspace, environment)
    command(
        prefix + ["plugins", "install", "--path", str(plugin / "plugin.yaml")],
        workspace,
        environment,
    )
    command(
        prefix
        + [
            "workflows",
            "install",
            "--path",
            str(SUITE / "workspace-alias.workflow.yaml"),
            "--approve",
        ],
        workspace,
        environment,
    )
    inputs = root / "inputs.json"
    inputs.write_text(json.dumps({"repo_path": str(workspace)}))
    command(
        prefix
        + [
            "workflows",
            "run",
            "managed.documents.alias@0.1.0",
            "--run-ref",
            "alias",
            "--inputs-file",
            str(inputs),
            "--ui",
            "plain",
        ],
        workspace,
        environment,
    )
    state_file = home / "runs/run_alias/state.json"
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if state_file.exists() and read_json(state_file)["status"] in {
            "success",
            "failed",
            "canceled",
        }:
            break
        time.sleep(0.1)
    else:
        raise TimeoutError("workspace alias run did not finish")
    wait_for_runners(base)
    state = read_json(state_file)
    assert state["status"] == "success", json.dumps(state, indent=2)
    assert state["steps"]["read"]["last_result"]["output"]["seen"] == ["initial"]
    heads = read_json(home / "runs/run_alias/documents/heads.json")
    assert heads["file"]["execution"] == 0
    assert not list(temporary.glob("transist-runtime-temp-*"))
    print("OK: full workspace TMPDIR alias denied during real APP read", flush=True)


def runner_pids(base: Path) -> list[int]:
    """Identify only detached runtime processes using this unique fixture root."""
    listing = subprocess.run(
        ["ps", "-axo", "pid=,stat=,command="],
        capture_output=True,
        text=True,
        check=True,
        timeout=5,
    ).stdout
    pids = []
    for line in listing.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3 and str(base) in fields[2]:
            if "/transist " in fields[2] and "Z" not in fields[1]:
                pids.append(int(fields[0]))
    return pids


def wait_for_runners(base: Path) -> None:
    """Require runner termination before deleting homes or reporting success."""
    deadline = time.monotonic() + 10
    while runner_pids(base):
        if time.monotonic() >= deadline:
            raise TimeoutError("fixture runner did not terminate")
        time.sleep(0.1)


def stop_runners(base: Path) -> None:
    """Reap only this suite's detached runners when an assertion fails."""
    for sig in [signal.SIGTERM, signal.SIGKILL]:
        for pid in runner_pids(base):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 2
        while runner_pids(base) and time.monotonic() < deadline:
            time.sleep(0.1)
    if runner_pids(base):
        raise RuntimeError("fixture runner cleanup failed")


def main() -> None:
    """Require binaries and the native sandbox, then run both isolated cases."""
    runtime, admin = [str(Path(path).resolve()) for path in sys.argv[1:]]
    for binary in [runtime, admin]:
        if not os.access(binary, os.X_OK):
            raise RuntimeError(f"required binary unavailable: {binary}")
    if sys.platform == "darwin":
        assert Path("/usr/bin/sandbox-exec").is_file(), "native sandbox required"
    else:
        assert shutil.which("bwrap"), "bubblewrap required"
    with tempfile.TemporaryDirectory(
        prefix=".transist-managed-documents-"
    ) as temporary:
        base = Path(temporary).resolve()
        try:
            for mode in ["standalone", "governed"]:
                run_case(runtime, admin, base, mode)
            run_workspace_alias_case(runtime, base)
        finally:
            stop_runners(base)
    print("PASS: managed-documents", flush=True)


if __name__ == "__main__":
    main()
