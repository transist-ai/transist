# Plugin Lifecycle E2E Test Plan

## Goal

Exercise plugin installation, lifecycle-reservation contention, inspection, and uninstall end to end with per-step shell assertions.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite plugin-lifecycle`.

## Not Covered

- Central inspector or docker.
- Network access.
- Workflow execution.

## Environment Variables

| Variable | Purpose | Lifecycle | Sensitive |
|---|---|---|---|
| `TRANSIST_BIN` | CLI binary | Boot | No |
| `TRANSIST_HOME` | ephemeral home | Boot | No |
| `SUITE_ID` | suite label | Boot | No |

## Boot-time Setup

```bash
if [ -n "${TRANSIST_BIN:-}" ]; then
  :
elif cargo run -p transist-cli -- --help >/dev/null 2>&1; then
  export TRANSIST_BIN="cargo run -p transist-cli --"
else
  echo "FAIL: unable to resolve TRANSIST_BIN"
  exit 1
fi

export SUITE_ID=plugin-lifecycle
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `plugins/shell/plugin.py` exists.

## Execution

### Step 1 -- Boot and install

```bash
$TRANSIST_BIN --home "$TRANSIST_HOME" home init
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/installation.json" ] && echo "OK: installation.json exists" || { echo "FAIL: missing installation.json"; exit 1; }
[ -f "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml" ] && echo "OK: shell 0.1.0 installed" || { echo "FAIL: missing shell install"; exit 1; }
```

### Step 2 -- Wait for a separate-process lifecycle reservation

The suite installs a minimal control-only workflow, then starts a helper process that
writes a valid live `plugin-lifecycle-reservation` lease. A separate
`workflows prepare` process must remain blocked without creating a run or
changing the lease. The helper explicitly removes the lease; preparation then
must publish all required run-birth files.

### Step 3 -- Reinstall idempotently

```bash
sha_before="$(python3 - <<'PY' "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
set +e
reinstall_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml 2>&1)"
reinstall_rc=$?
set -e
sha_after="$(python3 - <<'PY' "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
```

```bash
set -euo pipefail
if [ "$reinstall_rc" -ne 0 ]; then
  printf '%s' "$reinstall_out" | grep -q 'already installed' && echo "OK: reinstall rejected with already-installed guard" || { echo "FAIL: unexpected reinstall error: $reinstall_out"; exit 1; }
else
  [ "$sha_before" = "$sha_after" ] && echo "OK: reinstall left manifest unchanged" || { echo "FAIL: manifest changed on reinstall"; exit 1; }
fi
```

### Step 4 -- Inspect

```bash
inspect_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins inspect shell)"
```

```bash
set -euo pipefail
printf '%s' "$inspect_json" | jq -e '.[0].id == "shell" and .[0].version == "0.1.0"' >/dev/null && echo "OK: plugin identity pinned" || { echo "FAIL: plugin identity mismatch"; exit 1; }
printf '%s' "$inspect_json" | jq -e '.[0].actions | map(.name) | sort == ["echo", "exec", "exec_with_secret_env"]' >/dev/null && echo "OK: all three actions present" || { echo "FAIL: action set mismatch"; exit 1; }
printf '%s' "$inspect_json" | jq -e '.[0].actions[] | select(.name == "exec_with_secret_env") | .secret_inputs[0].name == "MY_API_TOKEN" and .secret_inputs[0].binding.name == "API_TOKEN"' >/dev/null && echo "OK: secret binding pinned" || { echo "FAIL: secret binding mismatch"; exit 1; }
```

### Step 5 -- Negative installs

```bash
set +e
missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path /tmp/definitely-missing-$(now_ms) 2>&1)"
missing_rc=$?
set -e

broken_dir="/tmp/broken-shell-$(now_ms)"
cp -R plugins/shell "$broken_dir"
python3 - <<'PY' "$broken_dir/plugin.yaml"
from pathlib import Path
path = Path(__import__('sys').argv[1])
text = path.read_text()
path.write_text(text.replace('schema: transist.plugin.v1\n', '', 1))
PY
set +e
broken_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$broken_dir/plugin.yaml" 2>&1)"
broken_rc=$?
set -e

symlink_dir="/tmp/symlink-shell-$(now_ms)"
cp -R plugins/shell "$symlink_dir"
ln -s plugin.py "$symlink_dir/linked.py"
set +e
symlink_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$symlink_dir/plugin.yaml" 2>&1)"
symlink_rc=$?
set -e

special_dir="/tmp/special-shell-$(now_ms)"
cp -R plugins/shell "$special_dir"
mkfifo "$special_dir/plugin.fifo"
set +e
special_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$special_dir/plugin.yaml" 2>&1)"
special_rc=$?
set -e

invalid_schema_dir="/tmp/invalid-schema-shell-$(now_ms)"
cp -R plugins/shell "$invalid_schema_dir"
python3 - <<'PY' "$invalid_schema_dir/schemas/exec.input.schema.json"
from pathlib import Path
path = Path(__import__('sys').argv[1])
text = path.read_text()
path.write_text(text.replace('"type": "object"', '"type": "not-a-json-schema-type"', 1))
PY
set +e
invalid_schema_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$invalid_schema_dir/plugin.yaml" 2>&1)"
invalid_schema_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$missing_rc" -ne 0 ] && echo "OK: missing plugin path rejected" || { echo "FAIL: missing path accepted"; exit 1; }
printf '%s' "$missing_out" | grep -Eq 'not found|missing' && echo "OK: missing path token present" || echo "WARN: missing path token changed"
[ "$broken_rc" -ne 0 ] && echo "OK: broken manifest rejected" || { echo "FAIL: broken manifest accepted"; exit 1; }
printf '%s' "$broken_out" | grep -Eq 'schema|invalid' && echo "OK: broken manifest token present" || echo "WARN: broken manifest token changed"
[ "$symlink_rc" -ne 0 ] && echo "OK: symlink plugin rejected" || { echo "FAIL: symlink plugin accepted"; exit 1; }
[ "$special_rc" -ne 0 ] && echo "OK: special-file plugin rejected" || { echo "FAIL: special-file plugin accepted"; exit 1; }
[ "$invalid_schema_rc" -ne 0 ] && echo "OK: invalid action schema rejected" || { echo "FAIL: invalid action schema accepted"; exit 1; }
printf '%s' "$invalid_schema_out" | grep -Eq 'schema|invalid' && echo "OK: invalid schema token present" || echo "WARN: invalid schema rejection token changed"
```

### Step 6 -- Uninstall

```bash
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins uninstall shell --version 0.1.0
```

```bash
set -euo pipefail
[ ! -d "$TRANSIST_HOME/plugins/shell/0.1.0" ] && echo "OK: shell version removed" || { echo "FAIL: shell version still present"; exit 1; }
plugin_list="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins list)"
printf '%s' "$plugin_list" | grep -q 'shell' && { echo "FAIL: shell still listed"; exit 1; } || echo "OK: shell removed from list"
```

### Step 7 -- Cleanup

```bash
HOME_TO_DELETE="$TRANSIST_HOME"
for _ in $(seq 1 10); do
  rm -rf "$HOME_TO_DELETE" 2>/dev/null || true
  [ ! -d "$HOME_TO_DELETE" ] && break
  sleep 1
done
unset TRANSIST_BIN TRANSIST_HOME SUITE_ID
```

```bash
set -euo pipefail
leaked=""
for v in TRANSIST_BIN TRANSIST_HOME SUITE_ID; do
  if [ -n "${!v:-}" ]; then leaked="$leaked $v"; fi
done
[ -z "$leaked" ] && echo "OK: suite-scoped shell env clean" || { echo "FAIL: suite env still set:$leaked"; env | grep -E "^(TRANSIST_BIN|TRANSIST_HOME|SUITE_ID)="; exit 1; }
[ ! -d "$HOME_TO_DELETE" ] && echo "OK: TRANSIST_HOME removed" || echo "WARN: TRANSIST_HOME still present"
```

## Pass Criteria

- `shell` installs as `0.1.0`.
- `plugin inspect` reports `exec`, `echo`, and `exec_with_secret_env`.
- Negative install probes fail, including symlink and special-file fixtures.
- A separate prepare process waits for an unchanged live plugin-lifecycle lease,
  creates no partial run, and publishes a complete run after the helper removes
  the lease.
- Uninstall removes the installed version.

Last verified:

- transist @ main

Coverage notes:

- Plugin authoring and install validation now depend on schema parity for prerequisites, provider locality, optional `secret_inputs`/`bindings`, and MCP `bindings.tool_name`.
- Prerequisites are static (`binaries`, `env`, and `paths`); legacy executable fields are rejected with `legacy_executable_prerequisite_unsupported` before install or APP probing.
- Runtime manifest validation rejects duplicate action names and adapter/entrypoint mismatches.
