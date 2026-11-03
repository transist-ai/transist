#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: plugin-lifecycle
# Hermetic E2E — plugins install, list, inspect, uninstall.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=plugin-lifecycle
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"

cleanup() {
  if [ -n "${prepare_pid:-}" ]; then
    kill "$prepare_pid" 2>/dev/null || true
    wait "$prepare_pid" 2>/dev/null || true
  fi
  if [ -n "${reservation_helper_pid:-}" ]; then
    kill "$reservation_helper_pid" 2>/dev/null || true
    wait "$reservation_helper_pid" 2>/dev/null || true
  fi
  rm -rf "$TRANSIST_HOME" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

# Step 1 -- Boot and install
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"

[ -f "$TRANSIST_HOME/installation.json" ] && echo "OK: installation.json exists" || { echo "FAIL: missing installation.json"; exit 1; }
[ -f "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml" ] && echo "OK: shell 0.1.0 installed" || { echo "FAIL: missing shell install"; exit 1; }

# Step 2 -- A separate process retains the lifecycle reservation while prepare waits.
cat >"$TRANSIST_HOME/reservation-wait.flow.yaml" <<'YAML'
schema: transist.flow.v1
id: reservation_wait
version: v1
network:
  mode: none
steps:
  - id: finish
    control: control.noop
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/reservation-wait.flow.yaml" --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check reservation_wait@v1 >/dev/null

reservation_run_id="reservation-wait-$(now_ms)"
reservation_lock="$TRANSIST_HOME/locks/runs/plugin-lifecycle-reservation.lock"
reservation_lock_before="$TRANSIST_HOME/reservation.lock.before"
reservation_release="$TRANSIST_HOME/release-reservation"
mkdir -p "$(dirname "$reservation_lock")"
python3 - "$reservation_lock" "$reservation_release" <<'PY' &
import json
import os
from pathlib import Path
import sys
import time

lock_path = Path(sys.argv[1])
release_path = Path(sys.argv[2])
record = {
    "schema_version": "transist.run_tree_lease.v1",
    "root_run_id": "plugin-lifecycle-reservation",
    "lease_id": "plugin-lifecycle-e2e-helper",
    "owner_id": "plugin-lifecycle-e2e-helper",
    "management_mode": "standalone",
    "pid": os.getpid(),
    "hostname": "plugin-lifecycle-e2e-helper",
    "command": "plugin-lifecycle-e2e-helper",
    "transist_version": "test",
    "acquired_at_ms": int(time.time() * 1000),
    "last_heartbeat_ms": int(time.time() * 1000),
}
lock_path.write_text(json.dumps(record, separators=(",", ":")))
while not release_path.exists():
    time.sleep(0.02)
lock_path.unlink()
PY
reservation_helper_pid=$!
for _ in $(seq 1 50); do
  [ -f "$reservation_lock" ] && break
  sleep 0.02
done
[ -f "$reservation_lock" ] || { echo "FAIL: helper did not create lifecycle reservation"; exit 1; }
cp "$reservation_lock" "$reservation_lock_before"

$TRANSIST_BIN --home "$TRANSIST_HOME" workflows prepare reservation_wait@v1 --run-ref "$reservation_run_id" >"$TRANSIST_HOME/reservation-prepare.out" 2>"$TRANSIST_HOME/reservation-prepare.err" &
prepare_pid=$!
sleep 0.3
kill -0 "$prepare_pid" 2>/dev/null || { wait "$prepare_pid" || true; echo "FAIL: prepare did not wait for lifecycle reservation"; exit 1; }
[ ! -e "$TRANSIST_HOME/runs/run_${reservation_run_id}" ] && echo "OK: lifecycle reservation blocks partial run creation" || { echo "FAIL: prepare created a partial run while reservation was live"; exit 1; }
cmp -s "$reservation_lock_before" "$reservation_lock" && echo "OK: live lifecycle reservation remained unchanged" || { echo "FAIL: prepare modified the live lifecycle reservation"; exit 1; }

touch "$reservation_release"
wait "$reservation_helper_pid"
unset reservation_helper_pid
set +e
wait "$prepare_pid"
prepare_rc=$?
set -e
unset prepare_pid
[ "$prepare_rc" -eq 0 ] || { echo "FAIL: prepare did not succeed after lifecycle reservation release"; cat "$TRANSIST_HOME/reservation-prepare.err"; exit 1; }
[ -f "$TRANSIST_HOME/runs/run_${reservation_run_id}/state.json" ] && \
  [ -f "$TRANSIST_HOME/runs/run_${reservation_run_id}/recipe.yaml" ] && \
  [ -f "$TRANSIST_HOME/runs/run_${reservation_run_id}/context/run_inputs.json" ] && \
  [ -f "$TRANSIST_HOME/runs/run_${reservation_run_id}/provenance.json" ] && \
  echo "OK: prepare published a complete run after lifecycle reservation release" || { echo "FAIL: prepare did not publish a complete run"; exit 1; }

# Step 3 -- Reinstall idempotency
sha_before="$(python3 - <<'PY' "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
set +e
reinstall_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml" 2>&1)"
reinstall_rc=$?
set -e
sha_after="$(python3 - <<'PY' "$TRANSIST_HOME/plugins/shell/0.1.0/plugin.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"

if [ "$reinstall_rc" -ne 0 ]; then
  printf '%s' "$reinstall_out" | grep -q 'already installed' && echo "OK: reinstall rejected with already-installed guard" || { echo "FAIL: unexpected reinstall error: $reinstall_out"; exit 1; }
else
  [ "$sha_before" = "$sha_after" ] && echo "OK: reinstall left manifest unchanged" || { echo "FAIL: manifest changed on reinstall"; exit 1; }
fi

# Step 4 -- Inspect
inspect_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins inspect shell)"

printf '%s' "$inspect_json" | jq -e '.[0].id == "shell" and .[0].version == "0.1.0"' >/dev/null && echo "OK: plugin identity pinned" || { echo "FAIL: plugin identity mismatch"; exit 1; }
printf '%s' "$inspect_json" | jq -e '.[0].actions | map(.name) | sort == ["echo", "exec", "exec_with_secret_env"]' >/dev/null && echo "OK: all three actions present" || { echo "FAIL: action set mismatch"; exit 1; }
printf '%s' "$inspect_json" | jq -e '.[0].actions[] | select(.name == "exec_with_secret_env") | .secret_inputs[0].name == "MY_API_TOKEN" and .secret_inputs[0].binding.name == "API_TOKEN"' >/dev/null && echo "OK: secret binding pinned" || { echo "FAIL: secret binding mismatch"; exit 1; }

# Step 5 -- Negative installs
set +e
missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "/tmp/definitely-missing-$(now_ms)" 2>&1)"
missing_rc=$?
set -e

broken_dir="/tmp/broken-shell-$$"
cp -R "$REPO_ROOT/plugins/shell" "$broken_dir"
python3 - "$broken_dir/plugin.yaml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
path.write_text(text.replace('schema: transist.plugin.v1\n', '', 1))
PY
set +e
broken_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$broken_dir/plugin.yaml" 2>&1)"
broken_rc=$?
set -e

symlink_dir="/tmp/symlink-shell-$$"
cp -R "$REPO_ROOT/plugins/shell" "$symlink_dir"
ln -s plugin.py "$symlink_dir/linked.py"
set +e
symlink_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$symlink_dir/plugin.yaml" 2>&1)"
symlink_rc=$?
set -e

special_dir="/tmp/special-shell-$$"
cp -R "$REPO_ROOT/plugins/shell" "$special_dir"
mkfifo "$special_dir/plugin.fifo" 2>/dev/null || true
set +e
special_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$special_dir/plugin.yaml" 2>&1)"
special_rc=$?
set -e

invalid_schema_dir="/tmp/invalid-schema-shell-$$"
cp -R "$REPO_ROOT/plugins/shell" "$invalid_schema_dir"
python3 - "$invalid_schema_dir/schemas/exec.input.schema.json" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
path.write_text(text.replace('"type": "object"', '"type": "not-a-json-schema-type"', 1))
PY
set +e
invalid_schema_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$invalid_schema_dir/plugin.yaml" 2>&1)"
invalid_schema_rc=$?
set -e

[ "$missing_rc" -ne 0 ] && echo "OK: missing plugin path rejected" || { echo "FAIL: missing path accepted"; exit 1; }
printf '%s' "$missing_out" | grep -Eq 'not found|missing' && echo "OK: missing path token present" || echo "WARN: missing path token changed"
[ "$broken_rc" -ne 0 ] && echo "OK: broken manifest rejected" || { echo "FAIL: broken manifest accepted"; exit 1; }
printf '%s' "$broken_out" | grep -Eq 'schema|invalid' && echo "OK: broken manifest token present" || echo "WARN: broken manifest token changed"
[ "$symlink_rc" -ne 0 ] && echo "OK: symlink plugin rejected" || { echo "FAIL: symlink plugin accepted"; exit 1; }
[ "$special_rc" -ne 0 ] && echo "OK: special-file plugin rejected" || { echo "FAIL: special-file plugin accepted"; exit 1; }
[ "$invalid_schema_rc" -ne 0 ] && echo "OK: invalid action schema rejected" || { echo "FAIL: invalid action schema accepted"; exit 1; }
printf '%s' "$invalid_schema_out" | grep -Eq 'schema|invalid' && echo "OK: invalid schema token present" || echo "WARN: invalid schema rejection token changed"

# Cleanup negative-install dirs
rm -rf "$broken_dir" "$symlink_dir" "$special_dir" "$invalid_schema_dir" 2>/dev/null || true

# Step 6 -- Uninstall
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins uninstall shell --version 0.1.0

[ ! -d "$TRANSIST_HOME/plugins/shell/0.1.0" ] && echo "OK: shell version removed" || { echo "FAIL: shell version still present"; exit 1; }
plugin_list="$($TRANSIST_BIN --home "$TRANSIST_HOME" plugins list)"
printf '%s' "$plugin_list" | grep -q 'shell' && { echo "FAIL: shell still listed"; exit 1; } || echo "OK: shell removed from list"

echo "PASS: $SUITE_ID"
