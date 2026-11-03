# Benchmarks

Benchmarks use generated fixtures, not a user's runtime home, repository, or run
IDs. Run from a source checkout with the pinned Rust `1.88.0` toolchain:

```bash
cargo +1.88.0 bench --bench tui_load_benchmark -- --noplot
cargo +1.88.0 bench --bench workflow_readiness_benchmark -- --noplot
```

Criterion writes reports under `target/criterion/`. PR readiness compiles the
benchmark targets through `cargo test --locked --workspace --all-targets`; it
does not evaluate timing regressions or upload benchmark results. Benchmark
timing has no portable pass/fail threshold.

## TUI Inspection

The fixture contains one root and 32 child runs. Creation and summary refresh
happen outside measured loops. Each iteration creates a fresh inspection context
with a zero-capacity cache, independent of `TRANSIST_INSPECTION_CACHE`.

- `sequential_child_loads` calls `get_run` once per child.
- `batched_child_loads` calls `get_runs` once with the same ordered list.
- Both assert exactly 32 loaded child details; neither measures root lookup.

## Workflow Readiness

`workflow_readiness_benchmark` generates workflows, packages, schemas, detached
assets, workspaces, and homes under temporary storage. Local process and MCP
probes do not contact the network. The fixture contains three recipes, 37 invoke
steps, 16 distinct actions, five packages, and 13 detached assets of 1,024 bytes
each. The adapter mix is 30 process and seven MCP invokes.

The `sandbox_isolated` case declares a read-only workspace and denied network;
`authority_bearing` declares a full-access workspace and denied network. Plugin
startup has a fixed 50 ms delay to expose scheduling cost. Compare caps on the
same machine:

```bash
for cap in 1 2 4 8; do
  TRANSIST_PREFLIGHT_PROBE_CONCURRENCY="$cap" \
    cargo +1.88.0 bench --bench workflow_readiness_benchmark -- --noplot
done
```

The cap defaults to four, accepts `1..=32`, and applies independently to each
process/MCP preflight call. HTTP and recursive kernel calls remain sequential.
Only a sandbox plan proving read-only workspace/package access, no shared host
writes, and private staging/temp writes is eligible. Authority-bearing probes
remain sequential; macOS is conservatively sequential. Plugin startup is not
assumed side-effect-free.

Equivalent-authority invocations share probes. The fixture asserts 29 application
probe groups (24 process and five MCP). Kernel-recursive run birth checks each
recipe independently: 13, 12, and 12 probes by depth. Readiness leaves no run,
locator, or event; run-birth cases verify complete birth files and remove their
temporary fixture after measurement.

## Measurement Boundaries

The opt-in `capture_preflight_measurements` observer retains fixed scope/phase
enums, adapter kinds, counts, and monotonic durations. It cannot carry inputs,
paths, secrets, wall-clock timestamps, or plugin output. Without explicit capture,
it records nothing and emits no runtime output.

Measured phases include recipe loading, binding, trust, policy, adapters, total
readiness, detached-asset capture, and durable run publication. Recursive totals
include nested work and must not be added as disjoint durations. Probe counters
count actual process starts or MCP probe operations after grouping.

## Report Collection

```bash
cargo +1.88.0 bench --locked -p transist --bench workflow_readiness_benchmark -- workflow_report_collection --noplot
```

`workflow_report_collection/{summary,full}` evaluates the same static bound
validation fixture without live probes. Fixture creation, compilation, parity,
and run-absence assertions are outside timing; setup may print an untimed live
sample before Criterion filters cases.

Summary collection retains no ordered `ValidationCheck` vector. Passing checks
increment a count; non-passing findings group by status, code, message, and
remediation with at most three unique paths per group. Full collection retains
ordered checks and produces the same compact view. Authorization summaries retain
the exact reviewed grant fragment and destination without constructing full
reports. These are collection boundaries, not timing guarantees.

## Validation Fast Path

Build the fresh-process CLI, then run the filtered benchmark:

```bash
cargo +1.88.0 build --locked -p transist-cli -p transist-admin
cargo +1.88.0 bench --locked -p transist --bench workflow_readiness_benchmark -- workflow_validation_fast_path --noplot
```

The group installs into a hermetic home and validates installation receipts
before timing. Every evaluation creates an operation-scoped catalog; inputs,
authorization, and live evidence are not cached across operations. In-process
cases use summary APIs; fresh-process cases launch ten independent debug CLI
processes with explicit homes, fixture-local working directories, and JSON output.
They compare ordinary checks, already-authorized dry runs, and live checks.

The `capture_validation_work` observer counts compiler validation, manifest loads,
package hashes, schema compilation, input validation, binding, and policy
evaluation without retaining identities, inputs, paths, or secrets. Receipt reuse
still requires exact recipe bytes, pinned digests, and validator revision. Every
materialized input and current authority is reevaluated.

Probe-free validation can pass while readiness is `not_ready`. Warnings and
incomplete required validation block success. Preparation, execution, and recovery
still perform live checks. See the
[repeat validation tests](../../tests/workflow_validation_fast_path.rs) and
[authorization E2E fixture](../../.agents/testing/e2e_suites/workflow-authorization/run.sh)
for execution-boundary assertions.

## Interpret Results

Compare medians and confidence intervals only with the same fixture, machine,
toolchain, OS, sandbox backend, Python executable, filesystem, and power mode.
Use Criterion's median estimates, not its terminal slope/mean intervals. Compare
debug CLI cases with each other and optimized in-process cases with each other.
The fixed startup delay is a fixture property, not representative of all plugins.
Record environment and sample settings with measurements; neither timing nor
structural counters establish production latency guarantees.
