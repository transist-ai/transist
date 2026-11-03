# Companion Server API

The `transist-server` process provides a local, read-only API for inspecting Transist runs, workflows, and metrics. This document describes its endpoints and behavior.

**Stability:** The `/api/v1/...` routes and the `/metrics` endpoint are versioned public surfaces. `v1` is a format version, not a SemVer 1.0 stability promise.

## Machine-Readable Contract

The canonical OpenAPI 3.0 contract for every `/api/v1` route is published at
[`schemas/http/companion-api.v1.openapi.yaml`](../../../schemas/http/companion-api.v1.openapi.yaml).
It declares every route, parameter, success response, pagination envelope,
content response, and error. Closed response DTOs declare their properties,
required fields, and optional-field omission rules; arbitrary persisted JSON
is explicitly identified as open data. Representative JSON responses emitted
by the handlers are validated against this contract in integration tests. The route inventory in
`crates/transist-server/src/infrastructure/http_server/routes.rs` is the single
source of truth for the registered Axum router, and the `transist-server`
`api_contract_parity` test asserts bidirectional parity between the inventory,
the registered router, and the OpenAPI contract. `scripts/ci/check_schema_api_contracts.sh`
gates this surface in PR, mainline, and release. Every operation is `GET`;
no mutating route exists or can be added without updating the inventory and
OpenAPI in the same change.

## Server Behavior

- **Workspace Root:** Defaults to the current directory; override with `--workspace-root <path>`.
- **Transist Home:** Resolved the same way as the CLI: `--home` flag, then `TRANSIST_HOME` env, then the persisted user-set home, then the per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`).
- **Binding:** By default, it binds to `127.0.0.1:7777`. To bind to a non-loopback address, you must use the `--allow-non-local-bind` flag.
- **Transport Security:** The server has no built-in authentication or TLS.
    `--allow-non-local-bind` only permits the bind; it does not secure the API.
    Any non-loopback deployment must be placed behind a trusted reverse proxy
    that provides authenticated TLS access.

## API Conventions

- **Health & Metrics:** `GET /health` provides a simple health check. `GET /metrics` exposes Prometheus metrics.
- **JSON API:** Metadata and inspection endpoints under `GET /api/v1/...` return
    `application/json` with closed response DTOs. Blob content downloads keep
    their inferred media type (or `application/octet-stream`) and add an
    attachment disposition. Workflow source and compiled recipes return
    `text/yaml; charset=utf-8`, `application/json`, or text content according
    to the stored representation.
- **Pagination:** List endpoints return a `data` array and a `pagination` object.
- **Read-Only:** The API is strictly for inspection. There are no `POST`, `PUT`, or `DELETE` routes for modifying state.
- **Host-independent paths:** JSON responses never expose absolute local
    filesystem paths. Workflow-file metadata uses an opaque
    `workflow_file_id`, a workspace-relative `relative_path`, and content links.
    Clients must not decode resource IDs or reconstruct local paths. Workflow
    source, subflow, prompt, and filesystem-policy path fields follow the same
    rule. Known workspace-relative paths retain nested components; unknown or
    external paths are reduced to safe labels, and `workspace_root` is exposed
    only as `"."`.
- **Persisted JSON redaction:** Arbitrary persisted JSON in workflow inputs,
    event summaries, gate request summaries, diagnostics, and failure records
    is sanitized recursively at the HTTP boundary. Absolute POSIX, drive-letter,
    UNC, home-relative, and traversal paths, including forms prefixed by `./`,
    become `[path redacted]`. Path-shaped object keys are sanitized too;
    deterministic numeric aliases preserve colliding entries. Non-path values
    and JSON structure are preserved. This host-path guarantee applies to
    non-URL values; valid HTTP(S) URLs are handled as opaque remote identifiers.
    Reads never rewrite persisted state.
- **URL preservation:** Valid `http://` and `https://` values remain
    byte-identical by default, including their authority, path, query, fragment,
    ordering, duplicates, separators, and punctuation. The transport does not
    classify or redact URL components as host paths.
- **Central Sync Boundary:** The `transist.inspection.snapshot.v1` envelope is exported by the local library surface and `transist runs snapshot export`. `transist-server` does not ingest or emit that envelope.

### Timer Fields and Semantics

Run and step metrics expose three timer meanings:

- **`wall_clock_duration_ms`**: lifecycle elapsed time (`started_at_ms` to `finished_at_ms`, or to `now` for active/blocked snapshots).
- **`active_execution_duration_ms`**: summed concrete execution work across attempts/subtrees.
- **`blocked_wait_duration_ms`**: summed manual-gate wait intervals.

Backward compatibility: `total_duration_ms` remains wall-clock duration and is preserved for existing clients.

## Endpoints

### Health and Metrics

- `GET /health`: Returns a `200 OK` if the server is running.
- `GET /metrics`: Exposes real-time operational metrics in Prometheus format.

### Runs

Used for inspecting workflow runs.

- `GET /api/v1/runs`: Lists all top-level runs.
- `GET /api/v1/runs/{run_id}`: Retrieves detailed information for a single run.
- `GET /api/v1/runs/{run_id}/trust`: Retrieves the run's trust/provenance summary.
- `GET /api/v1/runs/{run_id}/locality`: Retrieves provider/model locality and external-transfer evidence for the run.
- `GET /api/v1/runs/{run_id}/steps`: Lists all steps within a run.
- `GET /api/v1/runs/{run_id}/steps/{step_id}`: Retrieves details for a specific step within a run.
- `GET /api/v1/runs/{run_id}/failures`: Lists classified failure records for a run.
- `GET /api/v1/runs/{run_id}/failures/{failure_id}`: Retrieves one classified failure record.

`GET /api/v1/runs/{run_id}` includes inspection fields for
`trusted_execution_mode`, `runtime_mode`, `policy_profile`,
`capability_enforcement`, `trust`, `locality`, and `evidence_export`. When
available it also includes sanitized filesystem-policy summaries. The
`evidence_export` object includes a `command` field (always present) and an
optional `location` field (present only when provenance exists).

You can filter the runs list with the following query parameters: `limit`, `offset`, `status`, `workflow`, `created_after`, `created_before`, `updated_after`, `updated_before`.

**Note on Nested Runs:** The main `/api/v1/runs` list only includes top-level runs. Runs created by `loop` or `subflow` steps are considered nested and are available inside the `contained_runs` field of their parent run's detail view.

**Note on Failed Subflow Steps:** When a parent subflow step fails because a descendant run failed, or because binding the child inputs failed before the child run could start, the parent step's `output_summary` may include an additive `child_failure` object with a compact pointer to the failed descendant (`child_run_id`, optional `failed_step_id`, `error_code`, `error_message`, `stderr`, `preflight_blocker_count`, `preflight_warning_count`, `preflight_first_blocker`). Older runs that predate durable child-failure attribution omit these fields; consumers that do not read them continue to work.

### Step Output Selection

`GET /api/v1/runs/{run_id}/steps/{step_id}?execution=latest|<positive integer>`
returns latest step metadata plus a selected persisted output. Omitted
`execution` means `latest`. Accept exactly `latest` or ASCII decimal digits
representing a positive `u32` (1–4294967295), including leading zeros. Empty,
whitespace-containing, signed, zero, nonnumeric, and overflowing selectors
return `400 invalid_request` in the standard problem envelope.

Detail responses always include `output` and `output_executions`:

```json
{
  "output": {
    "execution": 2,
    "status": "failed",
    "source": "execution_record",
    "result_present": true,
    "value": {"response_text": "Diagnostic advice"}
  },
  "output_executions": [1, 2]
}
```

- Latest is the maximum of the persisted execution counter and record ordinals;
  zero means no execution. Immutable records win even when they have no result.
- Without a latest record, `last_result` is accepted only when run, step, and
  execution identifiers match and step status is `success`, `failed`,
  `retry_wait`, or `canceled`. Other state results are excluded.
- Otherwise latest returns `source: "state_snapshot"`, `result_present: false`,
  and no `value`; an earlier result is never substituted for a newer attempt.
  `output.execution` is null when no execution exists, with current step status.
- Explicit ordinals require a record or the matching latest state-result
  fallback. Missing run, step, or selected execution returns
  `404 resource_not_found`. Malformed, unreadable, or identity-mismatched records
  return sanitized `500 storage_query_failure`.
- `output.status` is attributed to the selected execution. Existing fields,
  including `output_summary` and `executions`, retain their latest-state meanings
  and compact shapes. Run and step lists do not acquire output values.
- `value` is emitted exactly when a result exists. Null, `{}`, `[]`, `false`,
  `0`, and `""` are retained explicitly. Values use plugin field names without
  interpretation, are sanitized recursively (including path-shaped keys), and
  are returned complete without truncation. Valid HTTP(S) URLs retain the
  existing transport behavior.
- `output_executions` is always an ascending, deduplicated array of positive
  selectable ordinals: records (including no-result records) and a matching
  latest state-result fallback, excluding uncommitted attempts without results.
- Detail `metrics` is omitted when unavailable. Successful, failed, and pending
  steps remain inspectable. GET requests do not rewrite persisted bytes or
  dispatch work; snapshot-export payload selection is unchanged.

### Events

- `GET /api/v1/runs/{run_id}/events`: Lists all durable events recorded for a run. Supports `limit`, `before`, `after`, and `order` query parameters.

### Gates

- `GET /api/v1/runs/{run_id}/gates`: Lists any manual approval gates that are blocking or were decided during a run.

Gate decision objects now include additive `actor`, `reason`, and `decided_at` fields so companion inspection can show who approved or rejected a gate and when.

Gate responses always include typed `review_documents` (empty for ordinary gates).
Each item has `revision`, `links` and `members`; the compact gate view leaves tree
members empty and links to revision detail. Revision metadata includes document
name, kind, exact revision ID, SHA-256, byte/file counts and producing identities.
`request_summary.review_request` retains the original execution-bound request,
never refreshed from latest heads. The Gates HTML tab shows the same metadata and
links. Document decisions and decision history add typed `review_request` with
the exact original refs and execution. A prior cycle's decision appears in history,
not as the current waiting request's decision. Decisions are still CLI-only mutations; reviewing a document approves no
arbitrary workspace snapshot.

Authored guidance cannot populate runtime review metadata. Inspection strips the
reserved summary keys `review_request`, `review_document_count` and
`review_decision`, then inserts a request only from a validated committed
document-review execution. Ordinary gates and pending/failed gates with no such
request have an empty review list. Full decision contents (including actor,
actor source, reason and timestamp) must match exact immutable history; consumed
history must also equal the decision embedded in its immutable completion.
Conflicting or missing decision history fails closed with a sanitized storage
error, rather than displaying contradictory status and history.

### Managed Documents

- `GET /api/v1/runs/{run_id}/documents/{revision_id}`: typed immutable revision
  metadata and content links, including tree members.
- `GET /api/v1/runs/{run_id}/documents/{revision_id}/content`: exact file bytes or
  canonical tree manifest bytes as an attachment (`application/octet-stream`).
- The same content route accepts optional `?member_id=<opaque-id>` to download
  one exact tree member. Obtain that ID from metadata; no path parameter/query
  selecting a filesystem file is accepted.

IDs are opaque lowercase 64-hex values scoped to the selected run's physical root
document namespace, and member IDs additionally to the revision. Missing IDs,
unpublished candidates and other-run revisions return `404`; malformed IDs or
unsupported query selectors return `400`. Corruption, symlinks or inconsistent
root/parent lineage fail closed with sanitized `500 storage_query_failure`.
An existing root or contained run with no declared documents has an empty
namespace and returns `404` on both document routes after lineage validation.
A missing initialization record when documents are declared remains corruption
and returns `500`.
Contained runs share their current root's revisions; replay starts a distinct root
and cannot read the source's documents. Reads validate committed history and
digest/size/manifest metadata through inspection/storage ports; no durable home
paths appear in response metadata or links. Exact business content is deliberately
not rewritten by HTTP path redaction. These content endpoints use the same local
read-only server access boundary: loopback by default, authenticated TLS reverse
proxy required for non-loopback access; no new authentication or mutation surface.

Content is bounded by the workflow's authored `max_bytes` / `max_files` and the
storage manifest/path rules, not by a separate HTTP response-size limit. Reads
materialize exact payloads and validate retained history; work and memory grow
with those declarations, history and tree size. There is currently no independent
HTTP transfer-size or history-scan work ceiling.

### Blobs (Artifacts, Evidence, and Workflow Files)

These endpoints provide access to files produced or used by a workflow.

- **Artifacts:** Files produced by a step's execution (e.g., `stdout.txt`).
  - `GET /api/v1/runs/{run_id}/artifacts`
  - `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}`
  - `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}/content`
- **Evidence:** Files that provide proof of execution, often created by the tool itself.
  - `GET /api/v1/runs/{run_id}/evidence`
  - `GET /api/v1/runs/{run_id}/evidence/{evidence_id}`
  - `GET /api/v1/runs/{run_id}/evidence/{evidence_id}/content`
- **Workflow Files:** Declared, human-facing files referenced in a workflow, such as `plan.md` or `report.md`.
  - `GET /api/v1/runs/{run_id}/workflow-files/{workflow_file_id}`
  - `GET /api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content`

Workflow-file response objects use `locator_kind` and `relative_path`.
`locator_kind` is `workspace`, `repository_state`, or
`run`; the relative
locator is a label and must not be resolved by clients. Use the opaque ID and
provided links for retrieval. IDs include locator kind, producing step, field,
and relative locator, never an absolute host path. Retrieval is descriptor-
anchored beneath the configured workspace root, the persisted request's
`repo_path` `.transist` directory, or trusted lineage-root run directory,
does not follow symlinks, and fails closed with `404` for stale, ambiguous,
escaped, or symlinked locators.

### Workflows

Used for inspecting the authored workflow definitions.

- `GET /api/v1/workflows`: Lists all discoverable workflows, including total `step_count`, root-level `root_step_count`, and `is_subflow` metadata.
- `GET /api/v1/workflows/{workflow_ref}`: Retrieves detailed metadata for a workflow, including its inputs, step graph, and `is_subflow` marker. (`{workflow_ref}` is typically `id@version`).
- `GET /api/v1/workflows/{workflow_ref}/source`: Returns the raw UTF-8 YAML source of the workflow.
- `GET /api/v1/workflows/{workflow_ref}/compiled-recipe`: Returns the UTF-8 compiled, internal representation of the workflow used by the runtime.

If either text representation contains invalid UTF-8, the API returns `422 invalid_text_encoding` in the standard problem envelope and does not return source bytes.

Workflow-file metadata remains available when the server is embedded without a configured workspace root. Content reads are then denied, and metadata omits the `links.content` URL. A workflow-file ID is emitted only when Transist can derive an intrinsic host-independent locator; unsafe or ambiguous persisted paths remain visible as metadata but are not retrievable.

### Inspection UI

The server also hosts a simple, server-rendered HTML interface for easy visual inspection. All UI pages are served from the `/inspect/...` path.

- `/inspect/runs`: The main dashboard for listing and filtering runs.
- `/inspect/runs/{run_id}`: A detailed view of a single run. The `Overview` tab now includes `Trust & Provenance`, `Data Locality`, and `Evidence Export` panels, and the `Gates` tab includes approver identity when recorded.
- `/inspect/runs/{run_id}/failures`: Redirects to the run detail `Failures` tab.
- `/inspect/workflows`: A list of all available workflows.
- `/inspect/workflows/{ref}`: A detailed view of a workflow with `Plugins`, `Inputs`, and `Steps` tabs, plus links to the source and compiled recipe.

## Scope

The companion server is intentionally limited to read-only inspection. It **does not** handle:

- Starting or stopping workflows (`transist workflows run`).
- Executing runs.
- Approving gates (`transist runs gate approve`).
- Replaying or retrying runs.
- Installing or managing plugins.

These actions are handled exclusively by the `transist` CLI.

## Error Handling

- **400 Bad Request:** For invalid query parameters, malformed resource IDs,
    wrong blob-family IDs, or malformed workflow references.
- **422 Unprocessable Entity:** When workflow source or compiled-recipe content is not valid UTF-8 text.
- **404 Not Found:** For a missing run, step, workflow, or other resource.
- **500 Internal Server Error:** For an issue reading from storage or a server-side failure.

All error responses use the same `application/json` `{ "error": { "code",
"message" } }` envelope declared by the OpenAPI contract.
