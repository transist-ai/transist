# Plugin Manifest Reference (`transist.plugin.v1`)

This is the technical reference for the **`transist.plugin.v1`** schema. It is
for plugin authors and platform engineers packaging first-party, companion, or
customer-owned plugins for a Transist installation. Here, "plugin" means a
versioned integration package, not necessarily a small in-process extension.
Its execution participant may be a script, CLI wrapper, program, HTTP service,
or MCP server. The `plugin.yaml` file describes the participant's identity,
actions, schemas, and how a Transist adapter communicates with it.

## Top-Level Fields

A valid `plugin.yaml` must include the following fields:

- **`schema`**: Must be `transist.plugin.v1`.
- **`id`**: A unique identifier for the plugin (e.g., `git.cli`).
- **`version`**: The plugin's release version (e.g., `0.1.0` in the maintained catalog), independent of the `transist.plugin.v1` schema.
- **`adapter`**: The communication protocol used by the plugin (`process`, `http`, or `mcp`).
- **`entrypoint`**: Details on how to start the plugin (varies by adapter).
- **`package`**: The exact payload files included in the digest, signature, and
  installed plugin tree.
- **`actions`**: A list of the tasks the plugin can perform.
- **`capabilities`**: Optional flags that tell Transist which advanced features the plugin supports.
- **`default_provider_kind`**: (Optional) Manifest-level fallback for action locality. Accepted values: `local` or `external`. When absent, actions that also omit `provider_kind` are treated as `external` (fail-safe).

`id` and `version` must match `^[A-Za-z0-9][A-Za-z0-9._-]*$`: an ASCII
alphanumeric first character followed by any number of ASCII alphanumerics,
`.`, `_`, or `-`. This is enforced at install time before any path is derived
from these identifiers (including plugins uninstall/remove, which cannot
target a path outside `plugins/<id>/<version>`).

## Package Payload

`package.files` is a required, non-empty list of unique, package-relative
regular-file paths. It is the only package-selection mechanism.

- `plugin.yaml` is always included and must not appear in `package.files`.
- List the entrypoint, its local modules and dynamically loaded assets, the
  optional HTTP CA certificate, action schemas, and every local `$ref` target.
- Do not use globs, negation, ignore files, or paths outside the package root.
- Files not listed are not digested, signed, copied, or available to the
  installed plugin. Documentation, tests, repository metadata, and generated
  caches should normally remain unselected.

The selected payload is exactly `plugin.yaml` plus `package.files`. Changing
the manifest, the file selection, or any selected file's bytes changes the
package digest. Independent plugin authors use a distinct version for changed
selected bytes installed in the same home. The maintained repository catalog
retains `0.1.0`: validate and promote each changed signed bundle into a fresh
home, preserving the old home and bundle. Never overwrite installed payloads.
See [Plugin Lifecycle Management](../guides/plugins/plugin-packaging-install-upgrade-removal.md#4-upgrade-a-plugin)
for the upgrade procedure.

## Detached Signatures

Plugin package signing is handled by `transist-admin`, and the runtime
understands detached plugin sidecars and imported public keys.

- Canonical plugin digest: `plugin_dir_sha256(plugin_dir)`.
- Default signature sidecar: `<plugin-dir>.sig` next to the plugin directory.
- Runtime trust-root import: `transist trust import <trust-root-file>` stores a trusted public key under `<home>/trust/keys/<key_id>.pub`.
- Runtime listing and lifecycle: `transist trust list`, `transist trust inspect <key-id>`, `transist trust export <key-id> --output <path|->`, and `transist trust remove <key-id> --yes` manage the local trust store without rewriting historical run decisions.
- `transist-admin plugin sign <plugin-dir> --private-key-file <private-key-file>` writes the default
  sidecar; `--output <signature-file>` overrides that location. Trusted plugin
  install defaults to the same sidecar and may instead use
  `transist plugins install --signature <signature-file>`. An explicit install
  path is verified and persisted in the runtime home without fallback.

Current `transist.signature.v1` shape:

```json
{
  "schema": "transist.signature.v1",
  "sig_alg": "ed25519",
  "digest_alg": "sha256",
  "subject": {
    "kind": "plugin",
    "id": "demo.plugin",
    "version": "0.1.0",
    "digest": "<plugin_dir_sha256 hex>"
  },
  "key_id": "<sha256-of-raw-public-key-hex>",
  "signed_at_ms": 0,
  "signature": "<base64 ed25519 signature over raw digest bytes>"
}
```

Schema file: `schemas/trust/signature.v1.schema.json`

The runtime remains verification-only. Key generation, public-key export,
signing, verify dry-runs, and signature inspection live in `transist-admin`.

---

## Adapter Entrypoints

The `entrypoint` field's structure depends on the `adapter` type.

### 1. `process` and `mcp` Adapters

Used for local binaries and scripts.

- **`command`**: The executable or script to run, relative to the plugin directory. In v1 this must resolve to a file inside the plugin package; Transist does not resolve arbitrary `PATH` commands here.
- **`args`**: A list of command-line arguments.
- **`cwd`**: Legacy optional package-relative working directory for workflows
  without a declared workspace. When a workflow declares `workspace`, runtime
  and preflight execute in that workspace; `cwd` must be omitted or `.` and any
  other value is rejected. Plugin executables still resolve from `command`
  relative to the installed package. Resolve support files through selected
  `package.files`, action schema paths, or other manifest-defined package paths,
  not through the process working directory.
- **`env`**: (Optional) A map of environment variables to set. A complete value
  may be `runtime://temp` or `runtime://temp/<relative-path>` to request an
  invocation-private directory. Each path segment must match
  `[A-Za-z0-9][A-Za-z0-9._-]*`; empty segments, `.`, `..`, trailing slashes,
  backslashes, queries, fragments, and percent encoding are rejected. Tokens
   embedded in a larger string are not expanded.

The first path component `documents` is reserved for managed document copies,
case-insensitively. `runtime://temp/documents` and all its descendants are rejected
at validation/install and dispatch. Directory resolution also rejects symbolic
links, and validates the complete environment before creating any directories.
Use a provider-owned component such as `runtime://temp/provider/cache` instead.
This prevents declarations from mutating a read tree or precreating a required
write output. Existing installed same-version packages are never rewritten.

For process and MCP invocations, the runtime injects an invocation-private
`TMPDIR` after plugin, workflow, user, and secret environment processing. A
declared `TMPDIR` value of `runtime://temp` or `runtime://temp/<relative-path>` selects that directory; any literal
manifest `TMPDIR` remains overridden. Declared child directories exist before
startup with private permissions. `runtime://temp` grants no host authority,
does not name durable storage, and resolves only beneath the runtime-created
root already authorized by the sandbox. Plugins must not rely on a
caller-provided temporary directory. Document every secret environment binding
in the action README; secret values are never stored in workflow, run,
artifact, or evidence data. See [Path and Resource
Semantics](path-and-resource-semantics.md) for the distinction between this
manifest sentinel, capability resources, and action path conventions.

The runtime keeps the selected root alive only through process completion and
output validation/promotion, then removes it on success, failure, timeout,
cancellation, and ordinary preflight cleanup. It reaps stale owner-marked
roots after a crash. Do not place durable provider configuration, caches,
sessions, or authentication state there. A provider credential must use its
declared secret binding and applicable runtime policy instead.

Local entrypoints also receive `TRANSIST_RUNTIME_METADATA_FILE`. Its private JSON
contains runtime/workflow/plugin identity and additive `run.id`, copied exactly
from the invocation's runtime request. This is the current run (a child inside a
module), not a root locator or permission. Preserve the environment variable when
launching a helper that needs it; never forward the durable home or reconstruct
its path. See [APP runtime metadata](app.md#invocation-runtime-metadata) for the
field contract and root-only ledger initialization.

MCP entrypoints implement MCP `2025-06-18` tools over newline-delimited UTF-8
JSON-RPC 2.0 on stdio. Each inbound and outbound message is capped at 1 MiB
excluding LF or CRLF delimiters; `Content-Length` framing is rejected. Servers
must complete `initialize`, receive `notifications/initialized`, provide every
page of `tools/list`, and return a standard `CallToolResult` from `tools/call`.
Only the MCP tools capability is supported. Resources, prompts, sampling,
roots, elicitation, and Streamable HTTP are unsupported.

### 2. `http` Adapter

Used for web services and remote APIs.

- **`base_url`**: A valid `http://` or `https://` URL with a host and no
  userinfo. Malformed, non-HTTP(S), hostless, and userinfo-bearing URLs are
  rejected during installation.
- **`ca_cert`**: (Optional) A plugin-relative PEM certificate or certificate
  bundle path. Each certificate is added as an additional trust root for HTTPS
  requests; normal certificate-chain, expiry, hostname, and signature
  validation remains enabled. Invalid, expired, untrusted, or
  hostname-mismatched certificates fail before the adapter sends a request.

The host HTTP adapter enforces the effective network policy for handshake,
health, invoke, redirect, and cancellation requests. `none` makes no
connection, `loopback` requires every DNS result to be loopback, and
`allowlist` requires the scheme, normalized host, effective port, and TLS
requirement to match. Environment/system proxies are disabled for restricted
modes.

---

## Defining Actions

Actions are the specific tasks that users can call from their workflows. Each action is defined by:

- **`name`**: The unique name of the action (e.g., `exec`, `commit`). Duplicate action names are rejected by runtime manifest validation.
- **`input_schema`**: Path to a JSON schema file describing the required inputs.
- **`output_schema`**: Path to a JSON schema file describing the expected outputs.
- **`bindings`**: (Optional) Extension bindings. For `mcp` plugins, `bindings.tool_name` is required and maps the action to the underlying MCP tool.
- **`prerequisites`**: (Optional) Checks specific to this action.
- **`provider_kind`**: (Optional) Declares whether the action stays on the local machine (`local`) or may transfer data to an external provider (`external`). This is declared by the plugin author and is not inferred from `adapter` or `execution_kind`.

Example:

```yaml
default_provider_kind: external
actions:
  - name: summarize
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    bindings: {}
    provider_kind: external
  - name: render_markdown
    input_schema: schemas/render_input.json
    output_schema: schemas/render_output.json
    bindings: {}
    provider_kind: local
```

Recognized binding keys:

- `tool_name`: required for MCP actions.
- `input_projection`: optional exact string `action_data_v1`; see below.
- `transist_prompt_v1`: enables first-class `prompt` / `prompt_file` fields.
- `transist_skills_v1`: enables first-class `skills` fields.
- `transist_gate_decision_v1`: declares `decision_field`, the optional action
  input that receives one validated `transist.gate_decision.v1` record from a
  workflow's `gate_decision.step` binding.

`transist_gate_decision_v1` is a runtime-owned input binding. Its
`decision_field` must not collide with prompt or skills injected fields, and a
workflow cannot populate it through `with`. The field name must be a literal;
input and variable placeholders cannot redirect the kernel-owned injection at
runtime. The field is absent on the initial iteration when the referenced
manual gate has no durable history. Plugins must
not read `gates/` or `gates/history/` through `request.paths.run_root`; those
paths are invocation-private staging concerns.

Example:

```yaml
actions:
  - name: execute
    input_schema: schemas/execute.input.schema.json
    output_schema: schemas/execute.output.schema.json
    bindings:
      transist_gate_decision_v1:
        decision_field: gate_decision
```

Resolution order:

- action-level `provider_kind`
- manifest-level `default_provider_kind`
- implicit `external` when both are absent

### Action Schema Validation

#### Opt-in Action Input Projection

An action may opt in through the existing bindings map:

```yaml
bindings:
  input_projection: action_data_v1
```

With this binding, process, HTTP, and MCP adapters clone the materialized
action input and remove **only top-level `secret_refs`** before schema
validation and transport. Static readiness and live preflight use the same
projection without resolving or delivering secrets. All other keys remain,
including unknown keys, nested `secret_refs`, and injected prompt, skills, or
manual-gate decision fields. Strict schemas still reject undeclared business
fields; the schema itself is never rewritten or used to infer a projection.
Non-object inputs retain their original shape and are validated normally.

Absent binding preserves legacy action-input bytes, including `secret_refs`.
Present null, non-string, empty, whitespace-padded, or unknown values fail
closed at installation, manifest validation, and adapter loading. The only
accepted value is the exact string `action_data_v1`.

The original TEP request, metadata, secret references, capability authority,
and policy checks are unchanged. Secret delivery still uses `secret_inputs`
and effective policy: process/MCP environment bindings remain separate from
action input; HTTP header bindings remain headers, and HTTP `body_field`
secrets are injected into the projected input before execution schema
validation. Preflight never injects raw body-field secrets. MCP `tools/call`
receives projected `arguments`, not the original request input.

Adopt the binding through the [immutable-package upgrade procedure](../guides/plugins/plugin-packaging-install-upgrade-removal.md#4-upgrade-a-plugin).
Existing packages and prepared runs are not rewritten.

#### Schema Rules

Action schemas use JSON Schema Draft 2020-12. Both the input and output schema
are compiled during manifest validation and plugin installation, so an invalid
schema prevents installation before any action can run.

- Schema paths and transitive `$ref` targets must be regular files inside the
  canonical plugin package root.
- `$ref` may use JSON Pointer fragments and relative package paths only.
  Remote URLs, absolute or `file:` URLs, parent traversal, and symlink escapes
  are rejected without network access.
- `$dynamicRef` is unsupported and rejected before full or partial validation;
  use static package-local `$ref` instead. Property names and literal `enum` or
  `const` values containing `$dynamicRef` remain ordinary data.
- The adapter validates the materialized input before handshake, process spawn,
  or MCP startup. A successful APP result or mapped MCP `CallToolResult` output
  is validated before result data are converted. MCP `structuredContent` is the
  output when present; otherwise the output is `{ "content": [...] }`.
- Invalid input or output maps to `validation_error`. Diagnostics contain only
  the schema file, schema path, and instance path; the invalid value is not
  copied into the diagnostic.

`provider_kind` matters for governed runs:

- `transist runs start ... --local-only`, `transist workflows run ... --local-only`, and `transist workflows prepare ... --local-only` deny any invoke step whose effective `provider_kind` is `external` before dispatch.
- governed runs may record additive `transist.external_transfer.v1` evidence for allowed external calls when a governance mode enables transfer recording.
- local-only runs persist `external_transfer_summary.count = 0` because no external step is allowed to dispatch.

---

## Host Prerequisites

You can define static, machine-readable requirements for the host environment.

- **Plugin-level prerequisites** are checked during installation and before every run.
- **Action-level prerequisites** are checked only when a specific action is about to be executed.

**Supported Categories:**

- `binaries`: Check for installed software presence only.
- `env`: Check for required environment variables.
- `paths`: Ensure specific files or directories exist.
- `commands`: Legacy executable checks that remain parseable but are rejected.

```yaml
prerequisites:
  binaries:
    - name: node
```

Binary `version`, `version_check`, and `commands` fields return
`legacy_executable_prerequisite_unsupported`; Transist never executes them.
Use an explicit invoke step through an installed plugin action for executable
checks. Plugin readiness uses the APP handshake and, when advertised, the APP
health check rather than a prerequisite command.

---

## Secret Bindings

If an action requires a secret (like an API key), you must define how that secret should be delivered to the plugin.

- **`env`**: Delivered as an environment variable.
- **`temp_file`**: Written to a mode-0600 temporary file, and the path is passed to the plugin. The runtime owns the file with RAII and removes it on binding, spawn, protocol, timeout, cancellation, and panic exits.
- **`header`**: (HTTP only) Delivered as an HTTP header.
- **`body_field`**: (HTTP only) Injected into the JSON request body.
- **`arg`**: Passed as a command-line argument.

Workflow `with.secret_refs` must match these declared `secret_inputs` exactly:

- Undeclared secret refs are rejected during compile or preflight.
- Any `secret_inputs.required: true` entry must appear in the workflow's `with.secret_refs`.

Example:

```yaml
actions:
  - name: exec_with_secret_env
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: MY_API_TOKEN
        required: true
        binding:
          type: env
          name: API_TOKEN
```

---

## Plugin Capabilities

Capabilities tell Transist which advanced features your plugin supports.

- **`supports_cancel`**: The plugin can cleanly stop an ongoing action.
- **`supports_health`**: (HTTP only) Transist can check the plugin's health via a `GET /app/health` endpoint.
- **`supports_artifact_hints`**: The plugin can suggest names and paths for output files.
- **`supports_evidence_hints`**: The plugin can provide additional audit-trail information.
