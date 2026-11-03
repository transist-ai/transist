# Secrets And Policy E2E Test Plan

## Goal

Exercise secret delivery and non-persistence with per-step shell assertions.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite secrets-and-policy`.

## Not Covered

- Central inspector or docker.
- Network access.
- Manual gate decisions.

## Environment Variables

| Variable | Purpose | Lifecycle | Sensitive |
|---|---|---|---|
| `TRANSIST_BIN` | CLI binary | Boot | No |
| `TRANSIST_HOME` | ephemeral home | Boot | No |
| `SUITE_ID` | suite label | Boot | No |
| `MY_API_TOKEN` | secret source value | Boot/Step 1-5 | Yes |
| `SECRET_VALUE` | shell-local copy for assertions | Step 1-5 | Yes |

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

export SUITE_ID=secrets-and-policy
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `workflows/demos/getting_started/10_secret_env/workflow.yaml` exists.

## Execution

### Step 1 -- Boot and compile

```bash
export MY_API_TOKEN="local-test-secret-$(date +%s)-$RANDOM"
export SECRET_VALUE="$MY_API_TOKEN"
$TRANSIST_BIN --home "$TRANSIST_HOME" home init
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path workflows/demos/getting_started/10_secret_env/workflow.yaml --approve
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/registry/recipes/demo_10_secret_env-0.1.0.yaml" ] && echo "OK: secret demo compiled" || { echo "FAIL: missing secret recipe"; exit 1; }
```

### Step 2 -- Happy path delivery

```bash
secret_run_id="secret-ok-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$secret_run_id"
```

```bash
set -euo pipefail
for _ in $(seq 1 40); do
  secret_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${secret_run_id}/state.json" 2>/dev/null || true)"
  [ "$secret_state" = "success" ] && break
  sleep 1
done
jq -e '.status == "success"' "$TRANSIST_HOME/runs/run_${secret_run_id}/state.json" >/dev/null && echo "OK: secret run succeeded" || { echo "FAIL: secret run did not succeed"; exit 1; }
```

### Step 3 -- Non-persistence

```bash
set +e
grep -r "$SECRET_VALUE" "$TRANSIST_HOME/" >/tmp/secrets-hit.out 2>&1
secret_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$secret_rc" -eq 1 ] && echo "OK: secret value not found on disk" || { echo "FAIL: secret value leaked"; exit 1; }
prefix="${SECRET_VALUE:0:8}"
set +e
grep -r "$prefix" "$TRANSIST_HOME/" >/tmp/secrets-prefix.out 2>&1
prefix_rc=$?
set -e
[ "$prefix_rc" -eq 1 ] && echo "OK: secret prefix not found on disk" || echo "WARN: secret prefix appeared"
```

### Step 4 -- Missing-secret negative

```bash
unset MY_API_TOKEN
missing_run_id="secret-missing-$(now_ms)"
set +e
missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$missing_run_id" 2>&1)"
missing_rc=$?
set -e
export MY_API_TOKEN="$SECRET_VALUE"
```

```bash
set -euo pipefail
[ "$missing_rc" -ne 0 ] && echo "OK: missing secret run failed fast (nonzero exit)" || { echo "FAIL: missing secret run unexpectedly succeeded"; exit 1; }
for _ in $(seq 1 40); do
  missing_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${missing_run_id}/state.json" 2>/dev/null || true)"
  [ "$missing_state" = "failed" ] && break
  sleep 1
done
[ "$missing_state" = "failed" ] && echo "OK: missing secret run failed before dispatch" || { echo "FAIL: missing secret run did not fail"; exit 1; }
jq -e '.steps.receive_secret.status == "pending"' "$TRANSIST_HOME/runs/run_${missing_run_id}/state.json" >/dev/null && echo "OK: step never dispatched without secret" || { echo "FAIL: step status mismatch for missing secret"; exit 1; }
```

### Step 5 -- Process policy denial

```bash
cat >"$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
YAML
denied_run_id="secret-process-denied-$(now_ms)"
set +e
denied_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$denied_run_id" 2>&1)"
denied_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$denied_rc" -ne 0 ] && echo "OK: process-policy denial run failed fast (nonzero exit)" || { echo "FAIL: process-policy denial run unexpectedly succeeded"; exit 1; }
for _ in $(seq 1 40); do
  denied_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${denied_run_id}/state.json" 2>/dev/null || true)"
  [ "$denied_state" = "failed" ] && break
  sleep 1
done
[ "$denied_state" = "failed" ] && echo "OK: process secret delivery denial failed the run" || { echo "FAIL: process policy denial run did not fail"; exit 1; }
jq -e '.preflight.status == "failed"' "$TRANSIST_HOME/runs/run_${denied_run_id}/state.json" >/dev/null && echo "OK: preflight failed" || { echo "FAIL: expected preflight failure"; exit 1; }
jq -e '.preflight.checks[] | select(.kind == "remote_secret_policy" and .status == "failed")' "$TRANSIST_HOME/runs/run_${denied_run_id}/state.json" >/dev/null && echo "OK: remote_secret_policy denial recorded" || { echo "FAIL: expected remote_secret_policy preflight failure"; exit 1; }
jq -e '.steps.receive_secret.status == "pending"' "$TRANSIST_HOME/runs/run_${denied_run_id}/state.json" >/dev/null && echo "OK: step never dispatched under process secret denial" || { echo "FAIL: step status mismatch for process policy denial"; exit 1; }
```

### Step 6 -- Child environment isolation

```bash
export TRANSIST_STEP2_UNRELATED_ENV="should-not-leak"
cat >"$TRANSIST_HOME/child-env-check.yaml" <<'YAML'
schema: transist.flow.v1
id: child_env_check
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: inspect_env
    uses: process:shell/exec
    with:
      command: sh
      args:
        - -c
        - |
          printf '%s|%s|%s' "${PATH:+present}" "${TRANSIST_MANAGED_RUNTIME_AUTH_ACTIVE:-}" "${TRANSIST_STEP2_UNRELATED_ENV:+present}"
YAML
cat >"$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  child_env:
    inherit: false
    allowlist: ["PATH"]
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/child-env-check.yaml" --approve
child_env_run_id="child-env-check-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run child_env_check@v1 --run-ref "$child_env_run_id"
```

```bash
set -euo pipefail
for _ in $(seq 1 40); do
  child_env_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${child_env_run_id}/state.json" 2>/dev/null || true)"
  [ "$child_env_state" = "success" ] && break
  sleep 1
done
[ "$child_env_state" = "success" ] && echo "OK: child-env check run succeeded" || { echo "FAIL: child-env check run did not succeed"; exit 1; }
jq -e '.steps.inspect_env.last_result.output.stdout == "present|1|"' "$TRANSIST_HOME/runs/run_${child_env_run_id}/state.json" >/dev/null && echo "OK: unrelated parent env var was not inherited" || { echo "FAIL: child env isolation output mismatch"; exit 1; }
unset TRANSIST_STEP2_UNRELATED_ENV
```

### Step 7 -- Authoring validation

```bash
cat >"$TRANSIST_HOME/invalid-secret-ref.yaml" <<'YAML'
schema: transist.flow.v1
id: invalid_secret_ref
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: bad_secret_ref
    uses: process:shell/echo
    with:
      args: ["hello"]
      secret_refs: ["MY_API_TOKEN"]
YAML
set +e
invalid_secret_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/invalid-secret-ref.yaml" --approve 2>&1)"
invalid_secret_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$invalid_secret_rc" -ne 0 ] && echo "OK: compile rejected undeclared secret ref" || { echo "FAIL: expected compile failure for undeclared secret ref"; exit 1; }
printf '%s' "$invalid_secret_out" | grep -q "undeclared secret ref" && echo "OK: undeclared secret ref message surfaced" || { echo "FAIL: expected undeclared secret ref message"; exit 1; }
```

```bash
cat >"$TRANSIST_HOME/missing-required-secret.yaml" <<'YAML'
schema: transist.flow.v1
id: missing_required_secret
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: missing_secret
    uses: process:shell/exec_with_secret_env
    with:
      command: sh
      args: ["-c", "printf ok"]
YAML
set +e
missing_required_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/missing-required-secret.yaml" --approve 2>&1)"
missing_required_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$missing_required_rc" -ne 0 ] && echo "OK: compile rejected missing required secret ref" || { echo "FAIL: expected compile failure for missing required secret ref"; exit 1; }
printf '%s' "$missing_required_out" | grep -q "missing required secret ref" && echo "OK: missing required secret ref message surfaced" || { echo "FAIL: expected missing required secret ref message"; exit 1; }
```

### Step 8 -- Cleanup

```bash
unset MY_API_TOKEN SECRET_VALUE
rm -rf "$TRANSIST_HOME"
unset TRANSIST_BIN TRANSIST_HOME SUITE_ID
```

```bash
set -euo pipefail
env | grep -qE '^(TRANSIST_HOME|SUITE_ID|MY_API_TOKEN|SECRET_VALUE)(=|$)' && { echo "FAIL: suite env still set"; env | grep -E '^(TRANSIST_HOME|SUITE_ID|MY_API_TOKEN|SECRET_VALUE)(=|$)'; exit 1; } || echo "OK: shell env clean"
[ ! -d "${TRANSIST_HOME:-}" ] && echo "OK: TRANSIST_HOME removed" || echo "WARN: TRANSIST_HOME still present"
```

## Pass Criteria

- Secret delivery works through the `API_TOKEN` env binding.
- The secret value does not persist under `$TRANSIST_HOME`.
- Removing the secret causes the workflow to fail before dispatching the secret step.
- Setting `policy.remote_secret_delivery.process: false` causes preflight to fail before dispatching the secret step.
- Setting `policy.child_env.inherit: false` prevents unrelated parent env vars from reaching process children by default.
- Compiling a workflow fails when `with.secret_refs` includes an undeclared ref or omits a required action secret input.

Coverage notes:

- Verifies successful `process` secret delivery and non-persistence.
- Verifies missing-secret failure before dispatch.
- Verifies `policy.remote_secret_delivery.process: false` fails preflight with `remote_secret_policy` before dispatch.
- Verifies default-style child env isolation prevents unrelated parent env vars from reaching process children.
- Verifies authoring-time failures for undeclared and missing required secret refs.
