# Pull Request

## Summary

<!-- What changed and why? Link the issue if applicable (e.g., Closes #123). -->

## Change Classification

<!-- Check all that apply. For kernel changes, follow AGENTS.md. -->

- [ ] `kernel semantic`
- [ ] `reference local topology`
- [ ] `adapter/plugin`
- [ ] `control-plane/server/product surface`
- [ ] Docs / CI / tooling only

## Kernel Contract Impact

- [ ] Not affected.
- [ ] Affected — [kernel contract](../docs/developer/architecture/kernel-contract.md) reviewed.

<!-- Describe any impact. If protected execution, recovery, replay, artifact,
evidence, or adapter/plugin behavior changed, link the core-change record. -->

## Backward Compatibility

- [ ] No user-visible impact.
- [ ] Additive and backward compatible.
- [ ] Breaking or behavior-changing — changelog updated.

<!-- Describe any impact, migration notes, or public contract docs updated. -->

## Security And Trust Boundary

- [ ] No security or trust-boundary impact.
- [ ] Affected — trust boundary preserved; applicable checks are listed below.

<!-- Describe any impact. Key generation and signing stay in transist-admin. -->

## Validation

<!-- List commands and results, or explain why checks were not run. See
CONTRIBUTING.md#validation. Failed or skipped applicable checks require
maintainer approval before merge. -->

- [ ] Commits signed off (`git commit -s`).

## Tests

<!-- List tests added or updated, or explain why none were needed. -->

## Docs

<!-- List docs or skills updated, or explain why none were needed. -->
