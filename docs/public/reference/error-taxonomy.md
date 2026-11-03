# Reference: Error Taxonomy

Transist uses a standardized set of error codes to help you quickly identify what went wrong during a workflow run or an API request. These codes are categorized by where they occur: in the execution engine or in the companion inspection server.

## Execution Error Codes

These errors occur during the lifecycle of a workflow. They are produced by the Transist kernel and adapters.

| Error Code | Meaning |
| :--- | :--- |
| **`configuration_error`** | A problem with your Transist settings or home directory setup. |
| **`auth_error`** | Failed to authenticate with a plugin or external service. |
| **`secret_resolution_error`** | Could not find or resolve a required secret. |
| **`validation_error`** | The workflow or plugin manifest contains invalid YAML or data. |
| **`subflow_input_binding_failed`** | A parent subflow step produced invalid child inputs before the child run could start. |
| **`timeout_error`** | A step exceeded its configured maximum duration. |
| **`cancelled`** | The run was explicitly stopped by a user or supervisor. |
| **`rate_limit_error`** | An external service (like an AI model or API) is throttling requests. |
| **`transport_error`** | A network problem occurred while talking to an adapter or plugin. |
| **`protocol_error`** | The communication or process lifecycle contract between the runtime and a plugin was violated. |
| **`tool_execution_error`** | The script or binary inside a plugin failed (e.g., non-zero exit code). |
| **`adapter_internal_error`** | An unexpected crash or failure inside the adapter itself. |
| **`policy_denied`** | A runtime policy denied the step before adapter execution. |
| **`capability_not_granted`** | The governed allowlist did not contain an exact grant for a normalized capability request. |

`workflows check` reports these conditions as structured readiness checks rather
than creating an execution failure. It always returns `run_created: false`; use
`workflows authorize` only for the exact missing capabilities it reports, then
run the check again. Once a run exists, inspect persisted policy failures with
`runs show`, `runs preflight`, and `runs capabilities`.

## Process Lifecycle Failures

Process exit grace violations use the existing `protocol_error` code and exit
code `2`: `process boundary did not exit within 250 ms after terminal APP result`.
Their lifecycle evidence reason is `process_exit_grace_exceeded`, separate from
pre-terminal `timeout_error`. They require manual attention and are not
automatically retried. Cleanup failure uses `adapter_internal_error` with
`cleanup: unconfirmed` evidence; observed cancellation takes precedence.
Invalid terminal identity retains `protocol_error`; schema-invalid successful
output retains `validation_error` after confirmed termination rather than being
masked by an exit-grace violation. Failed-output schema invalidity removes output
and hints without replacing a legitimate provider failure on a clean exit.
Unavailable lifecycle snapshots are explicitly diagnosed with
`process_lifecycle_snapshot_observation_failed`; raw OS errors are not persisted.
See [APP process lifecycle](app.md#process-lifecycle-after-a-terminal-result).

## Trusted-Execution Verification Reasons

Trusted execution currently uses these stable verification reason codes on persisted `trust_decision` records and refusal messages:

| Reason Code | Meaning |
| :--- | :--- |
| **`unsigned`** | The workflow or plugin is missing a detached `.sig` sidecar. |
| **`untrusted_key`** | The sidecar was signed by a key that is not trusted by the active runtime home. Import the publisher's public trust root with `transist trust import <trust-root-file>`. |
| **`digest_mismatch`** | The executable input bytes changed after signing. |
| **`bad_signature`** | The detached signature does not validate against the trusted public key. |
| **`malformed_sig`** | The sidecar or trust input is malformed. |
| **`test_version_bypass`** | Trusted execution was enabled, but a `v0` workflow or explicit `0` / `v0` plugin test marker was admitted through the development-only bypass. Ordinary SemVer releases never use this bypass. |

## Local-Only Policy Reasons

These stable reason codes can appear on `policy.denied` events, preflight failures, and denied provenance when local-only mode blocks a step before dispatch.

| Reason Code | Meaning |
| :--- | :--- |
| **`local_only_denied_external`** | The run was started with `--local-only`, and the target plugin action's effective manifest `provider_kind` was `external`. |

## Local Allowlist Policy Reasons

These stable reason codes can appear on `policy.denied` events, preflight failures, and denied provenance when local runtime policy allowlists block a step before dispatch.

| Reason Code | Meaning |
| :--- | :--- |
| **`local_allow_workflow_not_allowed`** | The workflow ref is outside `policy.allow.workflows`. |
| **`local_allow_adapter_not_allowed`** | The adapter kind is outside `policy.allow.adapters`. |
| **`local_allow_plugin_not_allowed`** | The plugin id is outside `policy.allow.plugins`. |
| **`local_allow_action_not_allowed`** | The `plugin + action` pair is outside `policy.allow.actions`. |
| **`local_allow_secret_ref_not_allowed`** | One or more logical secret refs are outside `policy.allow.secret_refs`. |

---

## Failure Classes (v1)

Failure classes are kernel-level categories attached to failed step executions and inspection failure records. They extend the stable execution error codes without replacing them.

| Failure Class | Mapped Error Codes | Default Flags |
| :--- | :--- | :--- |
| **`workflow_definition_error`** | `configuration_error`, workflow-side `validation_error` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`input_validation_error`** | invoke step `validation_error`, `subflow_input_binding_failed` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`adapter_internal_error`** | `adapter_internal_error` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`tool_error`** | `tool_execution_error` | `retryable=true`, `safe_to_retry=false`, `requires_manual_attention=false` |
| **`protocol_error`** | `protocol_error` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`timeout`** | `timeout_error` | `retryable=true`, `safe_to_retry=false`, `requires_manual_attention=false` |
| **`authorization_error`** | `auth_error` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`secret_resolution_error`** | `secret_resolution_error` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`rate_limit_error`** | `rate_limit_error` | `retryable=true`, `safe_to_retry=true`, `requires_manual_attention=false` |
| **`transport_error`** | `transport_error` | `retryable=true`, `safe_to_retry=false`, `requires_manual_attention=false` |
| **`cancelled`** | `cancelled` | `retryable=true`, `safe_to_retry=true`, `requires_manual_attention=false` |
| **`policy_error`** | `policy_denied` | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |
| **`unknown_error`** | Any unmapped code | `retryable=false`, `safe_to_retry=false`, `requires_manual_attention=true` |

For `tool_error`, `timeout`, and `transport_error`, `safe_to_retry` is recomputed from observed state mutations when a failure record is emitted.

---

## Companion Server API Error Codes

These codes are returned by the `transist-server` when an HTTP request to the inspection API fails.

| Error Code | Meaning |
| :--- | :--- |
| **`invalid_request`** | The HTTP request was malformed or missing parameters. |
| **`invalid_text_encoding`** | Workflow source or compiled-recipe content is not valid UTF-8; no source bytes are returned. |
| **`resource_not_found`** | The requested run, workflow, or artifact does not exist. |
| **`unsupported_query_value`** | A search filter or query parameter is invalid. |
| **`storage_query_failure`** | A technical error occurred while reading runtime storage in the effective Transist home. |
| **`internal_server_error`** | An unexpected error occurred inside the companion server. |

---

### Key Distinctions

- **Execution Errors** are about *doing* work. They affect the outcome of your workflows.
- **Server Errors** are about *viewing* work. They only affect your ability to inspect runs through the API or UI.
