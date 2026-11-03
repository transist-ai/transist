---
name: transist-observability-and-metrics
description: Inspect current Transist observability surfaces and metrics. Use when starting `transist-server`, reading `/metrics`, using `GET /health` and `/api/v1/...` inspection endpoints, and deciding when metrics, CLI inspection, or JSON inspection is the right surface.
---

# Transist Observability and Metrics

Help the user choose the right inspection surface, start the local inspection server when needed, and interpret Transist v1 metrics without inventing unsupported fleet or per-run Prometheus behavior.

Use `transist-server` by default for the companion HTTP surface.
If working from a source checkout instead of an installed binary, substitute `cargo run -p transist-server -- ...`.

## Default Workflow

1. Determine whether the question is about current run state, structured JSON inspection, or aggregate metrics.
2. Use the smallest useful surface:
    - CLI for direct run inspection, starting with `transist runs status <run-id>` for the blessed human-readable summary before reaching for `runs show`; use `transist runs preflight <run-id>` for a fresh persisted readiness report
    - JSON endpoints for structured run, step, and event detail
    - `/metrics` for Prometheus-friendly counters, gauges, and histograms
    - `TRANSIST_PROGRESS` JSON lines on `stderr` (schema `transist.cli.progress.v1`) when a machine-readable progress stream is needed from `run`, `runs resume`, `runs replay`, or gate decisions
3. Start the local inspection server when HTTP or Prometheus access is needed:
   - `transist-server --port 7777 [--workspace-root <path>]`
   - default bind is `127.0.0.1`; non-loopback bind requires `--allow-non-local-bind`
4. Check the core endpoints:
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
   - `GET /api/v1/runs/{run_id}/artifacts` and artifact content routes
   - `GET /api/v1/runs/{run_id}/evidence` and evidence content routes
   - `GET /api/v1/workflows` and `GET /api/v1/workflows/{workflow_ref}`
   - `GET /api/v1/workflows/{workflow_ref}/source` and `GET /api/v1/workflows/{workflow_ref}/compiled-recipe`
   - `GET /inspect` when the built-in read-only UI is the fastest way to inspect runs or workflows; it renders per-run trust and locality summaries alongside the standard run detail
5. Interpret metric families, label constraints, histogram behavior, and usage data only from what the runtime actually exports.
6. Finish with the chosen surface, findings, and any gaps that still require CLI or filesystem inspection.

## Hard Rules

- Metrics are single-instance and local-first.
- `run_id` and `step_id` are not exported as Prometheus labels.
- Token metrics appear only when adapters or plugins report `usage`.
- Partial `usage` payloads remain partial; Transist does not infer missing token fields.
- Arbitrary adapter or plugin metric names do not become first-class Prometheus series in v1.
- Metric summaries complement run-state inspection; they do not replace `runs show`, event inspection, or persisted run state.
- Companion-server run lists are root-run oriented; nested loop and subflow runs are grouped through `contained_runs` with `root_run_id` and `parent_run_id`.
- Run-detail inspection exposes the effective `runtime_mode`, `policy_profile`,
  and `capability_enforcement` posture persisted for each run.
- Companion-server artifact and evidence routes are direct-run only; use the relevant child run when nested blobs are not on the root run itself.
- `workflow_assets/` is private run storage for snapshot-backed prompt and
  `source://` captures, not a companion-server or snapshot-export surface.
  Diagnose it only through local run storage by manifest path and digest; never
  expose captured prompt contents.
- The companion HTTP API is read-only and does not execute, resume, replay, or mutate runs. The canonical `/api/v1` route inventory lives in `crates/transist-server/src/infrastructure/http_server/routes.rs`, the published OpenAPI contract lives at `schemas/http/companion-api.v1.openapi.yaml`, and the `api_contract_parity` test keeps the inventory, router, and OpenAPI in bidirectional parity (all `GET`).

## Inspection cache behavior

- Run-detail inspection uses an in-memory read-through cache keyed by file fingerprint (`mtime` + file length).
- Cache scope is process-local; persisted files remain the source of truth.
- Capacity is LRU-bounded by run id, defaulting to `10` runs.
- User override: `tools.inspection_cache_runs` in user settings.
- Environment override: `TRANSIST_INSPECTION_CACHE=off` forces capacity `0`.
- In the run-detail screen, `r` invalidates the visible run and child-run entries before refresh.
- `events.jsonl` appends are durable (flush + `sync_all` + parent-dir sync); a truncated final fragment is repaired on the next append and a `storage.tail_repaired` event is emitted. Inspection ignores one truncated final fragment and rejects malformed complete or interior records.

## Source Priority

Use facts in this order when guidance conflicts:

1. `docs/public/reference/metrics.md`
2. `docs/public/reference/companion-server-api.md`
3. `docs/public/guides/inspection/observing-runs-with-prometheus.md`
4. `docs/public/guides/inspection/companion-http-server-in-practice.md`
5. `crates/transist-server`

## Sync vs metrics boundary

- The `transist.inspection.snapshot.v1` envelope is a separate library-level
  outbound sync contract, not a companion-server surface. `transist-server`
  does not ingest or emit it.
- Sync is push-only and non-authoritative; central inspection is optional
  and never blocks execution. `/metrics` is a local runtime surface.
- The snapshot envelope now carries nullable fleet-syncable trust/provenance
  fields populated from `provenance.json` on push: `trusted_execution_mode`,
  `workflow_digest`, `workflow_signature_status`, `workflow_signer_key_id`,
  `plugin_trust[]`, `trust_decision`, `external_transfer_summary`,
  `approver_identity[]`, `locality`, and `evidence_export`. Forward-only
  reserved identifiers (`runtime_id`, `organization_id`, `policy_revision`)
  are never populated in v1 — treat absence as normal. See
  `docs/public/reference/compatibility-and-stability.md`
  ("Fleet-management compatibility").

## References

- Inspection surface selector: [inspection-surface-selector.md](references/inspection-surface-selector.md)
- Prometheus and metric families: [prometheus-and-metric-families.md](references/prometheus-and-metric-families.md)
- Companion API cookbook: [companion-api-cookbook.md](references/companion-api-cookbook.md)
