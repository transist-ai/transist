# Secret Resolution and Persistence Boundaries

Workflow authoring names logical refs through invoke-step `with.secret_refs`.

## Resolution Boundary

- the kernel resolves logical secret refs
- plugins do not resolve workflow secret refs for themselves by default
- delivery to adapters happens in memory

## Persistence Boundary

Resolved secret values must not appear in:

- run state
- event logs
- artifacts
- evidence
- persisted request or result dumps

The kernel performs a final redaction pass before durable writes. It replaces
exact raw values and padded or unpadded standard and URL-safe base64 variants.
Plugin artifact and evidence files are staged privately, scanned before
promotion, redacted when valid UTF-8, and rejected when binary content contains
a known variant. This is not a DLP guarantee for arbitrary transformations or
external exfiltration.

## Authoring Implications

- keep logical secret names in the workflow
- keep narrow secret bindings in the plugin action
- inspect persisted run state only for metadata about secret handling, not for secret values
- when troubleshooting dispatch readiness, inspect `preflight.status`, `preflight.checks`, `last_request.metadata.secret_refs`, and `last_request.policy.remote_secret_delivery`
- treat locality denials (`--local-only`) as separate from secret-resolution or remote-secret-delivery denials; inspect `state.json.local_only_mode`, step request `provider_kind`, and `provenance.json.external_transfer_summary`
- treat trusted-execution denials as a third axis; inspect `state.json.trust_decision` and `provenance.json` rather than broadening secret policy
- keep credentials out of prompt files and shell `source://` helpers because fresh run birth can capture those raw workflow-owned bytes under private `workflow_assets/` storage
