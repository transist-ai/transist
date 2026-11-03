# Quickstart

This quickstart runs a simple local workflow from an extracted Transist release
archive in **standalone mode** — no trust roots, no signing, no capability
grants, no network. It covers the minimal lifecycle: export an isolated home,
install the bundled plugin, run a workflow, and inspect the record.

**Looking for the signed, governed walkthrough?** The full offline trusted
loop — trust import, governed home, capability grants, cancel, retry, and
evidence export — is covered in the
[Trusted Governed Execution Walkthrough](../trust/trusted-execution-walkthrough.md),
which is also verified automatically by
`scripts/ci/run_quickstart_smoke.sh`.

## Prerequisites

Installed `workflows check <id>@<version>` validates without plugin probes;
passing validation is not live readiness. Add `--live` or `--output` for
live checks/evidence. `prepare` and `run` still check live readiness before birth.

- An extracted Transist release archive for your platform
  (`transist-<tag>-<target>.tar.gz`) containing `bin/transist`,
  `plugins/shell/`, `examples/quickstart/`, and `.agents/skills/`.
- Python 3 (required by the bundled `shell` plugin).

**On Linux**, the `shell` plugin uses `bwrap` (bubblewrap) to sandbox child
processes. Install it before running any workflow:

| Distribution          | Install command                            |
| --------------------- | ------------------------------------------ |
| Debian / Ubuntu       | `sudo apt-get install bubblewrap`          |
| Fedora / RHEL         | `sudo dnf install bubblewrap`              |
| Arch Linux            | `sudo pacman -S bubblewrap`                |
| Alpine                | `sudo apk add bubblewrap`                  |

macOS uses `sandbox-exec` from the system; no separate install is required.

All commands below run from the extracted archive root with that directory on
your `PATH` (or invoke `./bin/transist` directly). The quickstart never
needs the network.

## 0. Use the archive binaries

```bash
export PATH="$PWD/bin:$PATH"
transist --version
```

## 1. Export an isolated home and initialize it in standalone mode

```bash
export TRANSIST_HOME="$(mktemp -d -t transist-quickstart-XXXXXX)"
trap 'rm -rf "$TRANSIST_HOME"' EXIT
transist home init --mode standalone --home "$TRANSIST_HOME"
```

`--mode standalone` writes a `config.yaml` with `profile: permissive` — no
capability grants are required and unsigned plugins and workflows are
accepted. The temporary `TRANSIST_HOME` is cleaned up by the `trap` when
the shell exits. Passing `--home "$TRANSIST_HOME"` explicitly ensures the
shared per-user home is never touched.

Because `TRANSIST_HOME` is exported, all subsequent `transist` commands on
this shell automatically resolve to it without a `--home` flag.

## 2. Install the bundled shell plugin (unsigned)

```bash
transist plugins install --path plugins/shell/plugin.yaml
```

No `--trusted-execution` flag is needed because the standalone home uses
permissive policy.

## 3. Install the quickstart workflow (unsigned)

```bash
transist workflows install --path examples/quickstart/quickstart.workflow.yaml
```

## 4. Run a workflow

```bash
transist workflows run quickstart.demo@0.1.0 --run-ref qs --input subject=transist-demo --ui plain
```

`run` starts a detached runner. The workflow sleeps briefly (a bounded
window) and then writes a report with evidence under `$TRANSIST_HOME`.

Wait for the run to reach `success`. A single `runs status` may observe the
detached runner before it has finished, so poll in a bounded loop:

```bash
for i in $(seq 1 60); do
  st=$(transist runs status qs 2>/dev/null | head -1 || true)
  case "$st" in
    *success*) echo "run completed"; break ;;
    *failed*|*canceled*) echo "run ended: $st" >&2; break ;;
  esac
  sleep 0.5
done
```

## 5. Inspect the local record

```bash
transist runs show qs
transist runs tail qs --limit 50
```

The run record carries `trusted_execution: false` and `local_only: false`
for this standalone run. The report step promotes an evidence file under:

```text
$TRANSIST_HOME/runs/run_qs/artifacts/write_report/execution_001/evidence/000_summary.txt
```

## What this demonstrates

- A standalone home accepts unsigned plugins and workflows with no policy grants.
- Local execution history, artifacts, and evidence are persisted under
  `TRANSIST_HOME`.
- The temporary home and all generated data are removed by the `trap` cleanup;
  the shared per-user home is never touched.

## Verify it automatically

The standalone leg of `scripts/ci/run_quickstart_smoke.sh` exercises this
path from the extracted archive (or a source build):

```bash
bash scripts/ci/run_quickstart_smoke.sh
```

From a source checkout (debug build):

```bash
cargo build -p transist-cli
bash scripts/ci/run_quickstart_smoke.sh
```

To verify a tarball without extracting it first:

```bash
bash scripts/ci/run_quickstart_smoke.sh --archive ./transist-<tag>-<target>.tar.gz
```

Release CI runs the archive form from the final package on all three supported
native targets: Linux x86-64 (glibc and musl) and macOS Apple Silicon.

## Linux troubleshooting — bubblewrap not found

If you see an error like `bwrap: No such file or directory` or
`failed to sandbox process: bwrap is not available`, `bubblewrap` is not
installed. Install it with the command for your distribution shown in the
Prerequisites section.

Sandboxing is unconditional: Linux process and MCP execution requires `bwrap`
in every mode, and no `transist home init` mode disables it. If you are in a
container or environment where `bwrap` cannot be installed or run, that
environment is unsupported for Linux process execution. Use a different host or
a container image that provides `bubblewrap` (see the Prerequisites section).
