#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fail=0
tmp_files=()

cleanup() {
  local path
  for path in "${tmp_files[@]:-}"; do
    if [[ -n "$path" && -e "$path" ]]; then
      rm -f "$path"
    fi
  done
  return 0
}
trap cleanup EXIT

report_failure() {
  echo "$1" >&2
  fail=1
}

make_tmp_file() {
  local path
  path="$(mktemp "${TMPDIR:-/tmp}/transist-skill-validate.XXXXXX")"
  tmp_files+=("$path")
  printf '%s\n' "$path"
}

canonical_existing_path() {
  local path="$1"
  local dir base
  if [[ -d "$path" ]]; then
    (cd "$path" && pwd -P)
    return
  fi
  dir="$(dirname "$path")"
  base="$(basename "$path")"
  (
    cd "$dir" &&
      printf '%s/%s\n' "$(pwd -P)" "$base"
  )
}

normalize_markdown_link() {
  local raw="$1"
  local target
  [[ -z "$raw" ]] && return 1
  case "$raw" in
    \#* | http://* | https://*)
      return 1
      ;;
  esac
  target="$(printf '%s' "$raw" | sed -E 's/#.*$//; s/^[[:space:]]+//; s/[[:space:]]+$//')"
  [[ -z "$target" ]] && return 1
  [[ "$target" == *.md ]] || return 1
  printf '%s\n' "$target"
}

extract_markdown_links() {
  local file="$1"
  grep -oE '\[[^]]+\]\(([^)]+)\)' "$file" | sed -E 's/.*\(([^)]+)\)$/\1/' || true
}

resolve_markdown_link() {
  local current_doc="$1"
  local link="$2"
  if [[ "$link" = /* ]]; then
    printf '%s\n' "$link"
  else
    printf '%s/%s\n' "$(dirname "$current_doc")" "$link"
  fi
}

extract_frontmatter_field() {
  local file="$1"
  local key="$2"
  awk -v key="$key" '
    NR == 1 && $0 == "---" { in_frontmatter = 1; next }
    in_frontmatter && $0 == "---" { exit }
    in_frontmatter {
      pattern = "^[[:space:]]*" key ":[[:space:]]*"
      if ($0 ~ pattern) {
        value = $0
        sub(pattern, "", value)
        gsub(/^["'"'"']|["'"'"']$/, "", value)
        if (value != "") {
          print value
        }
        exit
      }
    }
  ' "$file"
}

extract_interface_field() {
  local file="$1"
  local key="$2"
  awk -v key="$key" '
    /^interface:[[:space:]]*$/ { in_interface = 1; next }
    in_interface && /^[^[:space:]]/ { exit }
    in_interface {
      pattern = "^[[:space:]]+" key ":[[:space:]]*"
      if ($0 ~ pattern) {
        value = $0
        sub(pattern, "", value)
        gsub(/^["'"'"']|["'"'"']$/, "", value)
        if (value != "") {
          print value
        }
        exit
      }
    }
  ' "$file"
}

assert_skill_heading_present() {
  local skill_file="$1"
  local section_name="$2"
  local reason="$3"
  local heading_regex

  heading_regex="^[[:space:]]*#{1,6}[[:space:]]+${section_name}([[:space:]]+#+)?[[:space:]]*$"
  if ! grep -Eq -- "$heading_regex" "$skill_file"; then
    report_failure "$skill_file: missing required section heading '$section_name' ($reason)"
  fi
}

is_within_dir() {
  local path="$1"
  local dir="$2"
  case "$path" in
    "$dir" | "$dir"/*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

check_markdown_links() {
  local file="$1"
  local raw_link target anchor path dir cur_slugs target_slugs
  dir="$(dirname "$file")"
  cur_slugs=""
  while IFS= read -r raw_link; do
    case "$raw_link" in
      http://* | https://* | mailto:*)
        continue
        ;;
    esac
    target="${raw_link%%#*}"
    anchor="${raw_link#"$target"}"
    anchor="${anchor#\#}"
    if [[ -z "$target" ]]; then
      # Anchor-only link into the current document.
      [[ -z "$anchor" ]] && continue
      [[ -z "$cur_slugs" ]] && cur_slugs="$(heading_slugs "$file" | sort -u)"
      if ! printf '%s\n' "$cur_slugs" | grep -xF "$anchor" >/dev/null; then
        report_failure "$file: broken anchor '#$anchor' (no matching heading in $file)"
      fi
      continue
    fi
    if [[ "$target" = /* ]]; then
      path=".$target"
    else
      path="$dir/$target"
    fi
    if [[ ! -e "$path" ]]; then
      report_failure "$file: missing markdown link target '$raw_link' (broken local link)"
      continue
    fi
    if [[ -n "$anchor" && "$target" == *.md ]]; then
      target_slugs="$(heading_slugs "$path" | sort -u)"
      if ! printf '%s\n' "$target_slugs" | grep -xF "$anchor" >/dev/null; then
        report_failure "$file: broken anchor '#$anchor' (no matching heading in $target)"
      fi
    fi
  done < <(extract_markdown_links "$file")
}

# GitHub-compatible anchor slug for a heading text. Lowercases, drops
# punctuation (keeping alphanumerics, underscore, hyphen, space), and
# collapses runs of whitespace into a single hyphen.
heading_slug() {
  local text="$1"
  printf '%s' "$text" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//' \
    | tr -cd 'a-z0-9 _-' \
    | tr -s ' ' '-'
  printf '\n'
}

# Emit one GitHub-compatible slug per line for every Markdown heading in $1.
heading_slugs() {
  local file="$1"
  grep -nE '^#{1,6}[[:space:]]+' "$file" 2>/dev/null \
    | sed -E 's/^[0-9]+:[[:space:]]*#+[[:space:]]*//' \
    | while IFS= read -r heading; do
      heading_slug "$heading"
    done
}

assert_file_contains_literal() {
  local file="$1"
  local literal="$2"
  local reason="$3"
  if ! grep -Fq -- "$literal" "$file"; then
    report_failure "$file: missing expected pattern '$literal' ($reason)"
  fi
}

validate_root_guardrails() {
  local agent_file="AGENTS.md"
  local skill_file=".agents/CORE_CHANGE_CHECKLIST.md"
  local pr_template=".github/pull_request_template.md"

  if [[ ! -f "$agent_file" ]]; then
    report_failure "$agent_file: missing required file 'AGENTS.md' (repo root must define contributor guardrails)"
    return
  fi

  if [[ ! -f "$skill_file" ]]; then
    report_failure "$skill_file: missing required file '.agents/CORE_CHANGE_CHECKLIST.md' (repo root must define the core-change checklist)"
    return
  fi

  if [[ ! -f "$pr_template" ]]; then
    report_failure "$pr_template: missing required file 'pull_request_template.md' (repo must keep the canonical PR template)"
    return
  fi

  assert_file_contains_literal "$agent_file" '## What Transist Is' "root guardrail must define what Transist is"
  assert_file_contains_literal "$agent_file" '## What Transist Is Not' "root guardrail must define what Transist is not"
  assert_file_contains_literal "$agent_file" '## Core Architectural Principles' "root guardrail must define core architectural principles"
  assert_file_contains_literal "$agent_file" '## Extension-First Rule' "root guardrail must define the extension-first rule"
  assert_file_contains_literal "$agent_file" '## What Must Not Be Added To Core' "root guardrail must define what must not be added to the core"
  assert_file_contains_literal "$agent_file" 'Transist is not an agent framework.' "root guardrail must protect the non-agent-framework boundary"
  assert_file_contains_literal "$agent_file" 'Complexity should be pushed to extensions, not the kernel.' "root guardrail must keep complexity out of the kernel"
  assert_file_contains_literal "$agent_file" 'LLMs are untrusted participants.' "root guardrail must treat LLMs as untrusted participants"
  assert_file_contains_literal "$agent_file" 'Auditability and control matter more than convenience shortcuts.' "root guardrail must prefer auditability and control over shortcuts"

  assert_file_contains_literal "$skill_file" '## Problem Statement' "root skill must require a problem statement"
  assert_file_contains_literal "$skill_file" '## Why The Change Belongs In Core' "root skill must require justification for core ownership"
  assert_file_contains_literal "$skill_file" '## Alternatives Outside Core Considered' "root skill must require non-core alternatives"
  assert_file_contains_literal "$skill_file" '## Runtime Impact' "root skill must require runtime impact analysis"
  assert_file_contains_literal "$skill_file" '## Retry And Replay Impact' "root skill must require retry and replay impact analysis"
  assert_file_contains_literal "$skill_file" '## Storage, Artifact, And Evidence Impact' "root skill must require storage, artifact, and evidence impact analysis"
  assert_file_contains_literal "$skill_file" '## Tests Added' "root skill must require tests"
  assert_file_contains_literal "$skill_file" '## Docs Updated' "root skill must require docs updates"
  assert_file_contains_literal "$skill_file" '../docs/developer/architecture/kernel-contract.md' "root skill must point contributors at the kernel contract"
  assert_file_contains_literal "$skill_file" '../docs/public/reference/compatibility-and-stability.md' "root skill must point contributors at compatibility guidance"
  assert_file_contains_literal "$skill_file" '../docs/public/reference/storage-layout.md' "root skill must point contributors at storage guidance"
  assert_file_contains_literal "$skill_file" '../docs/public/reference/companion-server-api.md' "root skill must point contributors at companion-server guidance"

  assert_file_contains_literal "$pr_template" '## Change Classification' "PR template must require change classification"
  assert_file_contains_literal "$pr_template" '## Kernel Contract Impact' "PR template must require kernel contract impact disclosure"
  assert_file_contains_literal "$pr_template" '## Tests' "PR template must require test updates"
  assert_file_contains_literal "$pr_template" '## Docs' "PR template must require docs updates"
  assert_file_contains_literal "$pr_template" '## Backward Compatibility' "PR template must require backward-compatibility disclosure"
}

docs_files=(
  "README.md" "AGENTS.md" ".agents/CORE_CHANGE_CHECKLIST.md"
  "ROADMAP.md" "SUPPORT.md" "SECURITY.md" "MAINTAINERS.md"
  "CODE_OF_CONDUCT.md" "CONTRIBUTING.md" "ARCHITECTURE.md" "VISION.md"
  "THIRD_PARTY_NOTICES.md" "CHANGELOG.md"
  ".github/pull_request_template.md"
  ".github/ISSUE_TEMPLATE/bug_report.yml"
)
while IFS= read -r -d '' file; do
  docs_files+=("$file")
done < <(find docs workflows plugins -name '*.md' -print0)

for md in "${docs_files[@]}"; do
  [[ -f "$md" ]] || continue
  check_markdown_links "$md"
done

# Forbidden onboarding-accuracy phrases. Each maps to a corrected statement in
# the public docs; reintroducing any claim fails docs CI.
scan_docs_for_literal() {
  local literal="$1"
  local reason="$2"
  local md
  for md in "${docs_files[@]}"; do
    [[ -f "$md" ]] || continue
    if grep -Fq -- "$literal" "$md"; then
      report_failure "$md: forbidden pattern '$literal' ($reason)"
    fi
  done
}

scan_docs_for_literal '.transist/plugins/' "plugin packages live under <effective-home>/plugins, not a repo .transist dir"
# shellcheck disable=SC2016
scan_docs_for_literal 'every directory listed in `PATH`' "Linux mounts fixed roots and declared paths, not arbitrary PATH directories"
scan_docs_for_literal 'write_allow_paths entries are canonical create targets and do not need to exist yet' "Linux external write targets must already exist"
scan_docs_for_literal 'External write grants for paths that do not yet exist on disk are silently skipped' "Linux rejects missing external targets"
scan_docs_for_literal 'verifies cryptographic trust before executing anything' "standalone mode does not require trust verification"
scan_docs_for_literal 'both final archives' "the release matrix contains six archives"

assert_file_contains_literal README.md 'transist-sandbox-launcher' "README installation must include the Linux sandbox launcher"
assert_file_contains_literal README.md '](ROADMAP.md)' "README must link to the project roadmap"
assert_file_contains_literal README.md '](docs/public/overview.md)' "README must link to the public documentation overview"
assert_file_contains_literal docs/public/reference/README.md '](known-limitations.md)' "reference index must link to known limitations"
assert_file_contains_literal docs/public/reference/README.md '](reproducibility.md)' "reference index must link to reproducibility guidance"
assert_file_contains_literal docs/public/reference/README.md '](release-artifact-verification.md)' "reference index must link to release artifact verification"
assert_file_contains_literal docs/public/reference/README.md '](platform-distribution-signing.md)' "reference index must link to platform distribution signing"
assert_file_contains_literal docs/public/guides/getting-started/installation-and-local-setup.md 'transist-sandbox-launcher' "installation guide must include the Linux sandbox launcher"
assert_file_contains_literal docs/public/reference/reproducibility.md '-p transist-sandbox-launcher' "Linux rebuild must build the sandbox launcher"
assert_file_contains_literal docs/public/reference/reproducibility.md 'target/release/transist-sandbox-launcher' "Linux provenance and packaging must include the sandbox launcher"
assert_file_contains_literal docs/public/reference/supported-platforms.md 'runner image is not bit-pinned' "macOS runner pinning limitation must remain explicit"
assert_file_contains_literal docs/public/reference/companion-server-api.md 'no built-in authentication or TLS' "companion API must disclose missing transport security"
assert_file_contains_literal docs/public/reference/companion-server-api.md 'trusted reverse proxy' "companion API must require a trusted reverse proxy for non-local exposure"
assert_file_contains_literal docs/public/reference/cli.md 'no built-in authentication or TLS' "server CLI must disclose missing transport security"
assert_file_contains_literal docs/public/reference/storage-layout.md '├── config.yaml' "home layout must include required config.yaml"
assert_file_contains_literal docs/developer/architecture/dependency-direction.md 'transist-trust-digest` owns only canonical hashing primitives' "dependency docs must disclose digest primitive ownership"
assert_file_contains_literal .github/ISSUE_TEMPLATE/bug_report.yml 'Include relevant error output or inspection details. Before submission, remove secrets, tokens, customer data, private paths, and sensitive run artifacts.' "bug reports must warn contributors to redact sensitive output"
assert_file_contains_literal .github/ISSUE_TEMPLATE/bug_report.yml 'Add any screenshots, logs, or notes that help isolate the bug. Before submission, remove secrets, tokens, customer data, private paths, and sensitive run artifacts.' "bug reports must warn contributors to redact sensitive logs and screenshots"

# A mode claim must not pair a development/other mode with disabling bwrap:
# sandboxing is unconditional on Linux.
if rg -n --hidden -g '!.git/**' -g '*.md' -e '--mode development' . \
  | grep -q 'bwrap'; then
  report_failure "docs claim development mode disables the bwrap sandbox (no mode disables sandboxing)"
fi

validate_root_guardrails

# Every link inside a GitHub issue-form template must resolve from the
# template directory (see check_issue_template_links.py).
if ! python3 scripts/ci/check_issue_template_links.py "$ROOT_DIR" >/dev/null 2>&1; then
  report_failure "one or more issue-template links do not resolve (run scripts/ci/check_issue_template_links.py)"
fi

skill_bundle_contains_literal() {
  local bundle_list="$1"
  local metadata_file="$2"
  local literal="$3"
  local file
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if grep -Fq -- "$literal" "$file"; then
      return 0
    fi
  done < "$bundle_list"
  grep -Fq -- "$literal" "$metadata_file" && return 0
  return 1
}

skill_bundle_contains_regex() {
  local bundle_list="$1"
  local metadata_file="$2"
  local regex="$3"
  local file
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if grep -Eq -- "$regex" "$file"; then
      return 0
    fi
  done < "$bundle_list"
  grep -Eq -- "$regex" "$metadata_file" && return 0
  return 1
}

scan_bundle_for_literal() {
  local bundle_list="$1"
  local metadata_file="$2"
  local literal="$3"
  local reason="$4"
  local file
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if grep -Fq -- "$literal" "$file"; then
      report_failure "$file: forbidden pattern '$literal' ($reason)"
    fi
  done < "$bundle_list"
  if grep -Fq -- "$literal" "$metadata_file"; then
    report_failure "$metadata_file: forbidden pattern '$literal' ($reason)"
  fi
}

scan_bundle_for_regex() {
  local bundle_list="$1"
  local metadata_file="$2"
  local regex="$3"
  local reason="$4"
  local file
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if grep -Eq -- "$regex" "$file"; then
      report_failure "$file: forbidden pattern '$regex' ($reason)"
    fi
  done < "$bundle_list"
  if grep -Eq -- "$regex" "$metadata_file"; then
    report_failure "$metadata_file: forbidden pattern '$regex' ($reason)"
  fi
}

assert_bundle_contains_literal() {
  local bundle_list="$1"
  local metadata_file="$2"
  local skill_name="$3"
  local literal="$4"
  local reason="$5"
  if ! skill_bundle_contains_literal "$bundle_list" "$metadata_file" "$literal"; then
    report_failure "$skill_name: missing expected pattern '$literal' ($reason)"
  fi
}

collect_skill_bundle_docs() {
  local skill_dir="$1"
  local skill_file="$2"
  local docs_out="$3"
  local skill_root queue_file visited_file current raw_link link candidate resolved line_no

  skill_root="$(canonical_existing_path "$skill_dir")"
  queue_file="$(make_tmp_file)"
  visited_file="$(make_tmp_file)"

  : > "$docs_out"
  : > "$queue_file"
  : > "$visited_file"
  printf '%s\n' "$skill_file" >> "$queue_file"

  line_no=1
  while :; do
    current="$(sed -n "${line_no}p" "$queue_file")"
    [[ -n "$current" ]] || break
    line_no=$((line_no + 1))
    current="$(canonical_existing_path "$current")"
    if grep -Fqx -- "$current" "$visited_file"; then
      continue
    fi
    printf '%s\n' "$current" >> "$visited_file"
    printf '%s\n' "$current" >> "$docs_out"

    while IFS= read -r raw_link; do
      link="$(normalize_markdown_link "$raw_link" || true)"
      [[ -z "$link" ]] && continue
      candidate="$(resolve_markdown_link "$current" "$link")"
      if [[ ! -e "$candidate" ]]; then
        report_failure "$current: missing linked markdown file '$link' (skill bundle reference is missing)"
        continue
      fi
      if [[ ! -f "$candidate" ]]; then
        report_failure "$current: linked markdown path '$link' is not a file (skill bundle references must target files)"
        continue
      fi
      resolved="$(canonical_existing_path "$candidate")"
      if ! is_within_dir "$resolved" "$skill_root"; then
        report_failure "$current: linked markdown path '$link' escapes the skill bundle (skill references must stay inside '$skill_dir')"
        continue
      fi
      printf '%s\n' "$resolved" >> "$queue_file"
    done < <(extract_markdown_links "$current")
  done
}

validate_skill_semantics() {
  local skill_dir="$1"
  local skill_name="$2"
  local metadata_file="$3"
  local bundle_list="$4"
  local primary_file="$skill_dir/SKILL.md"

  scan_bundle_for_literal "$bundle_list" "$metadata_file" 'nested `execution`' "stale authored workflow terminology"
  scan_bundle_for_literal "$bundle_list" "$metadata_file" '`execution` object' "stale authored workflow shape"
  scan_bundle_for_literal "$bundle_list" "$metadata_file" 'execution.kind: invoke' "stale authored workflow shape"
  scan_bundle_for_literal "$bundle_list" "$metadata_file" 'use the `execution` object' "stale workflow metadata guidance"
  scan_bundle_for_literal "$bundle_list" "$metadata_file" 'transist runs start --flow' "stale pre-start execution lifecycle"
  scan_bundle_for_regex "$bundle_list" "$metadata_file" 'cargo run -- .* run --flow' "stale cargo lifecycle shape"

  case "$skill_name" in
    transist-workflow-authoring)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist.flow.v1' "workflow skill must name the authored schema"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'adapter' "workflow skill must cover invoke fields"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugin' "workflow skill must cover invoke fields"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'action' "workflow skill must cover invoke fields"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'execution_kind' "workflow skill must cover invoke fields"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows check --path' "workflow skill must cover the current lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows install --path' "workflow skill must cover the current lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows prepare' "workflow skill must cover the current lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows run' "workflow skill must cover the current lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist runs start' "workflow skill must cover the current lifecycle"

      if grep -Fq -- '`execution` object' "$metadata_file" ||
        grep -Fq -- 'transist runs start --flow' "$metadata_file" ||
        grep -Eq -- 'cargo run -- .* run --flow' "$metadata_file"; then
        report_failure "$metadata_file: metadata contradicts current flat-field/lifecycle guidance (workflow UI prompt must match check -> install -> start -> run)"
      fi
      ;;
    transist-workflow-setup)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows check --path' "workflow setup must check authored source"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist-admin workflow package' "workflow setup must cover governed package signing"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist workflows install' "workflow setup must cover governed installation"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist.policy_fragment.v1' "workflow setup must cover exact policy suggestions"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist runs start' "workflow setup must keep test dispatch explicit"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'private-key' "workflow setup must protect private-key operations"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugin.yaml' "workflow setup must cover plugin dependency installation"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'The agent must never search for private keys' "workflow setup must prohibit private-key discovery"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'ask whether the user already has a signing key' "workflow setup must ask before offering key generation"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'exact private-key file path' "workflow setup must require an exact existing-key path"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Verification failure: stop installation and signing' "workflow setup must stop on failed verification"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist-admin signature inspect' "workflow setup must inspect signature metadata"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'maximum `50` candidates' "workflow setup must cap plugin discovery results"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Re-run validation against the target home' "workflow setup must revalidate after plugin installation"
      ;;
    transist-plugin-authoring)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist.plugin.v1' "plugin skill must name the manifest schema"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugin.yaml' "plugin skill must cover package layout"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugins install' "plugin skill must cover install lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugins inspect' "plugin skill must cover inspect lifecycle"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'plugins uninstall' "plugin skill must cover uninstall lifecycle"
      ;;
    transist-run-inspection-and-recovery)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'runs show' "run inspection skill must cover run detail inspection"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'runs tail' "run inspection skill must cover event inspection"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'runs resume' "run inspection skill must cover resume"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'runs replay' "run inspection skill must cover replay"
      ;;
    transist-observability-and-metrics)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" '/metrics' "observability skill must cover Prometheus scraping"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist-server' "observability skill must cover the companion server"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" '/api/v1/runs' "observability skill must cover JSON inspection routes"
      ;;
    transist-secrets-and-policy)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'secret_refs' "secrets skill must cover workflow secret references"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'process' "secrets skill must cover adapter differences"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'http' "secrets skill must cover adapter differences"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'mcp' "secrets skill must cover adapter differences"
      ;;
    transist-runtime-setup)
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Ask exactly one decision question per turn' "runtime setup must use one beginner decision per turn"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Before any `transist home init` command, ask whether the user wants' "runtime setup must choose home scope before initialization"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'A **global per-user home**' "runtime setup must offer the global home choice"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'A **specific folder**' "runtime setup must offer the specific-folder home choice"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Never search the whole machine' "runtime setup must bound filesystem discovery"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'Would you like to install your first workflow now?' "runtime setup must offer the first-workflow handoff"
      assert_bundle_contains_literal "$bundle_list" "$metadata_file" "$primary_file" 'transist-workflow-setup' "runtime setup must hand workflow installation to its dedicated skill"
      ;;
  esac
}

validate_skill_dir() {
  local skill_dir="$1"
  local skill_name skill_file metadata_file frontmatter_name frontmatter_description
  local display_name short_description default_prompt docs_out linked_reference_count raw_link link

  skill_name="$(basename "$skill_dir")"
  skill_file="$skill_dir/SKILL.md"
  metadata_file="$skill_dir/agents/openai.yaml"
  docs_out="$(make_tmp_file)"

  if [[ ! -f "$skill_file" ]]; then
    report_failure "$skill_file: missing required file 'SKILL.md' (each skill directory must include a skill definition)"
    return
  fi

  frontmatter_name="$(extract_frontmatter_field "$skill_file" "name")"
  if [[ -z "$frontmatter_name" ]]; then
    report_failure "$skill_file: missing expected pattern 'name:' (frontmatter must declare the skill name)"
  elif [[ "$frontmatter_name" != "$skill_name" ]]; then
    report_failure "$skill_file: expected frontmatter name '$skill_name' but found '$frontmatter_name' (skill metadata must match the directory name)"
  fi

  frontmatter_description="$(extract_frontmatter_field "$skill_file" "description")"
  if [[ -z "$frontmatter_description" ]]; then
    report_failure "$skill_file: missing expected pattern 'description:' (frontmatter must declare the skill description)"
  fi

  assert_skill_heading_present "$skill_file" "Default Workflow" "every SKILL.md must include Default Workflow, Hard Rules, Source Priority, and References"
  assert_skill_heading_present "$skill_file" "Hard Rules" "every SKILL.md must include Default Workflow, Hard Rules, Source Priority, and References"
  assert_skill_heading_present "$skill_file" "Source Priority" "every SKILL.md must include Default Workflow, Hard Rules, Source Priority, and References"
  assert_skill_heading_present "$skill_file" "References" "every SKILL.md must include Default Workflow, Hard Rules, Source Priority, and References"

  if [[ ! -f "$metadata_file" ]]; then
    report_failure "$metadata_file: missing required file 'agents/openai.yaml' (each skill directory must include UI metadata)"
    return
  fi

  display_name="$(extract_interface_field "$metadata_file" "display_name")"
  [[ -n "$display_name" ]] || report_failure "$metadata_file: missing expected pattern 'display_name:' (skill UI metadata must define display_name)"

  short_description="$(extract_interface_field "$metadata_file" "short_description")"
  [[ -n "$short_description" ]] || report_failure "$metadata_file: missing expected pattern 'short_description:' (skill UI metadata must define short_description)"

  default_prompt="$(extract_interface_field "$metadata_file" "default_prompt")"
  [[ -n "$default_prompt" ]] || report_failure "$metadata_file: missing expected pattern 'default_prompt:' (skill UI metadata must define default_prompt)"

  linked_reference_count=0
  while IFS= read -r raw_link; do
    link="$(normalize_markdown_link "$raw_link" || true)"
    [[ -z "$link" ]] && continue
    case "$link" in
      references/*.md)
        linked_reference_count=$((linked_reference_count + 1))
        ;;
    esac
  done < <(extract_markdown_links "$skill_file")
  if [[ "$linked_reference_count" -eq 0 ]]; then
    report_failure "$skill_file: missing expected pattern 'references/*.md' (SKILL.md must link to at least one skill-local reference document)"
  fi

  collect_skill_bundle_docs "$skill_dir" "$skill_file" "$docs_out"
  validate_skill_semantics "$skill_dir" "$skill_name" "$metadata_file" "$docs_out"
}

validate_removed_install_options() {
  local findings
  findings="$(make_tmp_file)"
  python3 - "$findings" <<'PY'
from pathlib import Path
import re
import sys

root = Path.cwd()
output = Path(sys.argv[1])
roots = (root / "docs", root / ".agents", root / "scripts", root / "README.md", root / "CONTRIBUTING.md")
suffixes = {".md", ".sh", ".py", ".yaml", ".yml", ".rs"}
for item in roots:
    paths = [item] if item.is_file() else item.rglob("*")
    for path in paths:
        if not path.is_file() or path.suffix not in suffixes:
            continue
        try:
            lines = path.read_text().splitlines()
        except UnicodeDecodeError:
            continue
        for index, line in enumerate(lines):
            if not re.search(r"(?:transist|\$TRANSIST_BIN|\$TRANSIST)\s+workflows\s+install\b", line):
                continue
            command = line
            cursor = index
            while command.rstrip().endswith("\\") and cursor + 1 < len(lines):
                cursor += 1
                command += " " + lines[cursor]
            obsolete = re.search(r"(?:--input(?:s-file)?|--local-only|--signature|--policy-output)\b", command)
            if obsolete:
                print(f"{path.relative_to(root)}:{index + 1}: obsolete workflow install option {obsolete.group(0)}", file=output)
PY
  if [[ -s "$findings" ]]; then
    while IFS= read -r finding; do
      report_failure "$finding"
    done < "$findings"
  fi
}

report_markdown_matches() {
  local root="$1"
  local regex="$2"
  local reason="$3"
  local findings
  findings="$(make_tmp_file)"

  grep -R -n -E --include='*.md' -- "$regex" "$root" > "$findings" || true
  while IFS= read -r finding; do
    [[ -n "$finding" ]] || continue
    report_failure "$finding ($reason)"
  done < "$findings"
}

validate_path_semantics_docs() {
  local reference="docs/public/reference/path-and-resource-semantics.md"
  local file

  if [[ ! -f "$reference" ]]; then
    report_failure "$reference: missing canonical path and resource semantics reference"
    return
  fi

  assert_file_contains_literal "$reference" '`artifact://` and `evidence://` are not Transist schemes.' "canonical reference must reject nonexistent artifact/evidence schemes"
  assert_file_contains_literal "$reference" '`run://artifacts`' "canonical reference must describe run artifact capability"
  assert_file_contains_literal "$reference" '`file:///absolute/path`' "canonical reference must describe external file capability"
  assert_file_contains_literal "$reference" '`repo://path`' "canonical reference must describe action-specific repo paths"

  for file in \
    docs/public/reference/workflow-schema.md \
    docs/public/reference/plugin-manifest.md \
    docs/public/reference/tep.md \
    docs/public/handbook/02-everyday-usage/03-invoke-steps.md; do
    assert_file_contains_literal "$file" 'path-and-resource-semantics.md' "path-sensitive documentation must link the canonical reference"
  done

  for file in \
    .agents/skills/transist-workflow-authoring/SKILL.md \
    .agents/skills/transist-plugin-authoring/SKILL.md \
    .agents/skills/transist-secrets-and-policy/SKILL.md \
    .agents/skills/transist-workflow-setup/SKILL.md; do
    assert_file_contains_literal "$file" 'path-and-resource-semantics.md' "path-sensitive skill must cite the canonical reference"
  done

  report_markdown_matches docs/public 'workspace://[^/]' "workspace://relative is not a universal action-input URI"
  report_markdown_matches docs/public 'run://[^`[:space:]]*.*run-root|run-root.*run://' "arbitrary run:// paths are not durable run-root storage"
  report_markdown_matches docs 'current runtime still needs the authored workflow source directory|current shell runtime still needs the authored workflow source directory|current dispatch still needs the authored recipe source directory' "snapshot-backed runs dispatch captured source assets"

  while IFS= read -r file; do
    [[ -n "$file" ]] || continue
    [[ "$file" == "$reference" ]] || report_failure "$file: artifact:// and evidence:// are unsupported outside the canonical reference"
  done < <(grep -R -l -E --include='*.md' -- '`artifact://|evidence://' docs || true)
}

# Retired skills leave no tracked directory in a clean checkout. Select only
# actual skill definitions so an empty local directory cannot make docs CI fail.
while IFS= read -r skill_dir; do
  [[ -n "$skill_dir" ]] || continue
  validate_skill_dir "$skill_dir"
done < <(find .agents/skills -mindepth 2 -maxdepth 2 -type f -name SKILL.md -exec dirname {} \; | sort)

# Installation persists a package or standalone definition only. Inputs,
# authority posture, and policy output belong to the subsequent check and
# authorize commands; reject retired install command forms in CI and docs.
validate_removed_install_options
validate_path_semantics_docs

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "Docs and skill validation passed."
