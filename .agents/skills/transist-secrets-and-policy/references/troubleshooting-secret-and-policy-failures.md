# Troubleshooting Secret and Policy Failures

Installed checks and authorization validate without probes and check secret
availability by name only on every invocation. Passing validation leaves live
readiness `not_ready`; use `check --live` or `--output` for live diagnostics.
Required warnings and incomplete validation block success. Applying grants
revalidates the exact reviewed fragment; drift requires another review without writes.

## Separate the Failure Class

Secret resolution failure means the logical ref could not be resolved at all.

Remote secret delivery denial means the secret resolved, but policy did not allow raw delivery to the target adapter.

Other nearby failure classes include:

- wrong plugin binding
- wrong action name
- unrelated workflow authoring mistake
- unrelated runtime failure

## Useful Inspection Flow

1. inspect the workflow step for invoke-step `with.secret_refs`
2. confirm the adapter kind: `process`, `http`, or `mcp`
3. resolve the effective home with `transist home get` if needed, then inspect policy configuration in `config.yaml`
4. before run creation, use `transist workflows check <id>@<version> --inputs-file <inputs.json>` for prospective schema, workspace, policy, and secret checks; it returns `run_created: false`
5. when only exact authority is missing, use `transist workflows authorize <id>@<version>` with the same repeatable `--input key=value` arguments or `--inputs-file <inputs.json>`, then rerun `workflows check`; do not widen policy
6. after preparation creates a run, use `transist runs status <run-id>` for a quick summary and `transist runs preflight <run-id>` for persisted readiness checks before dispatch
7. use `transist runs capabilities <run-id> --format yaml` when the persisted blocker is `capability_not_granted`; preserve the emitted exact fragment rather than widening policy
8. use `transist runs show <run-id>` to inspect secret-related metadata without exposing values
9. verify that persisted run files still do not contain resolved secret values

Useful fields to inspect include:

- `preflight.status`
- `preflight.checks`
- `last_request.metadata.secret_refs`
- `last_request.policy.remote_secret_delivery`

## Safe Recommendation Pattern

- keep logical secret refs in the workflow
- enable remote delivery only for the required adapter
- avoid broad policy changes when a single plugin action is the only consumer
