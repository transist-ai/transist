#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: filesystem-policy
# Hermetic E2E — workflow installation, preflight, sandbox enforcement, custom-mode rejection.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=filesystem-policy
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
SUITE_ROOT="$TRANSIST_HOME"
export WORKSPACE_DIR="$(mktemp -d -t ".transist-${SUITE_ID}-workspace-XXXXXX")"
export WORKSPACE_DIR="$(cd "$WORKSPACE_DIR" && pwd -P)"
# Home-relative grants resolve against HOME, independently of TRANSIST_HOME.
# Use an isolated sibling operator home, never the operator's actual files.
OPS_HOME="$(mktemp -d -t transist-fs-policy-ops-XXXXXX)"
export HOME="$(cd "$OPS_HOME" && pwd -P)"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$HOME/.ssh"
printf '%s\n' '[user]' 'name = filesystem-policy e2e' 'email = filesystem-policy@example.invalid' >"$HOME/.gitconfig"
printf '%s\n' 'ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC3nT filesystem-policy-e2e' >"$HOME/.ssh/id-test.pub"

cleanup() {
  rm -rf "$OPS_HOME" 2>/dev/null || true
  rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
  rm -rf "$TRANSIST_HOME" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

if [ -x /usr/bin/sandbox-exec ] || command -v bwrap >/dev/null 2>&1; then
  export HAVE_FS_SANDBOX=1
else
  export HAVE_FS_SANDBOX=0
fi

# Step 1 -- Boot and compile
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
cp "$REPO_ROOT/workflows/demos/getting_started/11_filesystem_policy/settings.example.yaml" \
  "$TRANSIST_HOME/settings.yaml"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check --path "$REPO_ROOT/workflows/demos/getting_started/11_filesystem_policy/workflow.yaml"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install \
  --path "$REPO_ROOT/workflows/demos/getting_started/11_filesystem_policy/workflow.yaml" \
  --approve
inputs_file="$SUITE_ROOT/inputs.json"
printf '{"repo_path":"%s"}\n' "$WORKSPACE_DIR" > "$inputs_file"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check \
  demo_11_filesystem_policy@0.1.0 --inputs-file "$inputs_file" >/dev/null

recipe_path="$TRANSIST_HOME/registry/recipes/demo_11_filesystem_policy-0.1.0.yaml"
[ -f "$recipe_path" ] && echo "OK: filesystem-policy recipe exists" || { echo "FAIL: missing filesystem-policy recipe"; exit 1; }
grep -q 'filesystem_policy:' "$recipe_path" && echo "OK: recipe persisted filesystem_policy" || { echo "FAIL: recipe missing filesystem_policy"; exit 1; }
grep -q 'mode: read_only' "$recipe_path" && echo "OK: recipe persisted read_only step policy" || { echo "FAIL: recipe missing read_only step policy"; exit 1; }
! grep -q 'mode: custom' "$recipe_path" && echo "OK: recipe has no custom mode step" || { echo "FAIL: recipe should not carry custom mode"; exit 1; }

# Step 2 -- Custom mode is rejected at compile time
custom_flow="$SUITE_ROOT/custom-mode.yaml"
cat > "$custom_flow" <<'YAML'
schema: transist.flow.v1
id: custom_mode_denied
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: probe
    uses: process:shell/exec
    filesystem_policy:
      workspace_access:
        mode: custom
    with:
      command: sh
      args: ["-c", "true"]
YAML

if $TRANSIST_BIN --home "$TRANSIST_HOME" workflows check --path "$custom_flow" >"$SUITE_ROOT/custom-validate.log" 2>&1; then
  echo "FAIL: custom mode workflow check succeeded (should be rejected)"
  exit 1
else
  echo "OK: custom mode workflow rejected"
fi
grep -q 'filesystem_custom_mode_unsupported' "$SUITE_ROOT/custom-validate.log" && echo "OK: rejection carried filesystem_custom_mode_unsupported reason code" || { echo "FAIL: rejection missing filesystem_custom_mode_unsupported reason code"; cat "$SUITE_ROOT/custom-validate.log"; exit 1; }
rm -f "$custom_flow" "$SUITE_ROOT/custom-validate.log"

# Step 3 -- Prepare and preflight
run_id="fs-policy-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows prepare demo_11_filesystem_policy@0.1.0 --run-ref "$run_id" --inputs-file "$inputs_file"
preflight_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs preflight "$run_id")"
show_before_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$run_id")"

printf '%s' "$preflight_json" | jq -e '.status and (.checks | length > 0)' >/dev/null && echo "OK: preflight returned a usable report" || { echo "FAIL: preflight report shape mismatch"; exit 1; }
printf '%s' "$show_before_json" | jq -e --arg run_id "$run_id" '.run_id == $run_id' >/dev/null && echo "OK: runs show returned prepared run state" || { echo "FAIL: runs show missing prepared run state"; exit 1; }

# Step 4 -- Backend-specific assertions
if [ "$HAVE_FS_SANDBOX" = "1" ]; then
  $TRANSIST_BIN --home "$TRANSIST_HOME" runs start "$run_id"
  state_path="$TRANSIST_HOME/runs/run_${run_id}/state.json"
  for _ in $(seq 1 40); do
    run_status="$(jq -r '.status // ""' "$state_path" 2>/dev/null || true)"
    [ "$run_status" = "success" ] && break
    sleep 1
  done

  jq -e '.status == "success"' "$state_path" >/dev/null && echo "OK: strict demo run succeeded" || { echo "FAIL: strict demo run did not succeed"; jq '{status,steps,preflight}' "$state_path"; exit 1; }
  jq -e '.steps.full_access_probe.status == "succeeded" and .steps.read_only_probe.status == "succeeded" and .steps.verify_workspace_intact.status == "succeeded"' "$state_path" >/dev/null && echo "OK: all filesystem policy demo steps succeeded" || { echo "FAIL: one or more demo steps failed"; exit 1; }
  jq -e '.steps.full_access_probe.last_request.policy.filesystem.source == "global" and .steps.full_access_probe.last_request.policy.filesystem.workspace_access.mode == "full"' "$state_path" >/dev/null && echo "OK: full probe inherited the global default policy" || { echo "FAIL: full probe missing global default policy"; exit 1; }
  jq -e '.steps.read_only_probe.last_request.policy.filesystem.source == "step" and .steps.read_only_probe.last_request.policy.filesystem.workspace_access.mode == "read_only"' "$state_path" >/dev/null && echo "OK: read_only probe persisted the step override" || { echo "FAIL: read_only probe missing step override policy"; exit 1; }
  jq -e --arg workspace "$WORKSPACE_DIR" '.workspace.path == $workspace and .workspace.access == "full"' "$state_path" >/dev/null && echo "OK: declared workspace persisted independently of CLI CWD" || { echo "FAIL: declared workspace missing from state"; exit 1; }
  jq -e '.steps.full_access_probe.last_result.output.workspace_readable == true and .steps.full_access_probe.last_result.output.workspace_mutations_allowed == true and .steps.full_access_probe.last_result.output.outside_write_denied == true' "$state_path" >/dev/null && echo "OK: full probe proved workspace access plus outside-workspace write denial" || { echo "FAIL: full probe output missing expected assertions"; exit 1; }
  jq -e '.steps.read_only_probe.last_result.output.workspace_read_allowed == true and .steps.read_only_probe.last_result.output.workspace_write_denied == true' "$state_path" >/dev/null && echo "OK: read_only probe proved workspace mutation denial" || { echo "FAIL: read_only probe output missing expected assertions"; exit 1; }
  jq -e '.steps.verify_workspace_intact.last_result.status == "success"' "$state_path" >/dev/null && echo "OK: verification step recorded success" || { echo "FAIL: verification step missing success result"; exit 1; }
  if [[ "$(uname -s)" == Linux ]]; then
    # Linux must expose only granted home children, never bind the whole home.
    mkdir -p "$HOME/private-runtime" "$HOME/approved-output"
    printf 'private fixture state\n' > "$HOME/private-runtime/state.json"
    cat > "$SUITE_ROOT/home-privacy.yaml" <<'YAML'
schema: transist.flow.v1
id: home_privacy
version: v1
plugins: {shell: 0.1.0}
inputs:
  repo_path: {type: string, required: true}
workspace: {path: "{{inputs.repo_path}}", access: full}
steps:
  - id: probe
    uses: process:shell/exec
    filesystem_policy:
      write_allow_paths: [~/approved-output]
    with:
      command: sh
      args:
        - -eu
        - -c
        - |
          test -f "$HOME/.gitconfig"
          printf 'authorized write\n' > "$HOME/approved-output/result.txt"
          if cat "$HOME/private-runtime/state.json" >/dev/null 2>&1; then exit 91; fi
          if touch "$HOME/ungranted-write" 2>/dev/null; then exit 92; fi
YAML
    $TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$SUITE_ROOT/home-privacy.yaml" --approve >/dev/null
    $TRANSIST_BIN --home "$TRANSIST_HOME" workflows prepare home_privacy@v1 --run-ref home-privacy --input "repo_path=$WORKSPACE_DIR" >/dev/null
    $TRANSIST_BIN --home "$TRANSIST_HOME" runs start home-privacy >/dev/null
    for _ in $(seq 1 40); do
      status="$(jq -r '.status' "$TRANSIST_HOME/runs/run_home-privacy/state.json")"
      [[ "$status" != success && "$status" != failed ]] || break
      sleep 0.2
    done
    [[ "$status" == success ]] || { echo "FAIL: home-relative grant reopened private home bytes or writes"; exit 1; }
    [[ "$(cat "$HOME/approved-output/result.txt")" == 'authorized write' ]] || { echo "FAIL: exact home write grant was not honored"; exit 1; }
    echo "OK: exact home grant exposed no ungranted private bytes or parent writes"
  fi
else
  echo "SKIP: no local strict sandbox backend available"
fi

echo "PASS: $SUITE_ID"
