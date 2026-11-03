# Inspection Surface Selector

Use CLI inspection when you need:

- a quick run list
- a concise current-state summary with suggested next commands (`transist runs status <run-id>`)
- current run state
- a short recent event tail

Use JSON inspection endpoints when you need:

- `GET /api/v1/runs`
- `GET /api/v1/runs/{run_id}`
- `GET /api/v1/runs/{run_id}/steps`
- `GET /api/v1/runs/{run_id}/steps/{step_id}`
- `GET /api/v1/runs/{run_id}/events`
- `GET /api/v1/runs/{run_id}/gates`
- `GET /api/v1/runs/{run_id}/artifacts` and artifact content routes
- `GET /api/v1/runs/{run_id}/evidence` and evidence content routes
- `GET /api/v1/runs/{run_id}/workflow-files` and workflow-file content routes
- `GET /api/v1/workflows`
- `GET /api/v1/workflows/{workflow_ref}`
- `GET /api/v1/workflows/{workflow_ref}/source`
- `GET /api/v1/workflows/{workflow_ref}/compiled-recipe`
- `GET /inspect`

Use `/metrics` when you need:

- aggregate counters
- gauges
- histograms
- Prometheus scraping

## Boundary

The companion server is read-only. It complements CLI inspection and persisted storage inspection rather than replacing them.

Run listings are root-run oriented. For nested loop or subflow runs, follow `contained_runs`, `root_run_id`, and `parent_run_id` from run detail.
