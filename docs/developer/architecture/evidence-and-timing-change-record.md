# Evidence and Invocation Timing Corrections

## Problem Statement

- External-transfer evidence hashed struct-ordered JSON but saved key-ordered JSON,
  producing invalid file digests despite identical parsed content.
- Adapters assigned one completion timestamp to both invocation endpoints while
  recording positive elapsed duration.

## Ownership and Alternatives

- Core creates transfer evidence references; correct their serialization there.
- Adapters own invocation clocks and terminal-result construction; capture timing
  there, not in plugins, workflows, inspection, or the execution scheduler.
- Reject plugin/workflow workarounds: neither owns the defective runtime metadata.
- Avoid an artifact-port API change: serialize the same JSON value used by the
  existing local store before computing the reference.

## Runtime Impact

- Preserve statuses, errors, dispatch/event order, authority checks, deadlines,
  cleanup, and durability boundaries. No execution-phase refactor is involved.
- Capture wall-clock start at adapter entry and finish at terminal-result
  construction; preserve both samples even when the wall clock regresses.
- Measure elapsed duration monotonically; forward one terminal timing snapshot
  through result conversion and conversion fallbacks. A late cancellation that
  supersedes a converted result captures its own finish after observation while
  retaining the original invocation start.
- Leave propagated adapter errors as errors; add no synthetic result paths.

## Persistence, Compatibility, and Recovery

- Preserve protocol/schema versions, field types, file names, and JSON meaning.
- Hash the exact key-ordered JSON bytes saved for new external-transfer evidence.
- New result timestamps reflect the measured invocation; historical records remain
  readable and unchanged. No migration or repair command is introduced.
- Retry/resume/replay semantics, captured assets, workspace/path contracts, trust,
  secrets, document publication, and gate decisions are unchanged.

## Tests Added

- `tests/runtime_critical_paths.rs`: the governed external-transfer fixture hashes
  actual files independently and checks result references, descriptor sidecars,
  immutable execution records, typed transfer contents, and invocation evidence.
- `transist-adapters` timing unit tests: distinct wall samples, zero elapsed time,
  and backward wall samples independent of monotonic duration.
- Adapter integration tests: delayed process/HTTP/MCP success, validation and
  sandbox rejection, protocol errors, timeout, cancellation, and lifecycle failure.
- Late process cancellation tests delay the final observation and require the
  winning cancellation's finish to include it, for clean and lifecycle-failure paths.
- Both defects have failing-before/passing-after regression evidence. No live
  provider or installed operator home is required.

## Related E2E Coverage and Validation

- Run `.agents/testing/e2e_suites/run-cancellation/run.sh` for cancellation behavior.
- No existing executable E2E suite checks byte-exact transfer references or all
  adapter clocks directly; focused Rust integration tests cover these contracts.
- Validate each implementation step with its affected tests, strict package lint,
  and changed-file formatting only; run Cargo stages sequentially.
- After integration, require `bash scripts/ci/check_pr_ready.sh` with no skip flags
  to pass after the final change. Record commands, exit statuses, logs, and final
  candidate identity in the implementation handoff.
- No dependencies, trust policy, secrets policy, or local-only semantics change;
  no additional standalone security gate is required for these corrections.
- Final validation exposed a pre-existing mismatch between the README's two
  informational catalog links and the self-containment guard. Align its two exact
  allowlist entries with the existing README; keep all rejection patterns and
  root-README-only restrictions unchanged. Cover with the guard's regression suite.

## Documentation Updated

- Changelog: corrected digests and invocation timestamps, without historical repair.
- Storage reference: exact evidence-byte identity.
- TEP reference: invocation wall timestamps and monotonic duration.
- Kernel contract: runtime-owned evidence references describe persisted bytes.
- No new public concept or glossary term is introduced.

## Rollback

- Revert either implementation independently through normal review; no migration
  is necessary. Existing evidence remains readable.
- Reverting restores the corresponding defect for future executions only. Never
  rewrite historical evidence or timestamps to simulate rollback.
