# Run Cancellation E2E Local Test Plan

Run `bash .agents/testing/e2e_suites/run-cancellation/run.sh` after building
`target/debug/transist`.

The suite creates an isolated standalone home and a separate workspace root;
neither contains the other. The fixtures and shell pin use `0.1.0`, and creation
uses the positional workflow reference with `--run-ref`.

`run_cases.py` drives direct and nested shell invocations through the real CLI.
Both primary work and canceled cleanup wait on explicit workspace release markers
with bounded polling. The driver waits for the primary marker and persisted
active invocation before the initial cancel, then for persisted running cleanup,
its start marker, and its own active invocation before repeating cancellation.
There are no duration-based sleeps to guess when a step is running.

Active records can be under the logical run directory or the home's private
`.transist-staging/invoke-*/active_invocations/` tree because staging replaces the
adapter-visible `run_root`. Match records to the persisted run ID, step ID, and
request invocation ID, and log their actual paths. Check the matched staged
record's cancel sentinel and eventual removal as well as the logical run tree.

Acceptance checks:

- The initial cancel command remains pending while canceled cleanup is gated.
- Direct root and nested child persisted state are `canceled`, proving the
  active process was interrupted rather than only recording a request.
- While cleanup is held open, start three concurrent repeated cancel commands
  against the root and, for the nested case, three against the contained run.
  These commands observe the live cancellation operation; inactive rejection is
  not acceptable during canceled cleanup.
- After each wave of repeated commands, inspect the live run through the CLI and
  poll every original/repeated caller to require that all remain waiting. Require
  the same persisted live cleanup
  invocation, cleanup state `running`, no on-disk invocation cancel marker, no
  trapped signal marker, and no premature cleanup completion marker.
- Release cleanup only after those checks, then collect all original/repeated
  cancellation commands against a shared bounded deadline. Every caller must exit
  successfully with its requested run ID, the same root ID, `settled: true`, final
  status `canceled`, `cleanup_state: succeeded`, and no cancellation errors.
  Repeated callers must not signal active execution or spawn a recovery runner.
- Require cleanup state `succeeded`,
  trigger `canceled`, a finish timestamp, exactly one successful teardown
  execution/result, and exact cleanup marker and attempt-file contents.
- The nested module declares a canceled cleanup sentinel. Require no child
  cleanup record or sentinel marker: terminal cleanup remains root-owned.
- Require no primary-completion or signal marker and no active invocation records
  left in the root tree after cleanup.
- Wait for the detached runner's final lifecycle event before removing the home.
  On failure, print persisted state and release fixture gates before collecting
  every concurrent caller against a shared bounded deadline. Kill and reap callers
  that exceed that deadline, then await runner shutdown. The normal path never
  calls `runs cleanup`.

All fixtures are hermetic and use only the bundled shell plugin. This suite does
not need network services, real secrets, or manual decisions.
