#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: detached-assets
# Hermetic E2E - governed execution of captured prompt and source assets.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="${TRANSIST_BIN:-$REPO_ROOT/target/debug/transist}"
TRANSIST_ADMIN_BIN="${TRANSIST_ADMIN_BIN:-$REPO_ROOT/target/debug/transist-admin}"
export SUITE_ID=detached-assets
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
work_root="$(mktemp -d -t "transist-${SUITE_ID}-XXXXXX")"

cleanup() {
  rm -rf "$TRANSIST_HOME" "$work_root" 2>/dev/null || true
}
trap cleanup EXIT

for required in "$TRANSIST_BIN" "$TRANSIST_ADMIN_BIN"; do
  if [[ ! -x "$required" ]]; then
    echo "FAIL: required binary is missing or not executable: $required" >&2
    exit 1
  fi
done
command -v jq >/dev/null || { echo "FAIL: jq not found" >&2; exit 1; }
command -v python3 >/dev/null || { echo "FAIL: python3 not found" >&2; exit 1; }

staged_root="$work_root/fixture"
workspace="$work_root/workspace"
mkdir -p "$staged_root" "$workspace"
workspace="$(cd "$workspace" && pwd -P)"
cp "$SCRIPT_DIR/detached.workflow.yaml" "$staged_root/detached.workflow.yaml"
cp "$SCRIPT_DIR/config.yaml" "$staged_root/config.yaml"
cp -R "$SCRIPT_DIR/prompt-plugin" "$staged_root/prompt-plugin"
cp -R "$SCRIPT_DIR/prompts" "$staged_root/prompts"
cp -R "$SCRIPT_DIR/scripts" "$staged_root/scripts"
cp -R "$REPO_ROOT/plugins/shell" "$staged_root/shell-plugin"
chmod +x "$staged_root/prompt-plugin/plugin.py"

# Replace the fixture's documentation-friendly workspace with this run's
# private workspace before the governed policy is loaded.
sed "s#workspace:///e2e#workspace://$workspace#g" \
  "$staged_root/config.yaml" > "$work_root/config.yaml"

key_path="$work_root/detached-e2e.key"
public_key_path="$work_root/detached-e2e.pub"
"$TRANSIST_ADMIN_BIN" key generate --name detached-e2e --private-key-file "$key_path" >/dev/null
"$TRANSIST_ADMIN_BIN" key export-public --private-key-file "$key_path" --trust-root-file "$public_key_path" >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" home init --mode governed >/dev/null
cp "$work_root/config.yaml" "$TRANSIST_HOME/config.yaml"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" trust import "$public_key_path" >/dev/null

# Sign staged copies so no trust material or generated output enters the
# checkout. The workflow signature covers its prompt and source assets.
"$TRANSIST_ADMIN_BIN" plugin sign "$staged_root/prompt-plugin" \
  --private-key-file "$key_path" --output "$staged_root/prompt-plugin.sig" >/dev/null
"$TRANSIST_ADMIN_BIN" plugin sign "$staged_root/shell-plugin" \
  --private-key-file "$key_path" --output "$staged_root/shell-plugin.sig" >/dev/null
workflow_package="$work_root/detached-package"
"$TRANSIST_ADMIN_BIN" workflow package "$staged_root/detached.workflow.yaml" \
  --version v1 --private-key-file "$key_path" --output "$workflow_package" >/dev/null
rm -f "$key_path"

"$TRANSIST_BIN" --home "$TRANSIST_HOME" plugins install \
  --path "$staged_root/prompt-plugin/plugin.yaml" --trusted-execution >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" plugins install \
  --path "$staged_root/shell-plugin/plugin.yaml" --trusted-execution >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  --path "$staged_root/detached.workflow.yaml" >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install \
  --package "$workflow_package" --approve >/dev/null
inputs_file="$work_root/inputs.json"
printf '{"repo_path":"%s"}\n' "$workspace" > "$inputs_file"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  detached.demo@v1 --inputs-file "$inputs_file" --trusted-execution >/dev/null

run_id="e2e-detached-success"
launch_log="$work_root/launch.log"
(
   cd "$workspace"
   "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows run detached.demo@v1 --run-ref "$run_id" \
      --inputs-file "$inputs_file" --trusted-execution --ui plain
) >"$launch_log" 2>&1 &
launch_pid=$!

state_path="$TRANSIST_HOME/runs/run_${run_id}/state.json"
status=""
for _ in $(seq 1 60); do
  status="$(jq -r '.status // ""' "$state_path" 2>/dev/null || true)"
  case "$status" in
    success|failed|canceled) break ;;
  esac
  sleep 0.5
done
launch_rc=0
wait "$launch_pid" || launch_rc=$?

if [[ "$status" != "success" ]]; then
  echo "FAIL: detached-assets run did not succeed (status: ${status:-missing}, launch rc: $launch_rc)" >&2
  cat "$launch_log" >&2
  runner_log="$TRANSIST_HOME/runs/run_${run_id}/runner.log"
  if [[ -f "$runner_log" ]]; then
    echo "--- detached runner log ---" >&2
    cat "$runner_log" >&2
  fi
  exit 1
fi
echo "OK: governed detached-assets run succeeded"

jq -e '.trusted_execution_mode == true and .trust_decision.decision == "allowed" and .asset_lock_version == 1 and .workflow_asset_snapshot == true' \
  "$state_path" >/dev/null && echo "OK: trusted decision and asset lock version recorded" || {
  echo "FAIL: trusted state metadata mismatch" >&2
  exit 1
}
jq -e '.steps.consume_prompt.status == "succeeded" and .steps.consume_source.status == "succeeded" and .steps.consume_prompt.executions == 1 and .steps.consume_source.executions == 1' \
  "$state_path" >/dev/null && echo "OK: each detached asset step executed once" || {
  echo "FAIL: detached asset step state mismatch" >&2
  exit 1
}
jq -e '.steps.consume_prompt.last_result.output.stdout == "detached prompt consumed"' \
  "$state_path" >/dev/null && echo "OK: captured prompt reached the plugin" || {
  echo "FAIL: captured prompt output mismatch" >&2
  exit 1
}

run_root="$TRANSIST_HOME/runs/run_${run_id}"
manifest="$run_root/workflow_assets/manifest.json"
jq -e '.schema == "transist.workflow_asset_snapshot.v1" and ([.entries[].path] | sort) == ["source/prompts/task.md", "source/scripts/hello.sh"]' \
  "$manifest" >/dev/null && echo "OK: prompt and source assets were captured" || {
  echo "FAIL: workflow asset manifest mismatch" >&2
  exit 1
}
while IFS= read -r sha256; do
  [[ -f "$run_root/workflow_assets/blobs/$sha256" ]] || {
    echo "FAIL: missing captured workflow asset blob: $sha256" >&2
    exit 1
  }
done < <(jq -r '.entries[].sha256' "$manifest")
echo "OK: captured asset blobs are present"

# The post-completion inspection path must remain available after authored
# prompt/source files are removed from the workflow root.
rm "$staged_root/prompts/task.md" "$staged_root/scripts/hello.sh"
[[ ! -e "$staged_root/prompts/task.md" && ! -e "$staged_root/scripts/hello.sh" ]] \
  && echo "OK: authored detached files removed"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" runs show "$run_id" >/dev/null
echo "OK: run inspection remains available after authored-file deletion"

echo "PASS: $SUITE_ID"
