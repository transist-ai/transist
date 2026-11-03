# Workflow Setup Lifecycle

Ordinary installed `check` and authorization use probe-free validation; passing
required validation leaves readiness `not_ready`. Warnings and incomplete checks
block success. `--live` or `--output` selects live readiness; launch review uses
fast validation but preparation still performs live checks before run birth.
Already-authorized calls probe and write nothing. Applying reevaluates current
inputs/resources/policy and compares the exact reviewed fragment/destination;
drift aborts without writes and requires another review.

## Delivery Choice

- For a new or changed governed workflow, use `transist-admin workflow package`
  on an authorized administrative machine and `transist workflows install` on
  the runtime machine.
- In a governed home, installation verifies and persists the package only.
  It never binds inputs, evaluates readiness, or writes policy.
- For an existing package, verify the supplied publisher public root before
  importing it. The runtime never accepts a private key.
- Use direct unsigned installation only in an explicitly selected standalone
  temporary home.

## Installation Sequence

```bash
transist workflows check --path <workflow-yaml>
transist-admin workflow package <workflow-yaml> --version <version> --private-key-file <private-key-file> --output <package-directory>
transist workflows install --package <package-directory>
transist workflows check <id>@<version> --inputs-file <inputs.json> --output readiness.json
transist workflows authorize <id>@<version> --inputs-file <inputs.json>
transist workflows check <id>@<version> --inputs-file <inputs.json> --output readiness.json
transist workflows prepare <id>@<version> --run-ref <run-id> --inputs-file <inputs.json> --readiness-file readiness.json
transist runs start <run-id> --trusted-execution
```

`check` and `authorize` both accept repeatable `--input key=value` arguments and
`--inputs-file`. Bind the same prospective invocation; inline values override
same-named file values when both forms are supplied.
Interactive and piped results group blockers; explicit `--format json` retains all
checks with `diagnostics.groups.<category>.check_indices` for filtering.
`diagnostics.authorization.blocking_codes` explains why a missing-grant plan
cannot be offered. Fix input/action-schema failures first, then rerun readiness
and review exact grants. `--dry-run` inspects the plan without writing policy.

Installation is input-free immutable persistence. `check` and `authorize` use
the prospective inputs and return `run_created: false`; only authorization may
write an exact, no-clobber policy fragment. Recheck after authorization before
preparation or execution.

Before applying a generated fragment, inspect every reachable package-local
module. Modules inherit root workspace and network ceilings and may only narrow
them. If installation validation reports a mismatch, correct the declaring root
or module, validate and publish a newly reviewed package; never hand-author a
broader policy fragment. Maintained catalog releases/pins stay `0.1.0`, schemas
stay `v1`, and runtime/admin crates use `0.1.0`. Changed same-version bytes require
a new signature and fresh home. Keep the old home/bundle for pinned runs and
rollback; never overwrite installed bytes or delete pinned packages/runs.
For the current authoring/persistence contract, follow
`docs/public/reference/compatibility-and-stability.md`: use a fresh home and preserve
the preceding runtime/bundle/home together. No legacy reader, version alias or
in-place home migration is provided.

## Policy Rule

- Treat a policy-only failure as distinct from source, trust, input, and
  runtime-only failures.
- Apply only the generated `transist.policy_fragment.v1` suggestion after user
   approval through `workflows authorize`. It carries exact version-bound plugin
   and capability grants.
- A policy destination must be unused, below `<home>/policy`, and covered by
   configured `policy.sources`. Authorization never merges, overwrites, or edits
   `config.yaml`.
- Secret availability and delivery are readiness checks. If either is denied,
  inspect `remote_secret_delivery.process`, make only the approved minimal
  runtime-wide change, then rerun readiness; do not create a diagnostic run.

## Test Rule

When reviewing execution effects, include every declared cleanup whose `when`
matches a terminal root outcome (`success`, `canceled`, or `failed`): these run
automatically. No matching branch means no action; failure never implicitly
selects canceled cleanup. Cleanup keeps a separate result and does not change the
primary status. Repeated cancel during cancellation cleanup does not interrupt
or duplicate it. `runs cleanup` is recovery for unfinished/failed cleanup after
failure or abrupt process death, not a mandatory normal-path failure command.
Failed runs without matching cleanup stay recoverable; once failure cleanup
begins, primary retry/resume is blocked and new primary work requires replay or
a fresh run. Durable history supports recovery, not guaranteed cleanup completion.

`prepare` tests live readiness but does not execute workflow steps. Start a
prepared run only after a separate user request, then inspect it with
`transist runs tail` and `transist runs show`.
