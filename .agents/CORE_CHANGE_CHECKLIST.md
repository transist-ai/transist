# Transist Core Change Skill

Use this checklist for any proposed change that touches kernel semantics,
durable runtime behavior, retry or replay behavior, storage layout, artifact or
evidence persistence, or the adapter and plugin boundary.

## Minimum Source Set

Read these documents before changing the core:

- [Contributor Guardrails](../AGENTS.md)
- [Kernel Contract](../docs/developer/architecture/kernel-contract.md)
- [Compatibility And Stability](../docs/public/reference/compatibility-and-stability.md)
- [Storage Layout](../docs/public/reference/storage-layout.md)
- [Companion Server API](../docs/public/reference/companion-server-api.md)

## Problem Statement

State the concrete runtime, operator, or contributor problem. Describe the
current behavior and why it is insufficient.

## Why The Change Belongs In Core

Explain why the change belongs in core instead of a workflow, plugin, adapter,
policy implementation, or companion tool.

## Alternatives Outside Core Considered

List the alternatives outside core considered and why they were rejected.

## Runtime Impact

Describe the runtime impact on execution semantics, state transitions,
inspection surfaces, policy checkpoints, or control flow.

For execution-kernel refactors, record the typed phase decomposition explicitly
(`RecoveredLoadedRun` -> `ActivatedLoadedRun` -> `PreflightedLoadedRun` ->
`ScheduledLoadedRun`) and verify that it is behavior-preserving: persisted
bytes, event ordering, dispatch order, and durability boundaries must remain
unchanged. Model cancellation and preflight terminal exits as explicit outcomes.

## Retry And Replay Impact

Describe the retry/replay impact, including whether resume, retry executions,
replay lineage, or persisted recipe behavior changes.

## Storage, Artifact, And Evidence Impact

Describe the storage/artifact/evidence impact, including any effect on
`state.json`, `recipe.yaml`, `events.jsonl`, artifact records, evidence records,
or durable run layout.

## Tests Added

List the tests added. If existing coverage changed, name the updated suites and
what persisted behavior they prove.

## Required Related E2E Suite

If the change touches workflow management or another covered runtime path, run
the corresponding executable suite under `.agents/testing/e2e_suites/` and name
it in `Tests Added`. If no suite covers the path, use focused cargo tests and
record that limitation.

## Docs Updated

List the docs updated. When the protected contract changes, update
`docs/developer/architecture/kernel-contract.md`. When user-visible behavior changes,
update the relevant reference, guide, or handbook pages.

**Glossary:** If the change introduces a new runtime term (concept, type,
protocol, participant, or mode that is visible to users or operators), add a
definition to `docs/GLOSSARY.md`.

## Workflow Path Contracts

If a change affects workflow-authored file paths or plugin path resolution,
state which contract changed:

- runtime capability resources such as `workspace:///absolute/root`,
  `run://artifacts`, `run://evidence`, and `file:///absolute/path`
- plugin action conventions such as `run://...` and `repo://...`, including
  their plugin-specific resolver and persistence behavior
- plugin-local helper assets such as shell `source://...`
- manifest-only sentinels such as `runtime://temp`

Do not describe a plugin-local scheme as generic workflow syntax. Update the
relevant reference, guide, plugin README, and [Path and Resource
Semantics](../docs/public/reference/path-and-resource-semantics.md) when the
supported path surface changes. `run://...` is not durable cross-step storage;
`artifact://` and `evidence://` do not exist.

## Validation Before Merging To Main

Run the applicable local validation after the final code change and before
merging changes to the trunk branch (`main`):

```bash
bash scripts/ci/check_pr_ready.sh
```

The hosted `PR Gate / pr-gate` runs the full Linux local gate plus schema/OpenAPI
and cargo-deny checks, but does not replace the applicable local validation
while iterating or release-boundary validation before tagging.

Change-type additions:

- Documentation-only changes: run `bash scripts/ci/validate_docs_and_skills.sh`.
- Dependency, trust, secret, or local-only changes: also run
  `bash scripts/ci/run_security_regression_suite.sh`.
- Kernel or public-contract changes: also run the matching e2e suite from
  `.agents/testing/e2e_suites/` before review.

Before tagging a release, run the canonical aggregate gate:

```bash
bash scripts/ci/check_release_readiness.sh
```

Validate the command output and confirm it completed successfully. Record every
command, its result, and any skipped check with justification in the pull
request. A change is not ready to merge while an applicable command is failing,
or was not run without documented maintainer approval.

## Submission Standard

A proposed core change is incomplete unless the pull request includes every
section above. If the change cannot be justified outside extensions, it should
not merge into the Transist core.
