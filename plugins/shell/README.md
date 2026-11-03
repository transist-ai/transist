# `shell` Plugin

The `shell` plugin is the official, first-party baseline for `process` execution in Transist. It is used extensively in our Quick Start guides, core examples, and compatibility tests.

## Why Use It?

The `shell` plugin is the simplest way to run local commands. It’s perfect for bridging Transist workflows with existing CLI tools, Python scripts, or custom binaries.

`source://` paths resolve inside the recipe source directory during compilation and are supported in both `command` and `args`; traversal, missing files, and symlink escapes are rejected. Snapshot-backed runs materialize captured helpers into a private per-invocation staging directory before dispatch, so execution does not depend on the authored source tree.

## Command lifecycle

`exec` and `exec_with_secret_env` use packaged `process_runner.py` and
`process_lifecycle.py`; `echo` stays in-process. Safe main-thread status heartbeats
keep quiet commands active without previewing arguments or environment values.
Commands retain no wrapper timeout (`timeout=None`). Terminal results follow
inherited-process-group TERM/KILL cleanup and bounded pipe-reader finalization
(five seconds); the wrapper creates no process groups. Unconfirmed cleanup
returns nonretryable `protocol_error` with valid captured stdout/stderr hints;
unexpected runner failures return safe `adapter_internal_error`.

## Available Actions

- **`exec`**: Runs a shell command and captures the output. Files the command writes to `$TRANSIST_EVIDENCE_DIR` are promoted as run-scoped evidence hints (UTF-8 text only; secrets are redacted by the runtime before persistence). A workflow `gate_decision` binding supplies the validated decision as `TRANSIST_GATE_DECISION_JSON` to the child; absent decisions clear that variable even when inherited from the plugin environment.
- **`exec_with_secret_env`**: Runs a shell command with `MY_API_TOKEN` delivered as `API_TOKEN`.
- **`echo`**: A simple diagnostic tool to print text to the Transist event stream.

`exec` and `echo` accept string, number, and boolean arguments. Numbers use JSON
formatting and booleans become lowercase `true` / `false`. The command itself
must be a string. The `exec_with_secret_env` action retains string-only args.

## Locality

The manifest declares `default_provider_kind: local`, so every action is a
local provider. Under `--local-only`, `exec` and `exec_with_secret_env` are
allowed to dispatch, and the runtime caps the shell process at `network: loopback`
so it cannot make non-loopback connections.

## Support Status

This is an **Official v1 Plugin**. Its manifest, installation flow, and execution model are fully supported and covered by automated regression tests in the main Transist repository. The current release version is `0.1.0`; `v1` identifies the manifest format.

The manifest's `package.files` list selects the complete runtime payload. It
includes the entrypoint and action schemas; `plugin.yaml` is implicit. Files
outside that list are not signed or installed.

## Runtime environment and paths

The runtime injects `TRANSIST_WORKSPACE_DIR`, `TRANSIST_ARTIFACTS_DIR`,
`TRANSIST_EVIDENCE_DIR`, and (when available) `TRANSIST_RUNTIME_METADATA_FILE`.
These paths are invocation-private runtime locations; only the declared workspace
is workflow authority. The runtime forces `TMPDIR` to the current private
runtime-temporary root after plugin, workflow, user, and secret environment
processing. Temporary-file consumers that honor `TMPDIR` therefore write only
inside that invocation-owned root; macOS BSD `mktemp` requires an explicit
template under `$TMPDIR`.

`exec_with_secret_env` receives only the declared `MY_API_TOKEN` secret as
`API_TOKEN`; it is not persisted or exposed to other actions. When `command` or
an `args` entry uses `repo://`, `repo_path` is required and selects the declared
repository root for that path. The workflow must declare corresponding exact
filesystem and secret capabilities; these environment variables do not widen
policy authority.

`TRANSIST_GATE_DECISION_JSON` is plugin-managed, is forwarded only for `exec`
when the runtime injects a decision, and needs no operator environment grant.
It is absent for an initial iteration without a decision. Child commands must
validate the decision before acting on optional actor or reason fields.

## Installation

Governed homes require trusted execution; keep `plugins/shell.sig` next to the
package and import the approving public root first:

```bash
# Import the approving public trust root on the runtime host.
transist trust import ./trust/acme.pub

# Install the plugin (the .sig sidecar is verified when --trusted-execution is on).
transist plugins install --path plugins/shell/plugin.yaml --trusted-execution

# Verify the installation
transist plugins inspect shell
```
