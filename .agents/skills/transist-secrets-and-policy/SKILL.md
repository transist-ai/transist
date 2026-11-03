---
name: transist-secrets-and-policy
description: Safely author and troubleshoot current Transist workflows and plugins that rely on invoke-step `with.secret_refs`, adapter-specific delivery, runtime policy, and non-persistence boundaries across `process`, `http`, and `mcp`.
---

# Transist Secrets and Policy

Help the user author or debug secret-aware workflows and plugins without leaking resolved secrets into persisted state or enabling remote delivery more broadly than needed.

Use `transist ...` commands by default.
If working from a source checkout instead of an installed CLI, substitute `cargo run -p transist-cli -- ...`.

## Default Workflow

1. Inspect the workflow step, plugin action, and effective policy surface:
    - invoke-step `with.secret_refs`
    - adapter kind: `process`, `http`, or `mcp`
    - declared `workspace.path` and `workspace.access` when filesystem or
      capability remediation is involved
    - the effective home from `transist home get` when policy location may be unclear
    - `config.yaml` under that effective home when remote secret delivery may matter
2. Confirm what the workflow is asking for:
    - which logical secret refs must resolve
    - whether the action needs local-only or remote delivery
    - whether the plugin action is declared `provider_kind: local` or `external`
    - which bindings or secret inputs the plugin expects
3. Explain the adapter posture:
   - `process` for direct local secret consumption
   - `http` and `mcp` only when remote delivery policy allows it
4. Troubleshoot by separating:
   - secret resolution failures
   - remote secret delivery denials
   - plugin binding mistakes
   - unrelated workflow or runtime failures
5. Before creating a run, use `transist workflows check <id>@<version> --inputs-file <inputs.json> --output readiness.json` to aggregate schema, workspace, policy, secret-availability, delivery, prerequisite, and plugin-health gaps without creating a run. Confirm its `run_created: false` result. Interactive results group blockers by concern; with `--format json`, use `diagnostics.groups` and `diagnostics.authorization.blocking_codes` to distinguish missing authority from input, secret, or delivery failures. Only when exact authority is missing and no other check fails, use `transist workflows authorize <id>@<version>` with the same repeatable `--input key=value` arguments or `--inputs-file <inputs.json>`, then rerun check. A blocked authorization writes no policy. Once preparation creates a run, use `transist runs capabilities <run-id> --format yaml`, `transist runs preflight <run-id>`, and `transist runs show <run-id>` without printing raw values.
6. Verify persistence boundaries and run inspection signals without printing or persisting raw secret values.
7. Finish with the root cause, safe policy recommendation, and any workflow or plugin changes required.

## Hard Rules

- Installed `check` and authorization validate without plugin probes or secret
  value resolution. Passing validation leaves readiness `not_ready`; use
  `--live` or `--output` for live readiness. Secret availability and delivery
  policy are checked every time. Required warnings/incomplete checks block success.
- Already-authorized calls perform no probes or writes. Applying revalidates
  current inputs/resources/policy and compares the exact reviewed fragment and
  destination; drift aborts without writes and requires another review.

- Resolved secrets must never be persisted.
- Readiness never resolves, emits, hashes, or persists secret values. Missing
  secrets and denied delivery are pre-dispatch findings with `run_created: false`.
- Workflow-owned prompt files and `source://` helpers are not secret references: fresh run birth may retain their raw bytes in private `workflow_assets/` storage. Never put credentials or tokens in those files.
- Workspace and externally granted files are mutable host state outside
  artifact/evidence promotion, redaction, and rollback. Never use them as a
  secret-bearing cross-step channel.
- The kernel's final persistence pass redacts every exact raw value and padded or unpadded standard and URL-safe base64 variant from durable records.
- Adapter artifact/evidence paths are private per-invocation staging paths; the kernel scans names and bytes before promotion, redacts UTF-8, rejects secret-bearing binary files, rejects symlinks/escapes/non-regular paths, and recomputes hashes.
- `process` is the most direct local secret-consumption path.
- New governed homes default remote-secret delivery to `process: false`,
  `http: false`, `mcp: false`. Explicit existing standalone homes retain their
  configured values.
- New homes are governed and use `policy.profile: allowlist`; standalone or
  development mode must be explicitly selected to use permissive policy.
- Raw secret delivery to `http` and `mcp` is policy-gated.
- `--local-only` is a separate run-time locality control. `prepare`, `workflows run`, `runs preflight`, `runs retry`, `runs resume`, `runs replay`, and `runs cleanup` accept it and deny actions whose effective plugin-action `provider_kind` is `external` before dispatch.
- `provider_kind` is declared by the plugin manifest; it is not inferred from adapter kind.
- The bundled `shell` plugin declares `default_provider_kind: local`, so
  `shell/exec` is allowed under `--local-only` and the runtime caps the shell
  process at `network: loopback` (no non-loopback connections). Declare `local`
  explicitly on any plugin that must stay local-only.
- Enable remote secret delivery only for the adapter that actually needs it.
- Prefer version-pinned `policy.allow.plugin_grants` for plugin actions. A
  matching entry covers that invocation's adapter, plugin, action, and provider
  capabilities; keep secrets, filesystem, network, and approval authority as
  exact `policy.allow.capabilities` grants.
- A declared workspace requests one exact canonical filesystem resource; it does
  not grant it. `full` may request mutations and `read_only` requests only
  list/read. Do not grant workflow source, CLI CWD, a worktree, or a guessed
  parent directory as a substitute. Secondary repository context is exact
  input-bound read-only external access.
- Persisted workspace authority survives preflight, dispatch, retry, resume,
  cleanup, and replay. Changing `repo_path` requires a fresh run; changing
  CWD cannot repair a declared-workspace denial. Legacy runs without workspace
  metadata retain the CLI-directory fallback.
- Troubleshooting must distinguish secret resolution failure from remote secret delivery denial.
- Troubleshooting must also distinguish remote secret delivery denial from local-only locality denial.
- Troubleshooting must also distinguish local-only locality denial from trusted-execution denial; inspect `state.json.local_only_mode`, `state.json.trust_decision`, and `provenance.json` instead of conflating them.
- Keep logical refs in workflow authoring under invoke-step `with.secret_refs` and keep narrow secret bindings in plugin authoring.
- Never place secret-dependent executable checks in prerequisite `commands` or
  binary version probes. Those legacy fields never execute; use an explicit
  invoke action so normal secret policy and persistence boundaries apply.
- Current binding families are adapter-specific: `process` uses `env` or `temp_file`, `http` uses `header` or `body_field`, and `mcp` uses `env` or `arg`.
- HTTP `ca_cert` configuration only adds a custom trust root; it never disables TLS chain, expiry, hostname, or signature validation.
- HTTP network policy is host-enforced for handshake, health, invoke, redirect,
  and cancellation. `none` makes no connection, `loopback` requires every DNS
  result to be loopback, honors non-empty authored host/port narrowing, and
  restricted modes disable system/environment proxies. Cross-authority
  redirects are denied outside `allowlist`; allowlisted redirects strip action
  secret headers and nested APP invoke `input` body fields.
- Under `--local-only`, HTTP requests have a non-widenable loopback ceiling;
  an authored `none` policy remains stricter.
- Do not suggest copying raw secret values into workflow YAML, plugin manifests,
  workspace or external files, artifacts, evidence, events, or run state.
- Allowed external-provider calls in governed runs are summarized in `provenance.json.external_transfer_summary`; per-call detail lives in additive `transist.external_transfer.v1` evidence records.

## Source Priority

Use facts in this order when guidance conflicts:

1. `docs/public/guides/trust/secrets-and-runtime-policy-in-practice.md`
2. `docs/public/handbook/04-trust-and-runtime-behavior/*`
3. `docs/public/handbook/03-inspection-and-recovery/05-troubleshooting.md`
4. `docs/public/reference/workflow-schema.md`
5. `docs/public/reference/path-and-resource-semantics.md`
6. `docs/public/reference/plugin-manifest.md`
7. `crates/transist-adapters`
8. `crates/transist-core`

## References

- Secret resolution and persistence boundaries: [secret-resolution-and-persistence-boundaries.md](references/secret-resolution-and-persistence-boundaries.md)
- Adapter policy matrix: [adapter-policy-matrix.md](references/adapter-policy-matrix.md)
- Troubleshooting secret and policy failures: [troubleshooting-secret-and-policy-failures.md](references/troubleshooting-secret-and-policy-failures.md)
