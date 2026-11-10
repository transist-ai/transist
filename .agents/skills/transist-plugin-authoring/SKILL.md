---
name: transist-plugin-authoring
description: Create, update, review, or repair current-state Transist plugin packages. Use when authoring `transist.plugin.v1` packages with `plugin.yaml`, action schemas, bindings, install and inspection lifecycle, and workflow-level validation; use the advanced adapter appendix only when package behavior is blocked by adapter details.
metadata:
  owner: transist
  domain: infra
  triggers: transist plugin, plugin.yaml, package.files, plugin digest, plugin lifecycle
  priority: high
  token_budget: medium
---

# Transist Plugin Authoring

## When to Use This Skill

Help the user create or repair a current `transist.plugin.v1` package and validate it through the public plugin lifecycle and a real workflow, while keeping maintainer-level adapter work isolated unless it is truly required.

## Core Content

Use `transist ...` commands by default.
If working from a source checkout instead of an installed CLI, substitute `cargo run -p transist-cli -- ...`.

### Repository-Scoped Version Policy

- In this repository's `plugins/`, every plugin manifest and reported runtime version
  remains exactly `0.1.0`, including after selected-byte or `package.files`
  changes.
- In this repository's `workflows/` and `examples/`, maintained root flows and modules
  keep release `version: 0.1.0` and all plugin pins stay exactly `0.1.0`.
  `transist.module.v1` remains the schema identifier, not the release version.
  Runtime/admin crates use `0.1.1`; arbitrary/negative test fixtures
  are exempt from the maintained catalog policy.
- The current authoring/persistence contract requires a fresh runtime home,
  with no legacy reader, version alias or in-place migration. See
  `docs/public/reference/compatibility-and-stability.md` for adoption and rollback.
- Do not recommend bundled catalog version bumps. This is not a global
  Transist rule: elsewhere, follow the target repository's version policy and
  bump only when it requires it. Apply this policy before generic version-bump
  advice in references.
- Changed selected bytes or file lists require a new digest and corresponding
  signature, not an assumed equivalent `0.1.0` install. Record source revision,
  `plugin_dir_sha256`, and signature; an old signature does not cover new bytes.
- Validate changed packages in a fresh home and promote a reviewed signed bundle
  to a fresh target home. Preserve the old home and reviewed signed bundle for
  existing runs/pins; rollback selects that old home/bundle. Never silently
  overwrite or edit installed same-version payloads, uninstall pinned packages,
  or delete runs to clear an install collision. Fresh homes still require
  approved trust roots and policy. Fresh explicit authorization for private-key
  use and governed trust verification remain required.

## Default Workflow

1. Inspect the current workspace before editing:
    - the source plugin directory or installed package
    - `plugin.yaml`
    - referenced schemas and entrypoint assets
    - the workflow or run that is expected to exercise the plugin
2. Clarify the package contract:
    - adapter kind: `process`, `http`, or `mcp`
    - plugin id and version
    - action names, input schema, output schema, bindings, secret inputs, and declared `provider_kind`
    - static prerequisites (`binaries`, `env`, `paths`); legacy executable prerequisite fields fail with `legacy_executable_prerequisite_unsupported`
    - any file path inputs or outputs and which schemes they support, such as `run://...`, `repo://...`, or plugin-local schemes like shell `source://...`
3. Update the plugin package itself:
    - `plugin.yaml`
    - referenced input and output schemas
    - entrypoint assets and package-local support files
    - README environment documentation that lists every inherited or
      child-process variable, classifies it as required, optional, or
      plugin-managed, gives its purpose and safe/default value, and states
      whether runtime policy must allow, inject, or redact it
4. Verify the plugin lifecycle:
    - `transist plugins install --path <plugin-dir>/plugin.yaml` (directory paths remain accepted as a compatibility input)
    - `transist plugins list`
    - `transist plugins inspect <plugin-id>`
    - governed homes default to `<plugin-dir>.sig`; use `--signature <path>` only when the publisher stores a detached sidecar elsewhere, then import the approving public key with `transist trust import <trust-root-file>` on the runtime side
    - `transist plugins uninstall <plugin-id> --version <version>` only for reviewed cleanup of unpinned packages; use a fresh home for changed same-version bytes
5. Validate the plugin through a real workflow:
    - `transist workflows check --path <flow.yaml>` for authored checks
    - install governed workflows with `transist workflows install --package <package-directory>` or unsigned standalone/development source with `--path <flow.yaml>`
    - `transist workflows check <id@version> --inputs-file <inputs.json>` for probe-free validation; add `--live` or `--output readiness.json` for live readiness
    - `transist workflows authorize <id>@<version>` with the same repeatable `--input key=value` arguments or `--inputs-file <inputs.json>` only when validation contains exact missing authority and no other blockers, then rerun check
    - `transist workflows check <id@version> --inputs-file <inputs.json> --live` when plugin readiness must be confirmed, then `transist workflows run <id@version> [--run-ref <run-ref>]` for the normal happy path
    - `transist workflows prepare <id@version> [--run-ref <run-ref>]` plus `transist runs start <run-id>` when you need staged inspection before dispatch
    - `transist runs show <run-id>`
    - when the workflow uses package-local modules or detached helpers, exercise the
      changed action through that module boundary; schema validation and
      preflight alone do not prove governed asset staging or sandbox file I/O
6. Report token usage when the plugin wraps a model-backed tool:
   - populate `usage` on the invoke result using the canonical keys
   - extract usage with the pattern that matches the backend (streaming JSON events, session-state files, rollout files, or stdout regex fallback)
   - do not rely on sibling plugin accumulators when shelling out to a raw CLI; extract usage in-plugin
   - keep extraction best-effort so it never fails the primary action
   - emit a `usage.json` artifact hint when usage is available
7. Inspect usage and metrics only when the adapter or plugin reports them.
8. When an action is declared `provider_kind: external`, expect governed runs to record additive `transist.external_transfer.v1` evidence plus a run-level `external_transfer_summary` in `provenance.json` for allowed calls.
9. Use the advanced adapter appendix only if package behavior cannot be explained without adapter or router details.
10. Finish with files touched, commands run, observed results, and blockers.

## Hard Rules

- Treat `transist.plugin.v1` as the manifest contract.
- Every package must ship `plugin.yaml` and a non-empty `package.files` list
  containing its complete runtime payload: referenced schemas, entrypoint
  assets, local modules, dynamic assets, certificates, and local `$ref` targets.
  `plugin.yaml` is implicit; globs, negation, and ignore files are not supported.
- The selected payload is exactly `plugin.yaml` plus `package.files`. Unselected
  files are not digested, signed, installed, or available as runtime
  dependencies. Changing `package.files` requires a new digest and corresponding
  signature for governed delivery; change the plugin version only when the
  repository policy requires it, never in the fixed-version bundled catalog above.
- Action input and output schemas use JSON Schema Draft 2020-12 and must both
  compile during install. Relative `$ref` targets must remain inside the
  canonical package root; remote, absolute, parent-traversing, and
  symlink-escaping references are rejected without network access.
- Adapters validate materialized input before transport dispatch and successful
  output before converting hints or returning result data. Invalid instances
  produce `validation_error` with paths only.
- Keep action names, schema files, adapter kind, and binding names deliberate because workflows depend on them.
- Document every environment variable the plugin reads or forwards in its
   README so operators can configure policy before running. For each variable,
  list its name, purpose, required/optional/plugin-managed classification,
  default or safe example value, whether it is inherited, read, forwarded, or
  created for a child process, and whether policy must allow, inject, or redact
  it. Include operational variables such as temporary-directory overrides
  (`TMPDIR`) as well as credentials. Put unconditional startup requirements in
  `plugin.yaml` prerequisites as well rather than relying on prose alone.
- Declare `provider_kind` deliberately on every action, or set `default_provider_kind` at the manifest level. When both are absent, the runtime treats the action as `external` (fail-safe).
- `mcp` actions must declare a non-empty `bindings.tool_name`, and prompt-aware or skills-aware actions must declare the corresponding workflow-level binding fields explicitly.
- If an action accepts file paths, document the supported path schemes explicitly. Treat `run://...` and `repo://...` as plugin conventions, and describe plugin-local schemes such as shell `source://...` as plugin-specific behavior rather than generic workflow syntax.
- For every accepted URI-shaped action input, document the resolver, required
  companion inputs, lifecycle, and path boundary. `repo://` is not a runtime
  capability scheme. The bundled shell plugin requires `repo_path` and maps
  `repo://path` to `<repo_path>/.transist/path`; a required path input must
  bind to the declared workspace and cannot select alternate authority.
- State persistence separately from path support: process/MCP `run://` paths
  resolve within invocation-private staging, not the durable run directory, and
  cannot transfer files to another invocation. Maintained delivery flows use
  bounded root managed documents for intermediate files/trees outside Git.
  Process action path fields must accept ordinary absolute paths outside the
  workspace for `{$document: name, access: read|write|update}` bindings; HTTP/MCP
  cannot use these bindings. Inspect the exact action contract rather than
  substituting `repo://` files or opaque shell plumbing. Keep intended source
  and OpenSpec deliverables in the workspace. Other workflows may explicitly
  choose mutable workspace files, without automatic capture, redaction or rollback.
- Fresh run birth may capture resolved workflow `prompt_file` content and referenced `source://` bytes under private `workflow_assets/` storage. This does not make `source://` a generic plugin contract: plugins must still document the exact scheme and path semantics they consume, and prompt-aware actions must declare the `transist_prompt_v1` binding so the runtime can inject captured prompt content.
- Workflow-owned detached assets are separate from plugin package files.
  Current snapshot-backed runs use `asset_lock_version: 1` with
  `workflow_asset_snapshot: true` and persisted asset blobs for dispatch and
  re-entry. Historical version-1 records without the marker retain authored-source
  behavior; historical version-2 records still require snapshots. Missing or
  corrupt required captures fail closed without authored-source fallback.
- `source://` assets in a nested module resolve from the declaring module's
  source root. Do not package or recommend helper paths that rely on symlinks,
  absolute paths, or parent traversal: governed source reads intentionally
  reject symlink traversal. A nested helper must be a regular contained file
  beneath the module's own source directory.
- Process and MCP plugins must declare provider temporary directories as complete
  `entrypoint.env` values of `runtime://temp` or
  `runtime://temp/<relative-path>`. Each segment is limited to
  `[A-Za-z0-9][A-Za-z0-9._-]*`; never embed the token in another value. The
   runtime creates these directories before startup and injects `TMPDIR` last.
   On macOS BSD `mktemp`, use an explicit template beneath `$TMPDIR`. Do not
   hardcode `/tmp`, use host-global temporary
   folders, override `TMPDIR`, or use runtime-temporary storage for durable
    provider state. A temporary `HOME`, configuration, or data directory
    redirects the provider's normal host-login lookup; model credentials need a
    declared secret binding and applicable runtime policy. This is not complete
    same-user host read isolation on macOS, so plugins must not probe alternate
     host credential paths. Use typed direct outputs for later non-secret JSON
     data or managed documents through supported process action path fields.
- Validate a plugin by installing it and using it in a real workflow, not by manifest inspection alone.
- Use `transist plugins install`, `transist plugins list`, `transist plugins inspect`, and `transist plugins uninstall` as the public lifecycle.
- When a plugin package is expected to participate in trusted execution, treat `plugin_dir_sha256` and its detached signature as part of the package contract. `<plugin-dir>.sig` is the default; `plugins install --signature <path>` verifies an explicit source and persists the verified bytes with the installed package.
- Governed homes automatically require trusted plugin verification at install and
  every governed execution entry point; standalone/development bypass is only
  valid when explicitly selected by the operator.
- Trusted-mode result handling rechecks adapter-reported artifact and evidence checksums before descriptor metadata is persisted, so reported files must stay byte-stable between the adapter result and runtime persistence.
- Run-level asset locks pin the installed plugin package for prepared runs. Changing plugin bytes after `prepare` is a re-entry compatibility break, not a silent update.
- Do not place secrets in plugin-local workflow helper files or prompt files. Fresh run birth can retain their raw bytes in private run storage, even though inspection and export surfaces omit them.
- Keep plugin-facing behavior in APP terms and adapter-facing behavior in TEP terms.
- Use APP handshake and advertised APP health for plugin readiness. Do not add
  executable prerequisite commands or binary version probes; executable checks
  belong in explicit plugin actions invoked through the adapter boundary.
- `process`, `http`, and `mcp` are the supported adapter kinds; do not invent new ones.
- The bundled `shell` plugin (`0.1.0`, `default_provider_kind: local`) is the
  reference local `process` package: it declares locality at the manifest level,
  ships a detached `plugins/shell.sig` for trusted install, and promotes
  `$TRANSIST_EVIDENCE_DIR` files written by the command as evidence hints.
  Mirror that shape for new local process plugins.
- Process is the default local path for direct secret consumption; by default raw secret delivery is allowed for `process` and denied for `http` and `mcp` until policy changes it.
- HTTP adapters enforce effective network policy at the host boundary for
  handshake, health, invoke, redirect, and cancellation requests. Restricted
  modes disable environment/system proxies; cross-authority redirects are
  denied unless the allowlist explicitly covers the destination.
- Never assume usage or token metrics exist unless the adapter or plugin reports `usage`.
- When a plugin wraps a model-backed tool, populating `usage` is part of conformance, not optional polish; see [usage-reporting.md](references/usage-reporting.md).
- Adapter or router edits are advanced follow-up work, not the default solution path for package authoring.

## Source Priority

Use facts in this order when guidance conflicts:

1. Actual installed exact `plugin.yaml`, selected action schemas/bindings and
   checked-out compiler/adapter contracts; record catalog-only unresolved pins.
2. `docs/public/reference/compatibility-and-stability.md`
3. `docs/public/reference/plugin-manifest.md`
4. `docs/public/reference/path-and-resource-semantics.md`
5. `docs/public/reference/plugin-support.md`
6. `docs/public/reference/app.md`
7. `docs/public/reference/tep.md`
8. `docs/public/reference/cli.md`
9. `docs/public/guides/plugins/*`
10. `crates/transist-plugins`
11. `crates/transist-adapters`
12. `src/architecture/mod.rs`

## Examples

### Good Example

After changing `plugins/shell/plugin.yaml` or its selected
payload, retain manifest and reported version `0.1.0` and demo pins `0.1.0`.
Record the source revision and new digest, obtain fresh signing authorization,
and validate the reviewed signed package through a workflow in a fresh home.
Promote the reviewed signed bundle to a fresh target home; keep the old
home/bundle for pinned runs and rollback.

### Bad Example

Bump that manifest, reported version, or demo pins to `0.1.1` because
`package.files` changed, reuse its old signature, or replace the installed
`0.1.0` payload beneath a prepared run. Equal version strings do not preserve
content identity or run-lock compatibility.

## Boundaries

- **Always**: Follow repository-scoped version policy and identify changed bytes
  by revision, digest, and signature while retaining old pinned bundles/homes.
- **Never**: Apply fixed `0.1.0` globally, rewrite installed pinned payloads,
  delete runs for reinstalls, or weaken signing authorization and trust checks.

## References

- Plugin package lifecycle: [plugin-package-lifecycle.md](references/plugin-package-lifecycle.md)
- Plugin manifest and bindings: [plugin-manifest-and-bindings.md](references/plugin-manifest-and-bindings.md)
- Conformance checklist: [conformance-checklist.md](references/conformance-checklist.md)
- Usage and token reporting: [usage-reporting.md](references/usage-reporting.md)
- APP and TEP essentials: [tep-v1.md](references/tep-v1.md)
- Advanced adapter appendix: [adapter-authoring-contract.md](references/adapter-authoring-contract.md)
