# Workflow Setup Scenarios

## First Workflow

- Ask whether the user has a particular `workflow.yaml` or wants a bundled
  example/demo.
- For a bundled example/demo, present this exact catalog:
  1. `quickstart` - signed governed quickstart example.
  2. `01_hello_world` - smallest deterministic demo.
  3. `02_review_with_manual_gate` - multi-step review demo.
  4. `03_resume_after_failure` - resume a failed run.
  5. `04_replay` - replay a completed run.
  6. `05_artifacts_and_evidence` - artifacts and evidence.
  7. `06_loop_until_condition` - loop control.
  8. `07_subflow_basics` - child workflow.
  9. `08_manual_gate` - manual approval gate.
  10. `09_cleanup_branches` - cleanup paths.
  11. `10_secret_env` - secret delivery.
  12. `11_filesystem_policy` - filesystem policy.

## Signing Authority

- For unsigned governed artifacts, confirm ownership or explicit republishing
  authority before offering signing.
- Ask whether a key already exists; if it does, require its exact path and
  never search for it. If not, offer generation only on an authorized
  administrative/development machine.
- Require fresh immediate consent for key generation, public export, and every
  command that uses a private key. Signature inspection and verification are
  read-only and do not need consent.

## Policy-Only Failure

- If readiness reports exact missing grants and no other blocking validation
  failures, show the generated authorization fragment and ask whether to apply it.
- If structural validation, package trust, plugin, or input checks fail, stop
  authorization. Do not apply policy as a workaround. Rerun readiness after a
  successful authorization.

## Readiness And Runtime Failure

- Missing secrets, prerequisites, plugin health, policy, workspace, and network
  authorization are readiness findings. They return `run_created: false`; repair
  the exact finding and rerun readiness without creating a diagnostic run.
- Once preparation creates a run, preserve and inspect its preflight and
  execution diagnostics even if no action dispatches. Repairing an external
  runtime condition does not require a new workflow package; changing workflow
  bytes does.
