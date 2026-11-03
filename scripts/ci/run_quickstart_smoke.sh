#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# run_quickstart_smoke.sh — Execute both documented offline quickstart paths as
# release-blocking contracts.
#
# Usage:
#   bash scripts/ci/run_quickstart_smoke.sh                     # source checkout (debug build)
#   bash scripts/ci/run_quickstart_smoke.sh --archive <archive> # extracted release archive tarball
#
# Two legs run on every invocation:
#
#   Leg 1 — Governed/trusted/signed walkthrough (contract for
#   docs/public/guides/trust/trusted-execution-walkthrough.md):
#     1. initialize a governed home inside a temporary project/workspace
#     2. import the explicitly demo-only public trust root
#     3. package and install the signed quickstart workflow without inputs
#     4. check, authorize exact missing grants, and recheck
#     5. run with --trusted-execution and --local-only
#     6. cancel during a bounded sleep
#     7. retry the same run
#     8. inspect state, events, artifacts, and evidence
#     8. export evidence
#
#   Leg 2 — Standalone quickstart (contract for
#   docs/public/guides/getting-started/quickstart.md):
#     1. export an isolated TRANSIST_HOME to a temp dir (trap cleanup)
#     2. initialize a standalone home (permissive policy, no trust roots)
#     3. install the bundled shell plugin unsigned
#     4. install the quickstart workflow unsigned
#     5. run and wait for success
#     6. inspect run state
#
# Both legs run with outbound network disabled at the host level where a
# platform isolation tool is available (unshare -n on Linux, sandbox-exec
# on macOS). When no tool is available, the runtime-level --local-only
# boundary (leg 1) and the shell plugin's declared local provider_kind
# remain enforced and a clear notice is printed.
#
# The generated homes and workspaces are removed through traps. No generated
# secret, private key, or runtime home ever enters the checkout. The runtime
# job runs no key-generation or signing command.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

archive_path=""
trust_dir=""
require_isolation=0
transist_bin_override=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --archive)
      archive_path="${2:-}"
      if [[ -z "$archive_path" ]]; then
        echo "Usage: $0 --archive <archive>" >&2
        exit 1
      fi
      shift 2
      ;;
    --trust-dir)
      trust_dir="${2:-}"
      if [[ -z "$trust_dir" ]]; then
        echo "Usage: $0 --trust-dir <dir>" >&2
        exit 1
      fi
      shift 2
      ;;
    --transist-bin)
      transist_bin_override="${2:-}"
      if [[ -z "$transist_bin_override" ]]; then
        echo "Usage: $0 --transist-bin <binary>" >&2
        exit 1
      fi
      shift 2
      ;;
    --require-isolation)
      require_isolation=1
      shift
      ;;
    *)
      break
      ;;
  esac
done

WORKFLOW_ID="quickstart.demo"
WORKFLOW_REF="${WORKFLOW_ID}@0.1.0"
RUN_ID="quickstart-run"
SUBJECT="transist-demo"

work_root=""
pkg_dir=""
transist_bin=""
shell_plugin_dir=""
workflow_path=""
workflow_sig=""
shell_sig=""
trust_root=""

# shellcheck disable=SC2329
cleanup() {
  if [[ -n "$work_root" && -d "$work_root" ]]; then
    rm -rf "$work_root"
  fi
}
trap cleanup EXIT

if [[ -n "$archive_path" ]]; then
  if [[ ! -f "$archive_path" ]]; then
    echo "FAIL: archive not found: $archive_path" >&2
    exit 1
  fi
  extract_dir="$(mktemp -d)"
  trap 'rm -rf "$extract_dir"; cleanup' EXIT
  if ! tar -xzf "$archive_path" -C "$extract_dir" 2>/dev/null; then
    echo "FAIL: could not extract archive: $archive_path" >&2
    exit 1
  fi
  pkg_dir="$(find "$extract_dir" -maxdepth 1 -type d -name 'transist-*' | head -1)"
  if [[ -z "$pkg_dir" ]]; then
    echo "FAIL: archive has no transist-* top-level directory" >&2
    exit 1
  fi
  transist_bin="$pkg_dir/bin/transist"
  shell_plugin_dir="$pkg_dir/plugins/shell"
  workflow_path="$pkg_dir/examples/quickstart/quickstart.workflow.yaml"
  workflow_sig="$pkg_dir/examples/quickstart/quickstart.workflow.yaml.sig"
  quickstart_config="$pkg_dir/examples/quickstart/quickstart.config.yaml"
  shell_sig="$pkg_dir/plugins/shell.sig"
  trust_root="$pkg_dir/examples/quickstart/trust/quickstart-demo.pub"
else
  # Source mode or extracted-archive-root mode.
  # If bin/transist exists in the repo root, we are running from an
  # extracted release archive; otherwise look for built binaries.
  if [[ -x "$REPO_ROOT/bin/transist" ]]; then
    transist_bin="$REPO_ROOT/bin/transist"
  elif [[ -n "$transist_bin_override" ]]; then
    if [[ -x "$transist_bin_override" ]]; then
      transist_bin="$transist_bin_override"
    else
      echo "FAIL: --transist-bin binary not found/executable: $transist_bin_override" >&2
      exit 1
    fi
  else
    bin_candidate=""
    for candidate in \
      "$REPO_ROOT/target/x86_64-unknown-linux-musl/release/transist" \
      "$REPO_ROOT/target/release/transist" \
      "$REPO_ROOT/target/debug/transist"; do
      if [[ -x "$candidate" ]]; then
        bin_candidate="$candidate"
        break
      fi
done

# Resolve trust_dir to an absolute path so it survives `cd` inside the runner
# script's isolated workspace.
if [[ -n "$trust_dir" ]]; then
  if [[ ! -d "$trust_dir" ]]; then
    echo "FAIL: --trust-dir does not exist: $trust_dir" >&2
    exit 1
  fi
  trust_dir="$(cd "$trust_dir" && pwd -P)"
fi
    if [[ -z "$bin_candidate" ]]; then
      echo "FAIL: no built transist binary found; run cargo build -p transist-cli" >&2
      exit 1
    fi
    transist_bin="$bin_candidate"
  fi

  # Resolve transist_bin to an absolute path so it survives the `cd` inside the
  # runner script's isolated workspace. `--transist-bin` may arrive relative
  # (e.g. target/x86_64-unknown-linux-musl/release/transist from CI); relative
  # works for the existence check above but breaks after the runner `cd`s.
  if [[ -n "$transist_bin" ]]; then
    transist_bin="$(cd "$(dirname "$transist_bin")" && pwd -P)/$(basename "$transist_bin")"
  fi
  shell_plugin_dir="$REPO_ROOT/plugins/shell"
  workflow_path="$REPO_ROOT/examples/quickstart/quickstart.workflow.yaml"
  workflow_sig="$REPO_ROOT/examples/quickstart/quickstart.workflow.yaml.sig"
  quickstart_config="$REPO_ROOT/examples/quickstart/quickstart.config.yaml"
  shell_sig="$REPO_ROOT/plugins/shell.sig"
  trust_root="$REPO_ROOT/examples/quickstart/trust/quickstart-demo.pub"
fi

# Override signature and trust root paths with admin-generated public material
# when --trust-dir is provided. This lets CI pass only public output from the
# isolated admin sign/verify job to the runtime smoke. The admin signatures
# are staged next to the plugin/workflow in a temp directory so the install
# commands find them as sidecars. Evaluation archives also ship their original
# demo signatures and public trust root; this option tests the isolated admin path.
if [[ -n "$trust_dir" ]]; then
  admin_workflow_sig="$trust_dir/examples/quickstart/quickstart.workflow.yaml.sig"
  admin_shell_sig="$trust_dir/plugins/shell.sig"
  admin_trust_root="$trust_dir/trust/quickstart-demo.pub"
  trust_root="$admin_trust_root"
  workflow_sig="$admin_workflow_sig"
  shell_sig="$admin_shell_sig"
  echo "NOTICE: using admin-generated signatures from $trust_dir" >&2
fi

for required in \
  "$transist_bin" \
  "$shell_plugin_dir/plugin.yaml" \
  "$workflow_path" \
  "$workflow_sig" \
  "$quickstart_config" \
  "$shell_sig" \
  "$trust_root"; do
  if [[ ! -e "$required" ]]; then
    echo "FAIL: required quickstart input is missing: $required" >&2
    exit 1
  fi
done

# Assert the shell plugin declares a local provider so the quickstart cannot
# make non-loopback connections under --local-only.
if ! grep -Eq 'default_provider_kind:[[:space:]]*local' "$shell_plugin_dir/plugin.yaml"; then
  echo "FAIL: $shell_plugin_dir/plugin.yaml must declare default_provider_kind: local" >&2
  exit 1
fi

# Assert the shipped inputs contain no private key material.
# Scan the full package directory in archive mode, or all quickstart/plugin
# inputs in source mode. Check both filenames and PEM content.
if [[ -n "$pkg_dir" ]]; then
  priv_found="$(find "$pkg_dir" -type f -name '*.key' 2>/dev/null || true)"
  priv_content="$(grep -rlE '-----BEGIN (AES|EC|OPENSSH|PGP|RSA|DSA|ECDSA|ENCRYPTED) PRIVATE KEY-----' "$pkg_dir" 2>/dev/null || true)"
else
  priv_found="$(find "$shell_plugin_dir" "$trust_root" "$workflow_path" "$workflow_sig" "$shell_sig" \
      -type f -name '*.key' 2>/dev/null || true)"
  priv_content="$(grep -rlE '-----BEGIN (AES|EC|OPENSSH|PGP|RSA|DSA|ECDSA|ENCRYPTED) PRIVATE KEY-----' \
      "$shell_plugin_dir" "$trust_root" "$workflow_path" "$workflow_sig" "$shell_sig" 2>/dev/null || true)"
fi
if [[ -n "$priv_found" || -n "$priv_content" ]]; then
  echo "FAIL: private key material present in the quickstart fixture" >&2
  [[ -n "$priv_found" ]] && echo "$priv_found" >&2
  [[ -n "$priv_content" ]] && echo "$priv_content" >&2
  exit 1
fi

work_root="$(mktemp -d -t transist-quickstart-XXXXXX)"
# macOS exposes its temporary root through /var -> /private/var. Keep every
# generated home rooted at the physical spelling required by descriptor opens.
work_root="$(cd "$work_root" && pwd -P)"
workspace="$work_root/project"
mkdir -p "$workspace"
# Canonicalize so the workspace capability grant matches the runtime's view.
workspace="$(cd "$workspace" && pwd -P)"

# When using admin-generated signatures, stage the plugin and workflow in
# the temp work_root with the admin sidecars so install commands find them.
if [[ -n "$trust_dir" ]]; then
  staged_plugins="$work_root/staged/plugins"
  staged_examples="$work_root/staged/examples/quickstart"
  mkdir -p "$staged_plugins" "$staged_examples"
  cp -R "$shell_plugin_dir" "$staged_plugins/shell"
  cp "$admin_shell_sig" "$staged_plugins/shell.sig"
  cp "$workflow_path" "$staged_examples/quickstart.workflow.yaml"
  cp "$admin_workflow_sig" "$staged_examples/quickstart.workflow.yaml.sig"
  cp "$quickstart_config" "$staged_examples/quickstart.config.yaml"
  shell_plugin_dir="$staged_plugins/shell"
  workflow_path="$staged_examples/quickstart.workflow.yaml"
  quickstart_config="$staged_examples/quickstart.config.yaml"
fi

# Package delivery is immutable. The runtime receives the package directory,
# not an authored workflow path or an operator-selected signature.
workflow_package_dir="$work_root/quickstart-package-v1"
mkdir -p "$workflow_package_dir"
cp "$workflow_path" "$workflow_package_dir/$(basename "$workflow_path")"
cp "$workflow_sig" "$workflow_package_dir/$(basename "$workflow_sig")"
inputs_file="$work_root/quickstart-inputs.json"
printf '{"subject":"%s"}\n' "$SUBJECT" > "$inputs_file"

runner_script="$work_root/run.sh"
fake_home="$work_root/fake-home"
mkdir -p "$fake_home"
cat > "$runner_script" <<RUNNER
set -euo pipefail
if [[ "\${TRANSIST_SMOKE_DEBUG:-0}" == "1" ]]; then
  set -x
fi
cd "$workspace"
TRANSIST="$transist_bin"
# Isolate the per-user default home resolution behind a synthetic \$HOME/
# \$XDG_CONFIG_HOME/\$XDG_STATE_HOME so the documented commands below run with
# no --home override, exactly as written in quickstart.md, while keeping the
# generated home out of the checkout and outside \$workspace.
export HOME="$fake_home"
export XDG_CONFIG_HOME="$fake_home/.config"
export XDG_STATE_HOME="$fake_home/.local/state"
unset TRANSIST_HOME
runtime_home="\$XDG_STATE_HOME/transist"
"\$TRANSIST" home init >/dev/null
# Install the documented governed policy. Substitute its workspace placeholder
# with the synthetic smoke workspace before loading the baseline grant set.
sed "s|__WORKSPACE__|$workspace|g" "$quickstart_config" > "\$runtime_home/config.yaml"
"\$TRANSIST" trust import "$trust_root" >/dev/null
"\$TRANSIST" plugins install --path "$shell_plugin_dir/plugin.yaml" --trusted-execution >/dev/null
"\$TRANSIST" workflows install --package "$workflow_package_dir" --approve >/dev/null
"\$TRANSIST" workflows check "$WORKFLOW_REF" --inputs-file "$inputs_file" \\
  --trusted-execution --local-only >/dev/null
if [[ "\${TRANSIST_SMOKE_DEBUG:-0}" == "1" ]]; then
  "\$TRANSIST" workflows authorize "$WORKFLOW_REF" --inputs-file "$inputs_file" --approve
else
  "\$TRANSIST" workflows authorize "$WORKFLOW_REF" --inputs-file "$inputs_file" \\
    --approve >/dev/null
fi
"\$TRANSIST" workflows check "$WORKFLOW_REF" --inputs-file "$inputs_file" \\
  --trusted-execution --local-only >/dev/null
"\$TRANSIST" workflows run "$WORKFLOW_REF" --run-ref "$RUN_ID" --trusted-execution --local-only \\
  --inputs-file "$inputs_file" --ui plain >/dev/null 2>&1 &
launch_pid=\$!
running=0
for i in \$(seq 1 30); do
  sleep 0.5
  st=\$("\$TRANSIST" runs status "$RUN_ID" 2>/dev/null | head -1 || true)
  if [[ "\$st" == *running* ]]; then
    running=1
    break
  fi
done
if [[ "\$running" -ne 1 ]]; then
  echo "FAIL: run never reached running state before cancel window" >&2
  "\$TRANSIST" runs status "$RUN_ID" >&2 || true
  "\$TRANSIST" runs show "$RUN_ID" >&2 || true
  wait \$launch_pid 2>/dev/null || true
  exit 1
fi
"\$TRANSIST" runs cancel "$RUN_ID" >/dev/null 2>&1 || true
wait \$launch_pid 2>/dev/null || true
canceled=0
for i in \$(seq 1 30); do
  st=\$("\$TRANSIST" runs status "$RUN_ID" 2>/dev/null | head -1 || true)
  if [[ "\$st" == *canceled* ]]; then
    canceled=1
    break
  fi
  sleep 0.5
done
if [[ "\$canceled" -ne 1 ]]; then
  echo "FAIL: run did not reach canceled state" >&2
  exit 1
fi
"\$TRANSIST" runs retry "$RUN_ID" --trusted-execution --local-only --ui plain >/dev/null 2>&1 &
retry_pid=\$!
# Wait for retry to re-enter the canceled run before checking terminal state.
reentered=0
for i in \$(seq 1 30); do
  sleep 0.5
  st=\$("\$TRANSIST" runs status "$RUN_ID" 2>/dev/null | head -1 || true)
  if [[ "\$st" == *running* || "\$st" == *success* ]]; then
    reentered=1
    break
  fi
done
if [[ "\$reentered" -ne 1 ]]; then
  echo "FAIL: retry never re-entered the canceled run" >&2
  wait \$retry_pid 2>/dev/null || true
  exit 1
fi
for i in \$(seq 1 60); do
  st=\$("\$TRANSIST" runs status "$RUN_ID" 2>/dev/null | head -1 || true)
  case "\$st" in
    *success*) break ;;
    *failed*|*canceled*)
      echo "FAIL: retry landed on terminal status: \$st" >&2
      wait \$retry_pid 2>/dev/null || true
      exit 1
      ;;
  esac
  sleep 0.5
done
wait \$retry_pid 2>/dev/null || true
st=\$("\$TRANSIST" runs status "$RUN_ID" 2>/dev/null | head -1 || true)
if [[ "\$st" != *success* ]]; then
  echo "FAIL: run did not reach success after retry; status=\$st" >&2
  exit 1
fi
# Inspect state, events, artifacts, evidence.
"\$TRANSIST" runs show "$RUN_ID" >/dev/null
"\$TRANSIST" runs tail "$RUN_ID" --limit 50 >/dev/null
evidence_dir="\$runtime_home/runs/run_${RUN_ID}/artifacts/write_report/execution_001/evidence"
summary_file=""
for f in "\$evidence_dir"/*summary.txt; do
  if [[ -f "\$f" ]]; then
    summary_file="\$f"
    break
  fi
done
if [[ -z "\$summary_file" ]]; then
  echo "FAIL: authored evidence summary.txt was not promoted" >&2
  exit 1
fi
# Verify evidence contents.
if ! grep -q 'subject=transist-demo' "\$summary_file" 2>/dev/null; then
  echo "FAIL: evidence summary does not contain expected subject" >&2
  exit 1
fi
if ! grep -q 'status=completed' "\$summary_file" 2>/dev/null; then
  echo "FAIL: evidence summary does not contain expected status" >&2
  exit 1
fi
# Export evidence and verify export output.
export_output="\$(\$TRANSIST runs evidence export "$RUN_ID" 2>&1)" || true
if ! echo "\$export_output" | grep -qi 'digest\|sha256\|signer\|key.id' 2>/dev/null; then
  echo "FAIL: evidence export does not mention digest or signer" >&2
  exit 1
fi
# Verify the run record carries governed/trusted/local-only posture and
# durable cancellation/retry history.
state_json="\$runtime_home/runs/run_${RUN_ID}/state.json"
events_jsonl="\$runtime_home/runs/run_${RUN_ID}/events.jsonl"
python3 - <<PY
import json, sys, pathlib
state = json.load(open("\$state_json"))
assert state["status"] == "success", state["status"]
assert state.get("local_only_mode") is True, "local_only_mode not recorded"
assert state.get("trusted_execution_mode") is True, "trusted_execution_mode not recorded"
assert state.get("policy_profile") == "allowlist", state.get("policy_profile")
assert state.get("capability_enforcement") == "enforced", state.get("capability_enforcement")
assert state.get("trust_decision", {}).get("decision") == "allowed", state.get("trust_decision")
# Durable retry history.
resume_count = state.get("resume_count", 0)
assert resume_count >= 1, f"resume_count={resume_count}, expected >= 1"
# The canceled step must have at least one execution record on disk.
# (The state.json executions field may not count a canceled dispatch.)
canceled_exec_dir = pathlib.Path(
    "\$runtime_home/runs/run_${RUN_ID}/artifacts/wait_for_cancel_window"
)
exec_records = list(canceled_exec_dir.glob("execution_*")) if canceled_exec_dir.exists() else []
assert len(exec_records) >= 1, \\
    f"no execution records found under {canceled_exec_dir}"
# Verify cancellation and resume events in the event log.
events_path = pathlib.Path("\$events_jsonl")
assert events_path.exists(), "events.jsonl not found"
event_types = set()
for line in events_path.read_text().splitlines():
    if not line.strip():
        continue
    try:
        event = json.loads(line)
        event_types.add(event.get("kind", ""))
    except json.JSONDecodeError:
        pass
assert any("stop" in et for et in event_types), \\
    f"no run-stopped event found in {event_types}"
assert any("resume" in et for et in event_types), \\
    f"no resume event found in {event_types}"
print("quickstart smoke: state and event assertions passed")
PY
RUNNER

# Host-level outbound network isolation. On Linux, `unshare -n` places the
# whole quickstart (runtime + plugin child) in a network namespace with no
# external routes; this composes cleanly with the runtime's bubblewrap plugin
# sandbox. On macOS the runtime itself sandboxes plugin processes with
# sandbox-exec; nesting an outer sandbox-exec breaks the APP handshake, so the
# host-level boundary on macOS is the runtime's own plugin sandbox, which under
# --local-only emits `(deny network-outbound)` except loopback. The smoke
# therefore uses `unshare -n` on Linux and relies on the runtime plugin sandbox
# plus --local-only on macOS, and asserts the run recorded local-only mode.
# When --require-isolation is set, the smoke fails closed if no isolation
# mechanism is available instead of running unisolated.
run_isolated() {
  local script="$1"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    if [[ "$require_isolation" -eq 1 ]] && [[ ! -x /usr/bin/sandbox-exec ]]; then
      echo "FAIL: --require-isolation set but /usr/bin/sandbox-exec is unavailable" >&2
      echo "      macOS host-level isolation requires the runtime's sandbox-exec backend" >&2
      return 1
    fi
    echo "NOTICE: macOS host-level isolation is the runtime's sandbox-exec plugin" >&2
    echo "        sandbox (network: loopback denies non-loopback outbound); an outer" >&2
    echo "        sandbox-exec would conflict with the runtime's plugin sandbox." >&2
    bash "$script"
    return $?
  elif command -v unshare >/dev/null 2>&1 && unshare -n true 2>/dev/null; then
    unshare -n bash "$script"
    return $?
  else
    if [[ "$require_isolation" -eq 1 ]]; then
      echo "FAIL: --require-isolation set but unshare -n is unavailable" >&2
      echo "      Linux host-level isolation requires unshare -n (CAP_SYS_ADMIN)" >&2
      return 1
    fi
    echo "NOTICE: host-level network namespace isolation is unavailable on this" >&2
    echo "        host; the runtime --local-only boundary and the shell plugin's" >&2
    echo "        declared local provider_kind remain enforced." >&2
    bash "$script"
    return $?
  fi
}

echo "============================================================"
echo "  Leg 1 — Governed/trusted/signed walkthrough"
if [[ -n "$archive_path" ]]; then
  echo "  archive: $archive_path"
else
  echo "  binary:  $transist_bin"
fi
echo "  workspace: $workspace"
echo "============================================================"

if run_isolated "$runner_script"; then
  echo "============================================================"
  echo "  Leg 1 (governed/trusted) passed."
  echo "============================================================"
else
  rc=$?
  echo "============================================================"
  echo "  Leg 1 (governed/trusted) FAILED (exit $rc)."
  echo "  Inspect the generated home before cleanup is not possible (trap"
  echo "  removes it). Re-run with a non-isolated shell to debug."
  echo "============================================================"
  exit "$rc"
fi

# ---------------------------------------------------------------------------
# Leg 2: Standalone quickstart
# Contract: docs/public/guides/getting-started/quickstart.md
# ---------------------------------------------------------------------------

STANDALONE_RUN_ID="qs-standalone-smoke"
STANDALONE_SUBJECT="standalone-smoke"

standalone_script="$work_root/run_standalone.sh"
cat > "$standalone_script" <<STANDALONE_RUNNER
set -euo pipefail
TRANSIST="$transist_bin"

# Create an isolated home via TRANSIST_HOME env (cleanup through trap).
standalone_home="\$(mktemp -d -t transist-standalone-smoke-XXXXXX)"
standalone_home="\$(cd "\$standalone_home" && pwd -P)"
trap 'rm -rf "\$standalone_home"' EXIT
export TRANSIST_HOME="\$standalone_home"

# 1. Initialize a standalone home at the isolated path.
"\$TRANSIST" home init --mode standalone --home "\$standalone_home" >/dev/null

# 2. Install the shell plugin unsigned.
"\$TRANSIST" plugins install --path "$shell_plugin_dir/plugin.yaml" >/dev/null

# 3. Install the quickstart workflow unsigned, then check prospective inputs.
"\$TRANSIST" workflows install --path "$workflow_path" --approve >/dev/null
standalone_inputs="\$standalone_home/inputs.json"
printf '{"subject":"%s"}\n' "$STANDALONE_SUBJECT" > "\$standalone_inputs"
"\$TRANSIST" workflows check "$WORKFLOW_REF" --inputs-file "\$standalone_inputs" >/dev/null

# 4. Run and wait for success.
"\$TRANSIST" workflows run "$WORKFLOW_REF" --run-ref "$STANDALONE_RUN_ID" \
  --inputs-file "\$standalone_inputs" --ui plain >/dev/null 2>&1 &
launch_pid=\$!

success=0
for i in \$(seq 1 60); do
  sleep 0.5
  st=\$("\$TRANSIST" runs status "$STANDALONE_RUN_ID" 2>/dev/null | head -1 || true)
  case "\$st" in
    *success*) success=1; break ;;
    *failed*|*canceled*)
      echo "FAIL: standalone run reached terminal status: \$st" >&2
      wait \$launch_pid 2>/dev/null || true
      exit 1
      ;;
  esac
done
wait \$launch_pid 2>/dev/null || true

if [[ "\$success" -ne 1 ]]; then
  echo "FAIL: standalone run did not reach success within timeout" >&2
  "\$TRANSIST" runs status "$STANDALONE_RUN_ID" >&2 || true
  exit 1
fi

# 5. Inspect run record.
"\$TRANSIST" runs show "$STANDALONE_RUN_ID" >/dev/null
"\$TRANSIST" runs tail "$STANDALONE_RUN_ID" --limit 50 >/dev/null

# 6. Verify run state: success, not trusted, not local-only.
state_json="\$standalone_home/runs/run_${STANDALONE_RUN_ID}/state.json"
python3 - <<PY
import json, sys
state = json.load(open("\$state_json"))
assert state["status"] == "success", "status=" + state["status"]
assert not state.get("trusted_execution_mode", False), \
    "trusted_execution_mode should be false in standalone mode"
print("standalone smoke: state assertions passed")
PY

# 7. Verify evidence was written.
evidence_dir="\$standalone_home/runs/run_${STANDALONE_RUN_ID}/artifacts/write_report/execution_001/evidence"
summary_file=""
for f in "\$evidence_dir"/*summary.txt; do
  if [[ -f "\$f" ]]; then
    summary_file="\$f"
    break
  fi
done
if [[ -z "\$summary_file" ]]; then
  echo "FAIL: standalone quickstart evidence summary.txt was not promoted" >&2
  exit 1
fi
if ! grep -q 'subject=$STANDALONE_SUBJECT' "\$summary_file" 2>/dev/null; then
  echo "FAIL: evidence summary does not contain expected subject" >&2
  exit 1
fi
STANDALONE_RUNNER

echo "============================================================"
echo "  Leg 2 — Standalone quickstart"
echo "  binary:  $transist_bin"
echo "============================================================"

# The standalone leg does not use --local-only so no network-namespace
# isolation is strictly required; the plugin runs a local process with no
# network calls. Run through run_isolated for consistency so both legs
# receive the same host-level treatment on both runners.
if run_isolated "$standalone_script"; then
  echo "============================================================"
  echo "  Leg 2 (standalone) passed."
  echo "  Both smoke legs passed."
  echo "============================================================"
else
  rc=$?
  echo "============================================================"
  echo "  Leg 2 (standalone) FAILED (exit $rc)."
  echo "  Re-run with a non-isolated shell to debug."
  echo "============================================================"
  exit "$rc"
fi

# Source-checkout tutorial and recovery regressions use the same runtime binary.
# Recovery fixtures also require its sibling transist-admin for disposable keys.
# These tests are not part of the minimal release archive.
if [[ -z "$archive_path" && -d "$REPO_ROOT/examples/authoring" ]]; then
  python3 "$REPO_ROOT/scripts/ci/test_authoring_tutorials.py" --transist-bin "$transist_bin"
  python3 "$REPO_ROOT/scripts/ci/test_cleanup_reentry.py" \
    --transist-bin "$transist_bin" --admin-bin "$(dirname "$transist_bin")/transist-admin"
fi
