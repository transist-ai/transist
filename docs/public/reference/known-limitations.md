# Known Limitations

This page tracks known constraints that matter to technical users, support teams, maintainers, and authorized collaborators.

## Run Provenance and Evidence

- **Provider locality is declaration-based**: `provider_kind` is taken from the plugin manifest (`provider_kind` or `default_provider_kind`), not inferred from adapter behavior. A misdeclared plugin can therefore misstate locality until signing/review processes catch it.
- **External-transfer bytes are best effort**: `transist.external_transfer.v1` currently records `bytes_sent: null` because the runtime does not instrument every adapter transport at byte-count granularity.
- **No DLP-grade redaction claim**: external-transfer evidence is additive audit data only. `redaction_applied` is fixed to `false` in v1 and must not be presented as a legal-compliance guarantee.
- **Default-mode runs do not record transfer ledgers**: external-transfer evidence is written only when governance recording is active (`--trusted-execution` or `--local-only`). A plain default run therefore reports `external_transfer_summary.count = 0` and `external_transfers = []` even if the workflow called an external provider.
- **Parent-run transfer summary is not tree-aggregated**: `external_transfer_summary` is run-local. Parent runs do not currently aggregate child subflow transfer counts into the root provenance record; inspect each child run's provenance separately when a workflow fans out into subflows.
- **Approver identity is self-asserted**: `approval_summary[].actor` records the resolved operator identity, but the runtime does not verify it with OIDC, SAML, RBAC, or separation-of-duties controls. Treat `actor_source` on `transist.gate_decision.v1` as audit context, not authentication.

## Governance and Export Scope

- **Readiness is prospective, not durable run evidence**: `workflows check`
  binds supplied inputs without persisting them and always reports
  `run_created: false`. After `prepare` or `run` creates a run, use `runs show`,
  `runs preflight`, and `runs capabilities` rather than recomputing historical
  diagnostics from current source or policy.
- **Authorization is deliberately explicit**: Transist never grants missing
  workflow authority during installation or run creation. An operator must
  review and apply the exact `workflows authorize` plan, then recheck before a
  run can be born.
- **Trust-root removal is local lifecycle only**: `transist trust remove <key-id> --yes` affects future verification on that machine, but it is not historical revocation and does not rewrite prior run records.
- **Plugin packaging remains directory-based in v1**: plugins install/sign/verify accept a plugin directory or `plugin.yaml` path only. Archive packaging is still out of scope.
- **Snapshot export is local and read-only**: `transist runs snapshot export` builds a validated snapshot from local inspection and local metrics state only. It does not enable central sync, mutate runs, or expose a new HTTP API.

## Filesystem Policy

- **`workspace_access.mode: custom` is unsupported**: The `custom` mode and its workspace-relative `read_allow_paths` / `write_allow_paths` allow-lists were never enforced precisely by either sandbox backend. The runtime rejects `custom` before dispatch with `filesystem_custom_mode_unsupported` on both Linux and macOS. Use `read_only` for workspace write protection and the top-level `read_allow_paths` / `write_allow_paths` for external path grants.
- **macOS is not complete same-user host read isolation**: The macOS `sandbox-exec` backend protects the Transist home, shared `.transist-staging` parent, and general temporary parent, reopening only the current plugin package, workspace, declared external paths, current invocation staging root, and exact runtime-temporary root. External grants cannot reopen a protected parent. Unrelated files readable by the same user remain readable because `allow default` is required for `fork` safety with `CoreServices.framework`. On Linux, both read and write boundaries are enforced by `bwrap`.
- **Builtin default `full` has no external grants**: When no explicit filesystem policy is declared at any level, the runtime uses the builtin `full` workspace policy with no external grants. Local `process` and `mcp` invocations still use their platform sandbox; declare an explicit policy only when additional external paths or `read_only` workspace behavior is needed.
- **Linux external write grants require existing paths**: `bwrap` can only bind-mount paths that exist on disk. A `write_allow_paths` (or `read_allow_paths`) entry for a file or directory that does not yet exist is rejected before dispatch rather than silently skipped, because a silently-omitted mount would let a grant claim coverage for a path bwrap never binds. macOS grants the path regardless.

## Platform Support

- Official release targets are `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, and `aarch64-unknown-linux-musl`.
- Required workspace tests run on the Linux glibc PR gate and are repeated by
  the local/default- and all-feature release readiness gates on Linux
  release-staging hosts. Mainline validation builds and smokes Linux glibc,
  Alpine/musl, and macOS Apple Silicon release targets, but does not run the
  workspace test suite on the macOS or Alpine mainline smoke hosts.
- Windows remains unsupported and is not part of the release matrix. No Windows binary, installer, or signing path is published.

## Release Validation

- **Coverage is an evidence signal, not a threshold**: the canonical release
  gate generates HTML, LCOV, and text reports, but the repository does not claim
  a universal percentage threshold.
- **Hosted release controls remain external**: local validation can run the
  pinned checks and exact clean-tree dry run, but it cannot attest to GitHub
  Actions checks, protected-tag rules, release-environment reviewers, CodeQL,
  or final GitHub attestations. Release staging must still pass the exact-SHA
  hosted mainline and security gates.

## Support

- Response times for support and non-critical issues are not guaranteed.
- See `SUPPORT.md` for supported versions and reporting channels.
