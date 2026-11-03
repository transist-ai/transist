# Pull Request

## Summary

Describe the change and why it is needed.

## Change Classification

For each changed module, classify ownership:

- [ ] `kernel semantic`
- [ ] `reference local topology`
- [ ] `adapter/plugin`
- [ ] `control-plane/server/product surface`

If any `kernel semantic` file changed, confirm boundary rules:

- [ ] No provider-specific logic added to kernel scope.
- [ ] No control-plane/server/distribution dependency added to kernel scope.
- [ ] Side effects route through kernel ports/local topology seams.

## Kernel Contract Impact

- [ ] Kernel contract is not affected.
- [ ] Kernel contract is affected and
      [docs/developer/architecture/kernel-contract.md](../docs/developer/architecture/kernel-contract.md)
      was reviewed or updated.
- [ ] If protected execution, recovery, replay, artifact, evidence, or
      adapter/plugin behavior changed, a core-change record was added under
      `docs/developer/core-change-records/` following
      [.agents/CORE_CHANGE_CHECKLIST.md](../.agents/CORE_CHANGE_CHECKLIST.md).

Describe whether protected execution, recovery, replay, artifact, evidence, or
adapter/plugin behavior changed.

## Backward Compatibility

- [ ] No user-visible compatibility impact.
- [ ] Backward-compatible additive change.
- [ ] Breaking or behavior-changing update that requires a changelog entry and,
      for a pre-1.0 minor release, a migration note.

If compatibility-impacting, update:

- [ ] [CHANGELOG.md](../CHANGELOG.md)
- [ ] [Compatibility and Stability](../docs/public/reference/compatibility-and-stability.md)
      when a documented public contract moves.

Describe any CLI, runtime, storage, artifact/evidence, or inspection impact.

## Security And Trust Boundary

- [ ] No trust, secret, local-only, or security-surface change.
- [ ] Trust, secret, local-only, or security-surface change — the
      [SECURITY.md](../SECURITY.md) trust boundary is preserved: `transist` does
      not gain key generation or signing code; those live in `transist-admin`.

For trust, secret, local-only, dependency, release-executable, or third-party
notice changes, also run the security regression suite and supply-chain checks
(see Validation).

## Validation

Run the applicable local commands after the final code change and **before
merging to `main`** (pinned Rust `1.88.0`, `--locked`). The hosted `PR Gate /
pr-gate` runs the full Linux gate plus schema/OpenAPI and cargo-deny checks; it
does not replace the applicable local and release-boundary commands below:

- [ ] `cargo +1.88.0 fmt --all -- --check`
- [ ] `cargo +1.88.0 clippy --locked --workspace --all-targets --all-features -- -D warnings`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --locked --workspace --all-features --no-deps`
- [ ] `cargo +1.88.0 test --locked --workspace --all-targets`
- [ ] `bash scripts/ci/check_pr_ready.sh` (the full local gate: formatting,
      clippy, rustdoc, maintainability, workspace tests, release build, archive
      reproducibility, quickstart smoke, and all required E2E suites)
- [ ] `bash scripts/ci/run_security_regression_suite.sh` (required for trust,
      secret, local-only, dependency, release-executable, or third-party notice
      changes)
- [ ] `bash scripts/ci/check_supply_chain_pins.sh` (required for dependency,
      CI-tool, release-executable, or third-party notice changes)
- [ ] `bash scripts/ci/check_licenses.sh` (required for dependency or
      third-party notice changes)
- [ ] `bash scripts/ci/validate_docs_and_skills.sh` (required for any doc or
      skill change)
- [ ] `bash scripts/ci/check_release_readiness.sh` (canonical aggregate gate for
      release readiness — runs every check above plus supply-chain pins, legal
      notices, full-git-history secret scanning, schema/OpenAPI contracts, release
      archive inventory, admin sign/verify conformance, and release invariants;
      required before tagging a release)
- [ ] Commits are signed off with `git commit -s` (DCO).

Record each command, its result, and any skipped check with a justification in
the checklist description below. A command that failed, or was not run, must be
explicitly approved by a maintainer before merging to `main`.

## Tests

- [ ] Added tests
- [ ] Updated existing tests
- [ ] No test changes needed

List the coverage added or explain why the existing suite is sufficient.

## Docs

- [ ] Updated docs
- [ ] Updated agent skills under `.agents/skills/` when behavior moved
- [ ] No doc/skill changes needed

List the docs, skills, guides, handbook pages, examples, glossary, and E2E
plans changed, or explain why docs are unchanged.
