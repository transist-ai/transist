#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

SUITE_ID="workflow-authorization"
REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)}"
TRANSIST_BIN="${REPO_ROOT}/target/debug/transist"
TRANSIST_ADMIN_BIN="${REPO_ROOT}/target/debug/transist-admin"
SUITE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/.transist-workflow-authorization-XXXXXX")"
# Runtime storage rejects symlink ancestors; macOS /tmp needs a physical path.
SUITE_ROOT="$(cd "$SUITE_ROOT" && pwd -P)"
TRANSIST_HOME="${SUITE_ROOT}/home"
trap 'rm -rf "$SUITE_ROOT"' EXIT

# A terminal state is committed before the detached runner writes its final
# completion record. Wait for that record so the EXIT trap cannot remove the
# suite home while the runner still owns files beneath it.
wait_for_runner_finish() {
  local run_id="$1"
  local runner_log="${TRANSIST_HOME}/runs/run_${run_id}/runner.log"

  for _ in $(seq 1 30); do
    grep -Fq '"event":"finished"' "$runner_log" 2>/dev/null && return 0
    sleep 0.2
  done

  echo "FAIL: ${run_id} runner did not finish"
  return 1
}

for required in "$TRANSIST_BIN" "$TRANSIST_ADMIN_BIN"; do
  [[ -x "$required" ]] || { echo "FAIL: missing required binary: $required"; exit 1; }
done

repos_root="${SUITE_ROOT}/independent-repositories"
repo_a="${repos_root}/repository-a"
repo_b="${repos_root}/repository-b"
mkdir -p "$repo_a" "$repo_b"
repo_a="$(cd "$repo_a" && pwd -P)"
repo_b="$(cd "$repo_b" && pwd -P)"
common_parent="$(cd "$repos_root" && pwd -P)"

staged="${SUITE_ROOT}/staged"
mkdir -p "$staged"
cp -R "${REPO_ROOT}/plugins/shell" "${staged}/shell"
cat > "${staged}/workflow.yaml" <<'YAML'
schema: transist.flow.v1
id: authorization_demo
version: v1
plugins:
  shell: 0.1.0
network:
  mode: none
inputs:
  repo_path:
    type: string
    required: true
workspace:
  path: "{{inputs.repo_path}}"
  access: full
steps:
  - id: write_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-eu", "-c", "printf 'authorized\n' > authorization-marker.txt"]
YAML

key="${SUITE_ROOT}/signing.key"
trust_root="${SUITE_ROOT}/signing.pub"
package_v1="${SUITE_ROOT}/workflow-package-v1"
package_v2="${SUITE_ROOT}/workflow-package-v2"
"$TRANSIST_ADMIN_BIN" key generate --name workflow-authorization --private-key-file "$key" >/dev/null
"$TRANSIST_ADMIN_BIN" key export-public --private-key-file "$key" --trust-root-file "$trust_root" >/dev/null
"$TRANSIST_ADMIN_BIN" plugin sign "${staged}/shell" --private-key-file "$key" --output "${staged}/shell.sig" >/dev/null
"$TRANSIST_ADMIN_BIN" workflow package "${staged}/workflow.yaml" --version v1 --private-key-file "$key" --output "$package_v1" >/dev/null
"$TRANSIST_ADMIN_BIN" workflow package "${staged}/workflow.yaml" --version v2 --private-key-file "$key" --output "$package_v2" >/dev/null
# A structurally valid signed package whose live APP handshake always fails.
mkdir -p "${staged}/broken-plugin"
printf '#!/usr/bin/env python3\nraise RuntimeError("deliberately unavailable live plugin")\n' > "${staged}/broken-plugin/probe.py"
chmod +x "${staged}/broken-plugin/probe.py"
printf '{}\n' > "${staged}/broken-plugin/input.json"
printf '{}\n' > "${staged}/broken-plugin/output.json"
cat > "${staged}/broken-plugin/plugin.yaml" <<'YAML'
schema: transist.plugin.v1
id: boundary.broken
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint: {type: process, command: probe.py}
package:
  files: [probe.py, input.json, output.json]
actions:
  - name: echo
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
YAML
cat > "${staged}/boundary.yaml" <<YAML
schema: transist.flow.v1
id: validation_boundary
version: v1
plugins: {boundary.broken: 1.0.0}
workspace: {path: "$repo_a", access: read_only}
network: {mode: none}
steps:
  - id: echo
    uses: process:boundary.broken/echo
YAML
boundary_package="${SUITE_ROOT}/boundary-package"
"$TRANSIST_ADMIN_BIN" plugin sign "${staged}/broken-plugin" --private-key-file "$key" --output "${staged}/broken-plugin.sig" >/dev/null
"$TRANSIST_ADMIN_BIN" workflow package "${staged}/boundary.yaml" --version v1 --private-key-file "$key" --output "$boundary_package" >/dev/null
rm -f "$key"

"$TRANSIST_BIN" --home "$TRANSIST_HOME" home init --mode governed >/dev/null
cat > "${TRANSIST_HOME}/config.yaml" <<'YAML'
version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  sources: [workflow-authorizations]
YAML
mkdir -p "${TRANSIST_HOME}/policy/workflow-authorizations"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" trust import "$trust_root" >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" plugins install --path "${staged}/shell/plugin.yaml" --trusted-execution >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check --path "${staged}/workflow.yaml" >/dev/null

# Intentional legacy syntax: reject usage before policy writes or run birth.
usage_rejections=0
for command in check authorize prepare run; do
  for target in missing legacy malformed extra; do
    args=("workflows" "$command")
    case "$target" in
      missing) ;;
      legacy) args+=(--workflow authorization_demo@v1) ;;
      malformed) args+=(authorization_demo) ;;
      extra) args+=(authorization_demo@v1 old-run-id) ;;
    esac
    if "$TRANSIST_BIN" --home "$TRANSIST_HOME" "${args[@]}" >"${SUITE_ROOT}/usage.out" 2>"${SUITE_ROOT}/usage.err"; then
      echo "FAIL: invalid $command $target usage succeeded"
      exit 1
    else
      status=$?
      [[ "$status" -eq 2 ]] || { echo "FAIL: $command $target returned $status instead of usage exit 2"; exit 1; }
    fi
    [[ ! -s "${SUITE_ROOT}/usage.out" ]] || { echo "FAIL: usage error reached dispatch"; exit 1; }
    grep -q 'error:' "${SUITE_ROOT}/usage.err" || { echo "FAIL: missing usage diagnostic"; exit 1; }
    usage_rejections=$((usage_rejections + 1))
  done
done
if "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check --path "${staged}/workflow.yaml" --live >/dev/null 2>&1; then
  echo "FAIL: authored live check was accepted"
  exit 1
fi
[[ -z "$(find "${TRANSIST_HOME}/policy/workflow-authorizations" -type f -print -quit)" ]] || { echo "FAIL: usage errors wrote policy"; exit 1; }
[[ -z "$(find "${TRANSIST_HOME}/runs" -name state.json -print -quit)" ]] || { echo "FAIL: usage errors created runs"; exit 1; }
echo "OK: ${usage_rejections} invalid positional commands and authored --live rejected before writes"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install --package "$package_v1" --approve >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install --package "$package_v2" --approve >/dev/null

inputs_a="${SUITE_ROOT}/inputs-a.json"
inputs_b="${SUITE_ROOT}/inputs-b.json"
printf '{"repo_path":"%s"}\n' "$repo_a" > "$inputs_a"
printf '{"repo_path":"%s"}\n' "$repo_b" > "$inputs_b"

for inputs in "$inputs_a" "$inputs_b"; do
  if "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check authorization_demo@v1 --inputs-file "$inputs" >/dev/null 2>&1; then
    echo "FAIL: unapproved repository unexpectedly passed readiness"; exit 1
  fi
done
echo "OK: both unrelated repositories are blocked before authorization"

invalid_inputs="${SUITE_ROOT}/inputs-invalid.json"
printf '{}\n' > "$invalid_inputs"
if blocked_report="$("$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize \
  authorization_demo@v1 --inputs-file "$invalid_inputs" --approve --format json)"; then
  echo "FAIL: invalid inputs unexpectedly offered authorization"; exit 1
fi
jq -e '.status == "blocked" and .run_created == false and
  .diagnostics.authorization.eligible == false and
  (.diagnostics.authorization.blocking_codes | index("bound_validation") != null)' \
  <<< "$blocked_report" >/dev/null || {
  echo "FAIL: blocked authorization did not identify the non-authority blocker"; exit 1;
}
shopt -s nullglob
fragments_before=("${TRANSIST_HOME}/policy/workflow-authorizations/"*.yaml)
shopt -u nullglob
[[ "${#fragments_before[@]}" -eq 0 ]] || {
  echo "FAIL: invalid inputs wrote a policy fragment"; exit 1;
}
echo "OK: invalid inputs block authorization without publishing grants"

plan_report="$("$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize \
  authorization_demo@v1 --inputs-file "$inputs_a" --dry-run --format json)"
jq -e '.status == "planned" and .diagnostics.authorization.eligible == true and
  .diagnostics.authorization.missing_capabilities > 0 and
  .diagnostics.groups.authority.failed > 0' <<< "$plan_report" >/dev/null || {
  echo "FAIL: policy-only plan did not expose grantable authority"; exit 1;
}

if "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows prepare --run-ref denied-before-authorization \
  authorization_demo@v1 --inputs-file "$inputs_a" >/dev/null 2>&1; then
  echo "FAIL: unauthorized preparation unexpectedly created a run"; exit 1
fi
[[ ! -e "${TRANSIST_HOME}/runs/run_denied-before-authorization" ]] || {
  echo "FAIL: unauthorized preparation wrote run storage"; exit 1;
}
echo "OK: unauthorized preparation creates no run storage"

"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize \
  authorization_demo@v1 --input "repo_path=$repo_a" --approve >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  authorization_demo@v1 --input "repo_path=$repo_a" >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize \
  authorization_demo@v2 --input "repo_path=$repo_a" --approve >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  authorization_demo@v2 --input "repo_path=$repo_a" >/dev/null
if "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  authorization_demo@v1 --inputs-file "$inputs_b" >/dev/null 2>&1; then
  echo "FAIL: repository A authorization widened to repository B"; exit 1
fi

"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize \
  authorization_demo@v1 --inputs-file "$inputs_b" --approve >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check \
  authorization_demo@v1 --inputs-file "$inputs_b" >/dev/null

fragments=()
while IFS= read -r fragment; do
  fragments+=("$fragment")
done < <(find "${TRANSIST_HOME}/policy/workflow-authorizations" -type f -name '*.yaml' | sort)
[[ "${#fragments[@]}" -eq 3 ]] || { echo "FAIL: expected three exact authorization fragments"; exit 1; }
grant_set_count="$({ grep -h '^  authorization-' "${fragments[@]}" || true; } | sort -u | wc -l | tr -d ' ')"
[[ "$grant_set_count" -eq 3 ]] || {
  echo "FAIL: workflow versions and input-bound grants must use distinct authorization grant sets"; exit 1;
}
for fragment in "${fragments[@]}"; do
  grep -Fq "$repo_a" "$fragment" || grep -Fq "$repo_b" "$fragment" || {
    echo "FAIL: fragment has no exact repository grant: $fragment"; exit 1;
  }
  ! grep -Eq "resource: workspace://${common_parent}([[:space:]]|$)" "$fragment" || {
    echo "FAIL: fragment widened authority to common parent: $fragment"; exit 1;
  }
done
echo "OK: workflow versions and input-bound authorizations use distinct exact grants"

"$TRANSIST_BIN" --home "$TRANSIST_HOME" plugins install --path "${staged}/broken-plugin/plugin.yaml" --trusted-execution >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install --package "$boundary_package" --approve >/dev/null
jq -e '.entries["validation_boundary@v1"].definition_validation.validator_revision == 2' \
  "${TRANSIST_HOME}/registry/workflow-install-index.json" >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize validation_boundary@v1 --approve >/dev/null
validation_report="$("$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check validation_boundary@v1 --format json)"
jq -e '.evaluation_mode == "validation" and .validation_status == "passed" and .status == "not_ready" and .run_created == false' <<< "$validation_report" >/dev/null
policy_before="$(find "${TRANSIST_HOME}/policy/workflow-authorizations" -type f -exec shasum -a 256 {} \; | sort)"
for _ in $(seq 1 10); do
  "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check validation_boundary@v1 >/dev/null
  authorization_report="$("$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows authorize validation_boundary@v1 --dry-run --format json)"
  jq -e '.status == "already_authorized" and .readiness.evaluation_mode == "validation" and .readiness.validation_status == "passed"' <<< "$authorization_report" >/dev/null
done
[[ "$policy_before" == "$(find "${TRANSIST_HOME}/policy/workflow-authorizations" -type f -exec shasum -a 256 {} \; | sort)" ]] || { echo "FAIL: repeat commands wrote policy"; exit 1; }
if "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows prepare validation_boundary@v1 --run-ref validation-boundary-denied \
  >"${SUITE_ROOT}/boundary.out" 2>"${SUITE_ROOT}/boundary.err"; then
  echo "FAIL: successful fast validation bypassed failed live preparation"; exit 1
fi
jq -e '.evaluation_mode == "live_readiness" and .validation_status == "failed" and .run_created == false and
  .validation.missing_capabilities == [] and
  any(.checks[]; .code == "adapter_handshake" and .status == "failed")' "${SUITE_ROOT}/boundary.out" >/dev/null || {
  echo "FAIL: live preparation failed for an unrelated reason"; exit 1;
}
[[ -z "$(find "${TRANSIST_HOME}/runs" -name state.json -print -quit)" ]] || { echo "FAIL: failed live preparation created run state"; exit 1; }
[[ -z "$(find "${TRANSIST_HOME}/runs" -mindepth 1 -print -quit)" ]] || { echo "FAIL: failed live preparation left run storage or locators"; exit 1; }
echo "OK: receipt-backed fast validation passes but failed live preparation creates no run"

for name in a b; do
  inputs_var="inputs_${name}"
  inputs="${!inputs_var}"
  run_id="authorization-${name}"
  "$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows prepare --run-ref "$run_id" \
    authorization_demo@v1 --inputs-file "$inputs" >/dev/null
  "$TRANSIST_BIN" --home "$TRANSIST_HOME" runs start "$run_id" --ui plain >/dev/null
  state_path="${TRANSIST_HOME}/runs/run_${run_id}/state.json"
  status=""
  for _ in $(seq 1 30); do
    [[ -f "$state_path" ]] && status="$(jq -r '.status // ""' "$state_path" 2>/dev/null || true)"
    [[ "$status" == "success" || "$status" == "failed" ]] && break
    sleep 0.2
  done
  [[ "$status" == "success" ]] || { echo "FAIL: $run_id did not succeed: $status"; exit 1; }
  wait_for_runner_finish "$run_id"
done
[[ -f "${repo_a}/authorization-marker.txt" && -f "${repo_b}/authorization-marker.txt" ]] || {
  echo "FAIL: authorized runs did not write both repository markers"; exit 1;
}

# Both run-ID forms preserve the installed workflow and bound input request.
for selection in explicit generated; do
  args=(workflows run authorization_demo@v1 --inputs-file "$inputs_a" --ui plain)
  [[ "$selection" != explicit ]] || args+=(--run-ref authorization-explicit-run)
  announcement="$("$TRANSIST_BIN" --home "$TRANSIST_HOME" "${args[@]}")"
  run_id="$(jq -r '.run_id' <<<"$announcement")"
  if [[ "$selection" == explicit ]]; then
    [[ "$run_id" == authorization-explicit-run ]] || { echo "FAIL: explicit run ID changed"; exit 1; }
  else
    [[ "$run_id" =~ ^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$ ]] || { echo "FAIL: missing generated run ID"; exit 1; }
  fi
  wait_for_runner_finish "$run_id"
  run_root="${TRANSIST_HOME}/runs/run_${run_id}"
  [[ "$(jq -r '.status' "${run_root}/state.json")" == success ]] || { echo "FAIL: positional run failed"; exit 1; }
  jq -e --arg repo "$repo_a" '.workflow.id == "authorization_demo" and .workflow.version == "v1" and .values.repo_path == $repo' \
    "${run_root}/context/run_inputs.json" >/dev/null
done
echo "OK: explicit and generated workflow run IDs preserve workflow and inputs"
echo "PASS: ${SUITE_ID}"
