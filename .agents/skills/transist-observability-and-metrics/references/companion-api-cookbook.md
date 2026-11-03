# Companion API Cookbook

Start the local inspection server with:

- `transist-server --port 7777 [--workspace-root <path>]`

The default bind is `127.0.0.1`. Non-loopback bind requires `--allow-non-local-bind`.

Key routes:

- `GET /health`
- `GET /metrics`
- `GET /api/v1/runs`
- `GET /api/v1/runs/{run_id}`
- `GET /api/v1/runs/{run_id}/steps`
- `GET /api/v1/runs/{run_id}/steps/{step_id}`
- `GET /api/v1/runs/{run_id}/trust`
- `GET /api/v1/runs/{run_id}/locality`
- `GET /api/v1/runs/{run_id}/events`
- `GET /api/v1/runs/{run_id}/gates`
- `GET /api/v1/runs/{run_id}/artifacts`
- `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}`
- `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}/content`
- `GET /api/v1/runs/{run_id}/evidence`
- `GET /api/v1/runs/{run_id}/evidence/{evidence_id}`
- `GET /api/v1/runs/{run_id}/evidence/{evidence_id}/content`
- `GET /api/v1/runs/{run_id}/workflow-files/{workflow_file_id}`
- `GET /api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content`
- `GET /api/v1/workflows`
- `GET /api/v1/workflows/{workflow_ref}`
- `GET /api/v1/workflows/{workflow_ref}/source`
- `GET /api/v1/workflows/{workflow_ref}/compiled-recipe`
- `GET /inspect`

This list is mirrored by the canonical OpenAPI contract at
`schemas/http/companion-api.v1.openapi.yaml` and the route inventory in
`crates/transist-server/src/infrastructure/http_server/routes.rs`; the
`transist-server` `api_contract_parity` test keeps all three in bidirectional
parity and enforces that every route is `GET` (read-only).

## What Each Route Is For

- `GET /health`: confirm the server is up and read-only
- `GET /metrics`: Prometheus text exposition for the local instance
- `GET /api/v1/runs`: paginated run list with lightweight metrics
- the runs list is root-run oriented, so nested child runs stay under run detail instead of appearing as peer top-level rows
- `GET /api/v1/runs/{run_id}`: structured run detail with summary metrics
- run detail exposes `contained_runs`, `root_run_id`, and `parent_run_id` for nested lineage-aware inspection
- `GET /api/v1/runs/{run_id}/steps`: step summary list for one run
- `GET /api/v1/runs/{run_id}/steps/{step_id}`: step detail with usage and numeric metrics when present
- `GET /api/v1/runs/{run_id}/trust`: trust-decision and signature summary for the run tree
- `GET /api/v1/runs/{run_id}/locality`: provider locality and external-transfer summary for the run tree
- `GET /api/v1/runs/{run_id}/events`: ordered run event inspection
- run gate, artifact, and evidence routes: inspection of durable decision and file surfaces
- workflow routes: workflow inventory, structured workflow detail, and direct source or compiled-recipe content fetches
- `GET /inspect`: built-in read-only browser UI on the same local origin as the API, including trust and locality rendering in run detail

## Read-Only Guardrail

These endpoints do not start runs, resume runs, replay runs, make gate decisions, or mutate plugins.
