# Safety: Secrets and Policy

Handling sensitive data like API keys, passwords, and tokens is one of the highest-risk parts of automation. Transist is built with strict runtime rules for scoped delivery and redaction of raw secret values it resolves.

## 1. Secret Resolution

Transist handles secrets at the **kernel** level. When an invoke step declares `with.secret_refs`, the runner is responsible for looking up those logical names from the configured provider.

Plugins and tools should not resolve raw secret values themselves. They receive scoped values at the moment they need them, according to adapter policy.

If policy disables secret delivery for the target adapter, Transist denies the step before adapter dispatch. The same denial applies during preflight and cleanup execution.

## 2. The Non-Persistence Rule

The core rule is: **raw secret values resolved by Transist must not be persisted as Transist-managed runtime data.**

Even though Transist records runtime state, it redacts raw secret values it resolved before persisting managed runtime data such as:

- `state.json`
- `events.jsonl`
- Transist-managed artifact or evidence payloads
- Captured stdout/stderr when the raw value is visible to the runtime

Transist replaces every exact raw secret substring and detected padded or unpadded standard and URL-safe base64 variant with `[REDACTED]`. Artifact and evidence files pass through a kernel-owned staging scan before promotion: valid UTF-8 is redacted, binary files containing a known variant are rejected, and accepted hashes are recomputed. Tools and plugins must still avoid emitting transformed, derived, hashed, summarized, or partially rewritten secrets that the runtime cannot recognize automatically.

## 3. Policy: Who can do what?

**Policy** is the layer that decides if an action is allowed. For example, a policy might state:

- "Only allow this managed runtime to start approved workflow refs."
- "Only allow these plugin IDs and adapter kinds."
- "Only allow these logical secret refs before provider lookup."

New homes use governed, default-deny execution. Permissive policy is available
only when an operator explicitly initializes or configures standalone or
development mode; that choice is visible in run state and inspection output.

Local policy can restrict workflow refs, adapters, plugins, plugin actions, and logical secret refs:

```yaml
policy:
  profile: permissive
  allow:
    workflows: ["demo.workflow@0.1.0"]
    adapters: ["process"]
    plugins: ["shell"]
    actions:
      - plugin: shell
        action: exec
    secret_refs: ["API_TOKEN"]
```

### Default-Deny Capability Grants

Governed execution uses `policy.profile: allowlist` with version-pinned plugin
grants and exact resource capabilities. The runtime generates requests from the
pinned plugin/action, provider locality, logical secret refs, effective
filesystem/network policy, and manual gates. A missing or partial grant fails
with `capability_not_granted` before secret lookup or adapter preflight:

```yaml
policy:
  profile: allowlist
  allow:
    capabilities:
      - capability: filesystem
        resource: workspace:///work/repo
        operations: [list, read, create, write, delete, rename]
      - capability: filesystem
        resource: run://artifacts
        operations: [create, write]
      - capability: filesystem
        resource: run://evidence
        operations: [create, write]
    plugin_grants:
      - plugin: shell@0.1.0
        actions: [exec]
```

`plugin_grants` cover the matching invocation's `adapter`, `plugin`, `action`,
and `provider` requests.

Supported namespaces are `adapter`, `plugin`, `action`, `provider`, `secret`,
`filesystem`, `network`, and `approval`. Wildcards are not supported. The
structured filesystem and network policies remain independent ceilings and a
capability grant cannot widen them. Requested and granted capabilities, the
policy profile/id, and denial reason are retained in the request and inspection
state without secret values.

Filesystem grants use only operations allowed by the effective policy: a
read-only workspace requests `list` and `read`, never mutation operations.
External filesystem grants are canonical absolute `file://` resources. Network
grants include the HTTP plugin base URL and every effective allowlist destination;
redirect targets require their own matching grant. Governed invokes must have a
pinned plugin version. Missing versions and malformed persisted requests deny
with `capability_not_granted` rather than receiving an inferred version.

For new workflows, `workspace { path, access }` determines the canonical
workspace capability. It is resolved from immutable inputs, commonly
`{{inputs.repo_path}}`, and persisted for recovery. The workflow source and CLI
CWD do not grant workspace authority. A child subflow keeps the parent path and
may only narrow access; use exact read-only external grants for secondary
context rather than worktree modes or parent-directory grants.

Manual gates request `approval:request` when opened and `approval:decide` only
when a decision is recorded. A grant for one operation does not authorize the
other.

- If an allowlist field is absent, that dimension keeps permissive behavior under `profile: permissive`.
- If an allowlist field is present but empty, that dimension becomes deny-all.
- Secret-ref allowlists are checked before secret lookup, so denied refs fail without resolving raw values.

## 4. Local-Only Mode and External Transfers

Transist also supports a run-time locality control: `--local-only`.

- Locality is declared by the plugin manifest's `provider_kind` (`local` or `external`).
- Locality is **not** inferred from `adapter` or `execution_kind`.
- Under `--local-only`, any invoke step whose effective `provider_kind` is `external` is denied before dispatch.
- HTTP steps under `--local-only` also have a non-widenable loopback network
  ceiling. An authored `none` policy remains stricter; `any`, allowlists, DNS,
  and environment proxies cannot widen the ceiling.
- The denial follows the normal policy path: the run emits `policy.denied`, the step fails through the regular failure path, and `provenance.json` is updated before the kernel bails.

For allowed external-provider calls in governed modes that activate transfer recording, the runtime records additive `transist.external_transfer.v1` evidence containing destination, adapter kind, provider/provider_kind, request content hash, and policy decision. Run state, provenance, and run-detail inspection also expose `runtime_mode`, `policy_profile`, and `capability_enforcement`.

Important scope note:

- This governed-run support does **not** claim DLP-grade redaction.
- `redaction_applied` on external-transfer evidence is always `false`.
- `--local-only` proves dispatch posture and evidence shape; it does not create a legal-compliance guarantee.

### Policy checks at preflight and dispatch

The policy layer is consulted at **two** points in the run lifecycle for every invoke step:

1. **Preflight** — before the dispatch loop starts. If any step is denied here, the whole run is marked `failed` before any work is done. The step remains in `runnable` state so the operator can fix the policy configuration and resume the run.
2. **Runtime dispatch** — immediately before each step is sent to its adapter. A denial here marks the step `failed`, emits a `policy.denied` event, and fails the run through the normal step-failure path.

### policy.denied event

When policy blocks a step at dispatch time, Transist records a `policy.denied` runtime event containing:

| Field | Description |
|---|---|
| `reason_code` | Stable machine-readable denial code (e.g. `RAC_POLICY_SECRET_REF_NOT_ALLOWED`) |
| `adapter_kind` | The adapter kind of the denied step |
| `plugin_id` | The plugin id of the denied step |
| `workflow_ref` | The workflow id of the run |
| `secret_ref` | Set when denial is due to a disallowed secret reference; otherwise `null` |

For local-only denials, the event also carries the normal `adapter_kind` and `plugin_id` fields; the stable reason code is `local_only_denied_external`.

## 5. Safe Delivery

Each adapter handles secret delivery in a way that fits its environment:

- **`process`**: Passes secrets as temporary environment variables or protected files when `policy.remote_secret_delivery.process` allows it.
- **`http`**: Can inject secrets into headers or request bodies when policy allows remote delivery.
- **`mcp`**: Can deliver secrets through configured environment or argument bindings when policy allows remote delivery.

The core principle remains the same: secrets are delivered just-in-time, scoped to the adapter call, and should not become durable run data. `temp_file` bindings use RAII-owned mode-0600 files that are removed on every adapter exit path.

### Remote secret delivery policy

Local runtime config can independently allow or deny adapter families from receiving resolved secret values:

```yaml
policy:
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
```

- `process: false` denies any invoke or cleanup step that uses `with.secret_refs` with the `process` adapter.
- `http: false` and `mcp: false` keep the existing default deny posture for those adapter families.
- Preflight records the denial as a `remote_secret_policy` failure so operators can correct config before retrying.

### Child environment isolation

Process and MCP child processes do not inherit the full parent environment by default. The runtime clears the child environment and re-adds only:

- `policy.child_env.allowlist` vars copied from the parent environment
- plugin manifest `entrypoint.env` vars
- Transist runtime vars such as `TRANSIST_WORKSPACE_DIR`, `TRANSIST_ARTIFACTS_DIR`,
  and `TRANSIST_EVIDENCE_DIR`. Plugins
  must not depend on durable run storage. Use the artifact/evidence directories
  or `TRANSIST_RUNTIME_METADATA_FILE` for invocation-scoped output and metadata.
- declared secret bindings for the step

`runtime://temp` and `runtime://temp/<relative-path>` declarations in a process or MCP manifest are resolved
after child-environment clearing, before runtime variables and secret bindings.
They do not expand inside larger strings and do not create a filesystem policy
grant. `TMPDIR` is set last to the declared runtime-temporary directory, or the
invocation root when no declaration selects one.

When a manifest redirects `HOME`, a provider configuration directory, or a data
directory to `runtime://temp`, the provider's normal lookup no longer reaches
host login state. An external filesystem grant does not undo that environment
redirection. This is not complete same-user host read isolation on macOS, so a
plugin must not probe alternate host paths for credentials. Credentialed plugins
must declare and receive an appropriate secret binding; never work around it by
writing a credential to a workspace, artifact, evidence, or temporary provider
directory.

Example:

```yaml
policy:
  child_env:
    inherit: false
    allowlist: ["PATH", "HOME", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR"]
```

Set `policy.child_env.inherit: true` only when a plugin must see the broader parent environment.

---

### Related

- [Secrets and Runtime Policy in Practice Guide](../../guides/trust/secrets-and-runtime-policy-in-practice.md)
- [TEP (Transist Execution Protocol) Reference](../../reference/tep.md)
- [Plugin Manifest Reference](../../reference/plugin-manifest.md)

**Navigation:**

- Previous: [Trust: Determinism and Auditability](01-determinism-and-auditability.md)
- Next: [Storage Model](03-storage-model.md)
