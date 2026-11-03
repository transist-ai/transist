# Advanced Adapter Appendix

Use this appendix only when a plugin-package task is blocked by adapter implementation or router composition details.

## Core Interface

A hosting-model adapter must implement `ExecutionPort` from `transist-core`.

That means:

- `capabilities()` advertises adapter support
- `execute_step()` accepts `ExecutionInvocation`
- `cancel_step()` accepts `ExecutionCancelRequest`
- both return canonical Transist result types

## Registration Path

Current built-in adapter registration lives in `crates/transist-adapters/src/router.rs`.

Typical extension path:

1. update or add the plugin package under `plugins/` or the installable package layout
2. update manifest handling in `crates/transist-plugins` if needed
3. update the hosting-model adapter in `crates/transist-adapters`
4. register the adapter in `AdapterRouter::with_builtin_adapters()`
5. confirm the default runtime in `src/architecture/mod.rs` still assembles the intended router

## Request Expectations

The runtime provides:

- adapter kind, plugin id, and action
- step and workflow identifiers
- `input`
- run-scoped paths for workspace, artifacts, and evidence
- resolved secret values in-memory only
- remote secret delivery policy in-memory only

Treat those fields as the versioned public contract boundary (protected in
the current minor line; `v1` is a format version, not a SemVer 1.0 stability
promise).

## Result Expectations

Current adapters should:

- return `status: success`, `failed`, or `cancelled`
- use typed `error` data on failures
- keep successful `output` as an object
- keep artifact and evidence metadata aligned with real file behavior
- redact secret material before persistence

## Test Expectations

Minimum current test targets:

- unit coverage close to the adapter implementation
- `crates/transist-adapters/tests/host_adapters.rs`
- `crates/transist-adapters/tests/adapter_gap_coverage.rs`
- relevant kernel tests when policy or cancellation behavior changes
