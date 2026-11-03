# Filesystem Policy E2E Test Plan

## Goal

Exercise workflow installation, preflight, workspace-scoped local sandbox enforcement, custom-mode rejection, and persisted execution inspection for the bundled filesystem-policy demo with per-step shell assertions.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite filesystem-policy`.

## Not Covered

- Central inspector or docker.
- Network access.
- Prompt-file resolver denial.
- Hosts without a supported local sandbox backend; the suite exits with an explicit skip.

## Environment Variables

| Variable | Purpose | Lifecycle | Sensitive |
|---|---|---|---|
| `TRANSIST_BIN` | CLI binary | Boot | No |
| `TRANSIST_HOME` | ephemeral home | Boot | No |
| `SUITE_ID` | suite label | Boot | No |
| `HAVE_FS_SANDBOX` | local sandbox backend detection | Boot | No |
| `WORKSPACE_DIR` | declared demo workspace outside the Transist home | Boot | No |

## Boot-time Setup

```bash
if [ -n "${TRANSIST_BIN:-}" ]; then
  :
elif [ -x "target/debug/transist" ]; then
  export TRANSIST_BIN="$PWD/target/debug/transist"
elif cargo build -p transist-cli >/dev/null 2>&1 && [ -x "target/debug/transist" ]; then
  export TRANSIST_BIN="$PWD/target/debug/transist"
else
  echo "FAIL: unable to resolve TRANSIST_BIN"
  exit 1
fi

export SUITE_ID=filesystem-policy
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"
export WORKSPACE_DIR="/tmp/.transist-${SUITE_ID}-workspace-$(date +%s)"
mkdir -p "$WORKSPACE_DIR"

if [ -x /usr/bin/sandbox-exec ] || command -v bwrap >/dev/null 2>&1; then
  export HAVE_FS_SANDBOX=1
else
  export HAVE_FS_SANDBOX=0
fi

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `workflows/demos/getting_started/11_filesystem_policy/workflow.yaml` exists.

## Execution

### Step 1 -- Boot and compile

```bash
$TRANSIST_BIN --home "$TRANSIST_HOME" home init
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml
mkdir -p "$TRANSIST_HOME"
cat > "$TRANSIST_HOME/settings.yaml" <<'YAML'
filesystem_policy:
  workspace_access:
    mode: full
  read_allow_paths:
    - /etc/gitconfig
    - ~/.gitconfig
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check --path workflows/demos/getting_started/11_filesystem_policy/workflow.yaml
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install \
  --path workflows/demos/getting_started/11_filesystem_policy/workflow.yaml --approve
printf '{"repo_path":"%s"}\n' "$WORKSPACE_DIR" > "$SUITE_ROOT/inputs.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check \
  demo_11_filesystem_policy@0.1.0 --inputs-file "$SUITE_ROOT/inputs.json"
```

```bash
set -euo pipefail
recipe_path="$TRANSIST_HOME/registry/recipes/demo_11_filesystem_policy-0.1.0.yaml"
[ -f "$recipe_path" ] && echo "OK: filesystem-policy recipe exists" || { echo "FAIL: missing filesystem-policy recipe"; exit 1; }
grep -q 'filesystem_policy:' "$recipe_path" && echo "OK: recipe persisted filesystem_policy" || { echo "FAIL: recipe missing filesystem_policy"; exit 1; }
grep -q 'mode: read_only' "$recipe_path" && echo "OK: recipe persisted read_only step policy" || { echo "FAIL: recipe missing read_only step policy"; exit 1; }
! grep -q 'mode: custom' "$recipe_path" && echo "OK: recipe has no custom mode step" || { echo "FAIL: recipe should not carry custom mode"; exit 1; }
```

### Step 2 -- Custom mode is rejected at compile time

```bash
cat > /tmp/custom-mode-$SUITE_ID.yaml <<'YAML'
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
        read_allow_paths: [src/**]
    with:
      command: sh
      args: ["-c", "true"]
YAML

if $TRANSIST_BIN --home "$TRANSIST_HOME" workflows check --path /tmp/custom-mode-$SUITE_ID.yaml >/tmp/custom-validate-$SUITE_ID.log 2>&1; then
  echo "FAIL: custom mode workflow check succeeded (should be rejected)"
  exit 1
else
  echo "OK: custom mode workflow rejected"
fi
grep -q 'filesystem_custom_mode_unsupported' /tmp/custom-validate-$SUITE_ID.log && echo "OK: rejection carried filesystem_custom_mode_unsupported reason code" || { echo "FAIL: rejection missing filesystem_custom_mode_unsupported reason code"; cat /tmp/custom-validate-$SUITE_ID.log; exit 1; }
rm -f /tmp/custom-mode-$SUITE_ID.yaml /tmp/custom-validate-$SUITE_ID.log
```

### Step 3 -- Prepare and preflight

```bash
run_id="fs-policy-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows prepare demo_11_filesystem_policy@0.1.0 --run-ref "$run_id" --input repo_path="$WORKSPACE_DIR"
preflight_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs preflight "$run_id")"
show_before_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$run_id")"
```

```bash
set -euo pipefail
printf '%s' "$preflight_json" | jq -e '.status and (.checks | length > 0)' >/dev/null && echo "OK: preflight returned a usable report" || { echo "FAIL: preflight report shape mismatch"; exit 1; }
printf '%s' "$show_before_json" | jq -e --arg run_id "$run_id" '.run_id == $run_id' >/dev/null && echo "OK: runs show returned prepared run state" || { echo "FAIL: runs show missing prepared run state"; exit 1; }
```

### Step 4 -- Backend-specific assertions

```bash
if [ "$HAVE_FS_SANDBOX" = "1" ]; then
  $TRANSIST_BIN --home "$TRANSIST_HOME" runs start "$run_id"
  state_path="$TRANSIST_HOME/runs/run_${run_id}/state.json"
  for _ in $(seq 1 40); do
    run_status="$(jq -r '.status // ""' "$state_path" 2>/dev/null || true)"
    [ "$run_status" = "success" ] && break
    sleep 1
  done
else
  echo "SKIP: no local strict sandbox backend available"
fi
```

```bash
set -euo pipefail
if [ "$HAVE_FS_SANDBOX" = "1" ]; then
  jq -e '.status == "success"' "$state_path" >/dev/null && echo "OK: strict demo run succeeded" || { echo "FAIL: strict demo run did not succeed"; exit 1; }
  jq -e '.steps.full_access_probe.status == "succeeded" and .steps.read_only_probe.status == "succeeded" and .steps.verify_workspace_intact.status == "succeeded"' "$state_path" >/dev/null && echo "OK: all filesystem policy demo steps succeeded" || { echo "FAIL: one or more demo steps failed"; exit 1; }
  jq -e '.steps.full_access_probe.last_request.policy.filesystem.source == "global" and .steps.full_access_probe.last_request.policy.filesystem.workspace_access.mode == "full"' "$state_path" >/dev/null && echo "OK: full probe inherited the global default policy" || { echo "FAIL: full probe missing global default policy"; exit 1; }
  jq -e '.steps.read_only_probe.last_request.policy.filesystem.source == "step" and .steps.read_only_probe.last_request.policy.filesystem.workspace_access.mode == "read_only"' "$state_path" >/dev/null && echo "OK: read_only probe persisted the step override" || { echo "FAIL: read_only probe missing step override policy"; exit 1; }
  jq -e --arg workspace "$WORKSPACE_DIR" '.workspace.path == $workspace and .workspace.access == "full"' "$state_path" >/dev/null && echo "OK: declared workspace was persisted" || { echo "FAIL: declared workspace missing from state"; exit 1; }
  jq -e '.steps.full_access_probe.last_result.output.workspace_readable == true and .steps.full_access_probe.last_result.output.workspace_mutations_allowed == true and .steps.full_access_probe.last_result.output.outside_write_denied == true' "$state_path" >/dev/null && echo "OK: full probe proved workspace access plus outside-workspace write denial" || { echo "FAIL: full probe output missing expected assertions"; exit 1; }
  jq -e '.steps.read_only_probe.last_result.output.workspace_read_allowed == true and .steps.read_only_probe.last_result.output.workspace_write_denied == true' "$state_path" >/dev/null && echo "OK: read_only probe proved workspace mutation denial" || { echo "FAIL: read_only probe output missing expected assertions"; exit 1; }
  jq -e '.steps.verify_workspace_intact.last_result.status == "success"' "$state_path" >/dev/null && echo "OK: verification step recorded success" || { echo "FAIL: verification step missing success result"; exit 1; }
else
  echo "OK: suite skipped without sandbox backend"
fi
```

### Step 5 -- Cleanup

```bash
HOME_TO_DELETE="$TRANSIST_HOME"
rm -rf "$HOME_TO_DELETE" 2>/dev/null || true
rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
unset TRANSIST_BIN TRANSIST_HOME SUITE_ID HAVE_FS_SANDBOX WORKSPACE_DIR
```

```bash
set -euo pipefail
leaked=""
for v in TRANSIST_BIN TRANSIST_HOME SUITE_ID HAVE_FS_SANDBOX WORKSPACE_DIR; do
  if [ -n "${!v:-}" ]; then leaked="$leaked $v"; fi
done
[ -z "$leaked" ] && echo "OK: suite-scoped shell env clean" || { echo "FAIL: suite env still set:$leaked"; env | grep -E "^(TRANSIST_BIN|TRANSIST_HOME|SUITE_ID|HAVE_FS_SANDBOX)="; exit 1; }
[ ! -d "$HOME_TO_DELETE" ] && echo "OK: TRANSIST_HOME removed" || echo "WARN: TRANSIST_HOME still present"
```

## Pass Criteria

- The demo workflow validates and compiles with a required `repo_path` that is
  persisted as its declared `workspace` rather than inferred from CWD.
- The compiled recipe exposes the `workspace_access` policy shape with `full` and `read_only` modes only.
- A workflow using `workspace_access.mode: custom` is rejected at `workflow check` with the `filesystem_custom_mode_unsupported` reason code.
- Hosts with a supported local sandbox backend complete the demo run successfully and prove workspace access plus outside-workspace denial in `full` and mutation denial in `read_only`.
- Hosts without a supported backend exit with an explicit skip.

Last verified:

- transist @ main
