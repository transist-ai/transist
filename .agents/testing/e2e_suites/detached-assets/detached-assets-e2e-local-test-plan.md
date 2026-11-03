# Detached Workflow Assets E2E Test Plan

## Goal

Exercise governed trusted execution of workflow-owned prompt and `source://` assets, including run-owned asset capture and post-completion inspection after authored files are removed.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite detached-assets`.

## Not Covered

- Pre-install deletion of authored files before package capture. Snapshot-backed run dispatch after successful run birth is covered separately by the captured-asset assertions.
- Cross-platform sandbox behavior beyond the runtime's configured policy boundary.
- Remote providers or network access.

## Fixture Layout

- `detached.workflow.yaml`: workflow packaged and signed by `transist-admin`, with prompt-file and shell `source://` steps.
- `prompt-plugin/`: local process plugin that asserts captured prompt delivery.
- `prompts/task.md`: workflow-owned prompt asset.
- `scripts/hello.sh`: workflow-owned shell source asset.
- `config.yaml`: governed allowlist policy template; the suite substitutes its private workspace path.

## Assertions

- Ephemeral trust material is created by `transist-admin` and the private key is removed before execution.
- The package installs without inputs; a prospective check blocks until its exact workspace authorization is written, then a recheck becomes ready.
- The run succeeds with `asset_lock_version: 1` and `workflow_asset_snapshot: true`.
- Prompt and source assets are persisted in the private workflow-asset manifest and blobs.
- Each asset-consuming step executes exactly once.
- `runs show` remains available after deleting the authored prompt and source files.
