# Workflow Schema Reference (`transist.flow.v1`)

This is the technical reference for the **`transist.flow.v1`** YAML schema. It
describes the current pre-1.0 authoring contract; the identifier is versioned,
but no surface is promised stable before `1.0.0`.

## Top-Level Structure

A valid workflow file must follow this basic structure:

```yaml
schema: transist.flow.v1
id: my_unique_workflow_id
version: 0.1.0
plugins: {} # Required: Exact package versions referenced by `uses`.
inputs: {}  # Optional: What data does this workflow need?
context: {} # Optional: Immutable reusable values.
artifacts: {} # Optional: Named run or repository artifact paths.
documents: {} # Optional: Root-lexical bounded managed documents.
profiles: {} # Optional: Reusable defaults for one exact action.
modules: {} # Optional: Package-local reusable child workflows.
workspace: {} # Optional: Declared execution workspace for the run.
filesystem_policy: {} # Optional: Workflow-level filesystem policy override.
network: {} # Optional: Workflow-level network policy override.
steps: []   # Required: What actions should be performed?
cleanup: [] # Optional: What should happen when the workflow finishes?
```

### Required Fields

- **`schema`**: Must be `transist.flow.v1`.
- **`id`**: A unique identifier for this workflow.
- **`version`**: The release of this workflow definition, independent of its schema identifier. Maintained Transist catalog workflows and modules use `0.1.0`. For changed catalog bytes, retain that release, record the revision/digest/signature, and validate and promote the reviewed signed bundle into a fresh home. Preserve the old home/bundle for pinned runs and rollback; never overwrite installed same-version bytes. Independent repositories follow their own version policy, publishing a distinct `<new-version>` when that policy requires it.
- **`steps`**: A list of at least one step.

### Identifier Rules

`id`, `version`, step `id`, cleanup step `id`, and gate `id` (steps use their
own `id` as the gate id) must match `^[A-Za-z0-9][A-Za-z0-9._-]*$`: an ASCII
alphanumeric first character followed by any number of ASCII alphanumerics,
`.`, `_`, or `-`. This is enforced at compile time (before any path is
derived from these identifiers), at plugins install time, and at run-birth
time for a caller-supplied run reference. It rejects an empty value, `.` or
`..`, any path separator (`/` or `\`), absolute paths, embedded whitespace,
and non-ASCII characters. An authored value is used verbatim — identifiers are
never trimmed — so leading/trailing whitespace or a whitespace-only value is
rejected rather than silently normalized.

### Reserved Version: `v0`

`version: v0` marks a workflow as a **test workflow**.

- The compiled registry recipe `registry/recipes/<id>-v0.yaml` may be overwritten in place.
- Existing `v0` runs may continue from a manually patched run-scoped `recipe.yaml`.
- `transist runs sync-recipe <run-id>` can refresh a `v0` run snapshot from the current compiled registry recipe without hand-editing YAML.
- Plugin asset locks are bypassed for `v0` runs.
- `prompt_file` asset locks are still enforced.
- Runtime policy checks are still enforced.
- `v0` is for local iteration only. Do not treat it as a production workflow version.

## Managed Document Declarations

Root workflows may declare immutable storage bounds, inherited lexically by
package-local modules. Modules cannot declare or shadow documents. Names use the
identifier rule above; declarations are separate from invoke `with` data and
from compiler-owned deferred dependency metadata or capability authority.

```yaml
documents:
  notes:
    kind: file
    max_bytes: 1048576
    initial: "Literal UTF-8 notes\n"
  attachments:
    kind: tree
    max_bytes: 10485760
    max_files: 100
```

`max_bytes` is a positive aggregate business-byte limit. Trees additionally require
positive `max_files` and cannot use `initial`; files cannot declare `max_files`.
File `initial` is literal UTF-8 content bounded in **bytes**, not a source path or
template. Initialization belongs to atomic root-run birth. A document without an
initial/published revision has no head. See [managed storage](storage-layout.md#managed-documents)
for immutable revision records, structural tree bounds, publication and recovery.

### Process Document Bindings

Use a reserved object recursively inside process invoke `with` action data:

```yaml
steps:
  - id: generate
    uses: process:reports/write
    with:
      output_file: {$document: notes, access: write}
  - id: consume
    uses: process:reports/read
    with:
      context_files:
        - {$document: notes, access: read}
```

The selected plugin action must accept an ordinary string path at that input
location and handle files outside the workspace. `read` supplies a private,
sandbox-enforced read-only copy of the latest committed revision. `write`
supplies a new, absent path that the plugin must create (including the root
directory of a tree). `update` copies latest committed bytes into a writable
output location. Read/update of an unpublished document fails before transport.
Repeated reads and one writer per name are supported in one invocation; multiple
write/update bindings for the same name are rejected. Modules refer to root names
directly, without forwarding filesystem paths through module inputs.

Bindings support **process actions only**; HTTP/MCP reject them explicitly while
continuing to accept ordinary structured dataflow. Bindings cannot appear in
control/gate data, inputs/defaults, context/profile declarations, executable
identity, `command`, `source`/`source_file`, `prompt_file`, secret references,
workspace or working-directory selectors. Input/variable/step-output objects
cannot introduce bindings dynamically. Read bindings may require an exact document
review using `reviewed_by`; write/update cannot carry that key.

Known input-schema types must allow a string. Actual path constraints and live
probes are reported as `deferred_to_dispatch`; static contracts, policy, authority,
prerequisites, and secret checks still apply before birth. Full materialized input
validation runs before the first consumer probe or external effect. Every output
must exist after schema-valid success. Limits, regular-file/directory rules,
coherent tree capture and raw/encoded known-secret checks apply to the entire batch
before its first blob is persisted. Failed/canceled attempts do not advance heads.

Copies are invocation-private, outside the durable home and workspace; plugins
cannot mutate stored prior revisions. Durable requests/results use logical document
references, not expired staging paths. Exact result path values become binding
objects; nested path strings use `$document:<name>` labels (not a resolvable URI).
Reported text artifact/evidence metadata is normalized too. Business document bytes
are preserved exactly; secret-bearing bytes are rejected rather than redacted.
Resume retains committed revisions; replay initializes a separate namespace and
reruns producers. Repository deliverables remain in the workspace.

### Document Review Gates

```yaml
steps:
  - id: review_notes
    control: control.gate.manual
    with:
      gate_id: operator-review
      review_documents: [notes]
  - id: consume_notes
    uses: process:reports/read
    with:
      context_files:
        - {$document: notes, access: read, reviewed_by: review_notes}
```

`review_documents` is a duplicate-free array of literal declared document names,
reserved for manual gates. Unknown names, malformed requests, unknown gate steps,
gates that do not review that document, and cross-recipe gate references fail
compilation. `reviewed_by` names the **step ID**, not `gate_id`. A module may use
shared root documents but must declare its own lexical gate; an ancestor or sibling
gate does not authorize its reads.

The gate snapshots exact committed revisions when it first waits. Names, byte/file
counts, revision IDs, digests and content links appear in inspection. Decisions
retain the exact request in immutable history. Restarting while waiting retains
the original content even if a newer revision is committed. Missing published
content makes the gate fail durably rather than asking for an empty review.

Before a reviewed read's private copy, input validation or first consumer probe,
the runtime checks the current gate approval against the selected latest revision's
ID **and** digest. Updating after approval blocks the consumer, including replacing
content with identical bytes in a different revision. Use explicit workflow control
to revisit the gate for another review. Loop/child reruns require a current approval;
rejection cannot authorize a consumer. This is document review, not a snapshot or
approval of arbitrary workspace changes. HTTP/MCP document paths remain unsupported.

## Detached Signatures

Workflow-definition signing is handled by `transist-admin`, and the runtime
shares the digest and verification contract with that CLI.

- Canonical workflow-definition digest: `workflow_definition_sha256(<flow.yaml>)`.
- The digest covers the **full authored definition set** rooted at the top-level workflow file:
  - the workflow file itself
  - every declared package-local module file (`modules.<name>.path`)
  - every referenced `prompt_file` target (literal paths only; see below)
  - every referenced `source://` helper script
- Each file is LF-normalized and hashed individually; the per-file digests are combined in sorted path order. Capture holds one descriptor for the top-level workflow root and opens every file and directory component relative to it without following symlinks. Fixed macOS `/tmp` and `/var` root aliases are normalized before that walk; operator-selected symlink components and paths outside the root are rejected.
- `prompt_file` paths with template placeholders (`{{inputs.*}}` / `{{vars.*}}`) are not resolvable at sign time and are refused with an explicit error. Use a literal path for any `prompt_file` that participates in a signed workflow definition. Runtime `prompt_file` asset locks still guard resolved-file drift at run creation time independently of the signature.
- **Sibling-directory subflows**: when a workflow uses parent-relative paths such as `subflow.path: ../sibling/workflow.yaml`, signing requires placing the top-level `workflow.yaml` at the enclosing directory level so that the sibling resolves inside the workflow root. The digest walker's escape guard is canonical-path containment; a parent-relative path that resolves outside the workflow root is rejected.
- Runtime verification preserves this authored-definition digest and does not substitute the compiled `recipe.yaml` snapshot hash.
- Trusted verification and compilation consume the same immutable in-memory capture; compilation does not independently re-read signed definition files after verification.
- Default signature sidecar: `<flow.yaml>.sig` next to the authored workflow file.

## Package Asset Safety

`prompt_file`, `subflow.path`, and `source://` assets resolve relative to the
workflow file that declares them. Validation, packaging, signing, compilation,
and installation reject absolute paths, traversal, directories, and symlinks in
any path component. Keep package assets beneath the top-level workflow root.
`repo://` is an action-input convention, not a workflow asset or capability
resource. Its resolver is action-specific; consult the action schema and
[Path and Resource Semantics](path-and-resource-semantics.md). It never grants
authority beyond the canonical runtime-owned workspace.

- `transist-admin workflow package` semantically validates the rewritten workflow, validates its canonical version identifier, signs and self-verifies the rewritten in-memory capture, and atomically publishes with no-clobber semantics after syncing nested package directories.
- Authoring and signing resolve the workflow's canonical source location and
  contained package assets. Those source reads are independent of the execution
  workspace and do not grant filesystem authority.
- Fresh run birth also captures resolved source-backed prompt and `source://` assets referenced by the recipe's direct steps and cleanup entries as private, content-addressed files under the run root. This capture is atomic with the initial run record and is not an inspection or export surface.
- Snapshot-backed runs use captured prompt and `source://` bytes during
  preflight, dispatch, cleanup, resume, and replay. Legacy records retain their
  authored-source behavior.
- Runtime trust roots: `transist trust import <trust-root-file>` stores trusted public keys under `<home>/trust/keys/<key_id>.pub`, `transist trust inspect <key-id>` loads one record, `transist trust export <key-id> --output <path|->` re-emits one stored record, and `transist trust remove <key-id> --yes` removes one local record for future verification only.
- `transist-admin workflow sign <flow.yaml> --private-key-file <private-key-file>` writes the default
  sidecar; `--output <signature-file>` overrides that location. This direct
  sidecar flow is standalone/development compatibility behavior. Governed
  delivery uses package-contained signature material and
  `transist workflows install --package <package-directory>`; the runtime does
  not select an external workflow signature during governed installation.

Current `transist.signature.v1` shape for workflows:

```json
{
  "schema": "transist.signature.v1",
  "sig_alg": "ed25519",
  "digest_alg": "sha256",
  "subject": {
    "kind": "workflow",
    "id": "demo.workflow",
    "version": "0.1.0",
    "digest": "<workflow_definition_sha256 hex>"
  },
  "key_id": "<sha256-of-raw-public-key-hex>",
  "signed_at_ms": 0,
  "signature": "<base64 ed25519 signature over raw digest bytes>"
}
```

Schema file: `schemas/trust/signature.v1.schema.json`

Current `transist.trust_root.v1` shape:

```json
{
  "schema": "transist.trust_root.v1",
  "name": "test-org",
  "key_id": "<sha256-of-raw-public-key-hex>",
  "public_key": "<base64 raw ed25519 public key>"
}
```

Schema file: `schemas/trust/trust-root.v1.schema.json`

---

## Host Prerequisites

Use the `prerequisites` block for static host checks that must pass before the
workflow is allowed to run. Binary presence, environment presence, and path
presence checks do not execute host commands.

```yaml
prerequisites:
  binaries:
    - name: gh
  env:
    - name: GITHUB_TOKEN
      required: true
  paths:
    - path: scripts/setup.sh
      kind: file
```

The legacy `commands`, binary `version`, and `version_check` fields remain
parseable so old definitions fail deterministically, but evaluation returns
`legacy_executable_prerequisite_unsupported` and never starts a process. Move
executable checks into an explicit invoke step through an installed plugin
action, for example `uses: process:shell/exec`.

---

## Defining Inputs

Inputs allow you to pass data into your workflow at run time.

```yaml
inputs:
  user_name:
    type: string
    required: true
    description: "The name of the user to greet."
  is_dry_run:
    type: boolean
    default: false
```

**Supported Types:** `string`, `number`, `boolean`, `object`, `array`.

If you declare a `default`, it must match the declared input `type`.

At runtime, missing optional inputs with no declared default are still bound as `null`. This is
stable for both root workflows and embedded subflows. Later invoke-step request validation may
still reject that `null` if the target action schema requires a concrete value.

---

## Reusable Declarations

`context` declares immutable values that can be expanded in compiler-owned
binding surfaces. Values may be YAML literals, including nested objects and
arrays, and `{{inputs.name}}` templates only. Variables, step outputs, context
references, and artifact references are rejected so declarations cannot change
after a run starts.

`artifacts` declares typed paths with a `run` or `repository` scope. Its `path`
is a literal string or an input template; it cannot depend on variables or step
outputs. Scope is type-checked authoring metadata in this compiler slice.

```yaml
context:
  release_label: "{{inputs.release}}"
  summary:
    title: Release notes
artifacts:
  notes:
    scope: repository # run | repository
    path: "docs/releases/{{inputs.release}}.md"
```

Use `{{context.name}}` and `{{artifacts.name}}` in `with`, `set`, subflow
inputs, and first-class prompt fields. Exact references preserve structured
context values; interpolated references require a scalar context value.

`profiles` supply reusable invoke defaults, but each profile is bound to exactly
one valid `uses` target. A step that selects a profile must still declare the
same `uses`; a profile never selects or changes an action for the step. Profile
and step values conflict if they set the same `with` key or both define prompts,
skills, filesystem policy, or network policy.

```yaml
profiles:
  announce:
    uses: process:shell/echo
    with:
      prefix: "{{context.release_label}}"

steps:
  - id: publish_notice
    uses: process:shell/echo
    profile: announce
    with:
      destination: "{{artifacts.notes}}"
```

Declaration source and profile-referenced workflow assets participate in the
workflow-definition digest. Changing a declaration or its referenced prompt,
skill, or `source://` asset therefore changes the signed definition digest.

---

## Declaring the Execution Workspace

Use `workspace` to name the one repository or directory where a workflow runs:

```yaml
inputs:
  repo_path:
    type: string
    required: true

workspace:
  path: "{{inputs.repo_path}}"
  access: full # full | read_only
```

`workspace.path` is either an absolute literal or exactly one required string
input reference such as `{{inputs.repo_path}}`; interpolation, relative paths,
variables, and step outputs are rejected. At fresh run birth the value must
exist, be a directory, canonicalize successfully, and not overlap the Transist
home. The canonical path is persisted in the run recipe and state.

`full` permits reads and mutations inside that workspace. `read_only` permits
reads and listing but denies workspace mutations. A workspace declaration cannot
coexist with top-level `filesystem_policy.workspace_access`. Global and
step-level filesystem policy may narrow access, but cannot widen a declared
`read_only` workspace.

The workflow source directory, detached prompts, skills, and `source://` assets
are package and trust inputs, not an execution workspace or filesystem grant.
Changing the CLI working directory never changes a declared workspace. For
older workflows and persisted runs without `workspace`, the runtime retains the
legacy CLI-directory fallback. Process and MCP runtime and preflight processes
use the declared workspace as their actual working directory. A plugin's
package-local `entrypoint.cwd` must therefore be omitted or `.` for these runs;
other values are rejected rather than interpreted relative to either the
workspace or package. The executable and support assets remain package inputs
resolved through `entrypoint.command`, `package.files`, schema references, and
other manifest-defined package paths.

For a secondary repository or other context, keep the primary `repo_path` as
the only workspace and add the exact existing secondary path through an
input-bound `filesystem_policy.read_allow_paths` entry. It is read-only context,
not a second workspace: writable secondary paths, parent-directory grants, and
worktree modes are unsupported.

---

## Step Types

Every step in the `steps` list must be one of four types: **Invoke**, **Control**, **Subflow**, or **Module**.

## Declaring Plugin Dependencies

Every plugin used by an invoke or cleanup step is declared once at the workflow
root with an exact SemVer version. Ranges, `latest`, and omitted versions are
not supported in new workflow source.

```yaml
plugins:
  shell: 0.1.0
  jira.rest: 0.1.0
```

The compiler rejects undeclared and unused entries. During home-aware validation
it resolves each declared version against the installed manifest, verifies that
the action exists, and verifies that the `uses` adapter matches the manifest.
The compiled recipe retains the resolved adapter, plugin, action, version, and
package digest for recovery and replay.

### 1. Invoke Step (The Workhorse)

Used to perform an action from a plugin.

- **Required Fields:** `id`, `uses`.
- **Optional Fields:** `with` (parameters), `prompt`, `prompt_file`, `skills`, `gate_decision`, `set`, `filesystem_policy`, `network`.

`uses` has the exact form `<adapter>:<plugin>/<action>` and selects one action
from a declared plugin dependency. For example:

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      command: echo
      args: ["Hello, {{inputs.user_name}}!"]
```

`adapter`, `plugin`, `action`, `plugin_version`, and `execution_kind` are not
valid authored fields. The compiler rejects each with a stable error directing
authors to `uses: <adapter>:<plugin>/<action>`. The compiled recipe retains the
resolved runtime descriptor and plugin pin for recovery and replay.

#### Manual-gate decision binding

An invoke step can request the latest immutable decision from one manual-gate
step in the same workflow or module:

```yaml
steps:
  - id: implement_fix
    uses: process:agent/execute
    gate_decision:
      step: review_implementation
  - id: review_implementation
    control: control.gate.manual
    with:
      gate_id: implementation-review
```

`gate_decision.step` must name a `control.gate.manual` step in the same
compiled recipe. Compilation freezes the target step id, its effective
`gate_id`, and the receiving action field declared by that action's
`transist_gate_decision_v1` manifest binding. The action must not set that
field through `with`.

Before an invocation is staged, the runtime resolves the durable gate history
for that frozen identity. When no matching decision exists, the receiving field
is omitted. Runtime injection, rather than `request.paths.run_root`, is the
only supported way for a plugin to receive a gate decision.

`filesystem_policy` has two concerns:

```yaml
filesystem_policy:
  # Controls access INSIDE workspace_root.
  workspace_access:
    mode: full # full | read_only (custom is rejected with filesystem_custom_mode_unsupported)

  # Additive grants for paths OUTSIDE workspace_root.
  # Each entry must be absolute (/…) or home-relative (~/…).
  read_allow_paths:
    - /etc/gitconfig
    - ~/.gitconfig
    - ~/.config/git
    - ~/.ssh
  write_allow_paths:
    - ~/.cache/git
```

#### `workspace_access` rules

- Missing `filesystem_policy` falls back to the workflow-level policy, then the global `transist/settings.yaml` policy, then builtin `workspace_access.mode: full`. A declared `workspace.access: read_only` caps the final result at `read_only`.
- `full` allows read/list/create/write/delete/rename inside the workspace.
- `read_only` allows read/list inside the workspace and denies workspace mutation.
- `custom` is **not supported**. The runtime rejects it before dispatch with the `filesystem_custom_mode_unsupported` reason code on both Linux and macOS. Workspace-relative glob allow-lists are not enforced by either sandbox backend; use `read_only` for workspace write protection and the top-level `read_allow_paths` / `write_allow_paths` for external path grants.
- **Composition**: access is monotonic across global, workflow, and step policy.
  Any `read_only` declaration keeps the effective mode read-only; a narrower
  scope cannot widen a broader restriction to `full`.

#### Plugin `PATH` inside the sandbox

Sandboxed plugins inherit the operator's `PATH` verbatim, but `PATH` does not
grant filesystem access. On Linux (`bwrap`), binaries under the fixed system
mounts are reachable; a user-installed binary may still be unreachable when
its `PATH` directory is outside those mounts. Grant that directory explicitly
with `read_allow_paths`.

#### Top-level `read_allow_paths` / `write_allow_paths` (external grants)

These lists grant **additive** access to paths that live **outside** `workspace_root`:

- Each entry must be an absolute path (`/etc/gitconfig`) or a home-relative path (`~/.gitconfig`).
  - `~/` is expanded to `$HOME` at sandbox build time.
  - Relative paths, `~user/` syntax, and bare `~` are rejected.
  - `..` traversal in any component is rejected.
  - Surrounding whitespace and wildcard syntax (`*`, `?`, `[...]`) are rejected.
- **`read_allow_paths` entries must exist at run birth.** An entry that does
  not resolve to a real path is rejected while projecting authority. On Linux,
  both read and write targets must exist when the `bwrap` plan is built; a
  missing target fails before dispatch. A write grant permits mutation within
  an existing target, not creation of the target mount itself.
- A `read_allow_paths` entry may be exactly an immutable declared string input
  reference such as `{{inputs.secondary_path}}`. Optional string inputs are
  allowed; an unset or empty value removes that entry instead of creating an
  empty filesystem grant. Interpolation, variables, and step outputs are
  rejected.
- A `write_allow_paths` entry grants read/list access as well as
  create/write/delete/rename access beneath that path, matching the writable
  mount exposed by both sandbox backends.
- **Inheritance**: merge/union — the effective list is the union of global + workflow + step entries, with duplicates removed (first-seen order preserved). This ensures more specific levels extend rather than replace broader grants.
- If neither `read_allow_paths` nor `write_allow_paths` is declared at any level, no external access is granted by default on Linux. On macOS, reads from the Transist home and shared invocation-staging parent are denied; the current plugin package, workspace, current staging root, and declared external paths are reopened. Unrelated same-user host files remain readable because macOS does not provide complete host read isolation.

#### Platform Differences

- **Linux (`bwrap`)**: The sandbox mounts fixed system roots (`/usr`, `/bin`,
  `/lib`, `/lib64`, and `/sbin` when present), its `/dev`, `/proc`, and `/tmp`
  runtime mounts, the plugin and invocation runtime paths, the workspace, and
  declared external paths. It does not mount arbitrary directories from the
  operator's `PATH`. Reads outside the mounted roots are denied; `--ro-bind`
  and `--bind` distinguish read-only and writable mounts. Every declared
  external read or write target must already exist, or sandbox construction
   fails before dispatch. A runtime-created temporary root is bound beneath the
   private `/tmp` mount only for the current invocation; general host `/tmp` is
   not granted.
- **macOS (`sandbox-exec`)**: The sandbox uses `allow default` for system calls (required for `fork` safety with `CoreServices.framework`), but denies reads from the Transist home, shared `.transist-staging` parent, and general system temporary parent. It reopens reads only for the current plugin package, workspace, current invocation staging root, exact runtime temporary root, and declared external paths. Write access outside the workspace is denied by a blanket `(deny file-write*)` rule and reopened only for declared paths, `/dev`, current invocation staging, and the exact runtime temporary root. External grants overlapping protected runtime roots are rejected. Process/MCP execution and preflight force `TMPDIR` to a declared `runtime://temp` or `runtime://temp/<relative-path>` directory when present, otherwise the private runtime temporary root, after manifest, managed, injected, and secret environment processing. Unrelated same-user host files remain readable because `sandbox-exec` is not complete host read isolation.

The old `mode` / `ignore_paths` shape is removed.

Update the global default policy in `transist/settings.yaml` directly by editing
`<effective-home>/settings.yaml`; the TUI displays it read-only.

### Workflow-Level Filesystem Policy

Workflows may declare a top-level `filesystem_policy`. The
`workspace_access` mode composes monotonically with global and step policy; the
external path lists are merged additively.

```yaml
filesystem_policy:
  workspace_access:
    mode: read_only
  read_allow_paths:
    - /etc/gitconfig
    - ~/.gitconfig
```

Step-level `workspace_access` may narrow inherited access but cannot widen it.
External path lists from the workflow and global policy are still merged in.

### Workflow-Level Network Policy

Workflows may also declare a top-level `network` block. `mode` uses whole-policy replacement semantics across global `settings.yaml` -> workflow -> step. `allow_hosts` and `allow_ports` are merged additively.

```yaml
network:
  mode: allowlist # any | loopback | allowlist | none
  allow_hosts:
    - api.example.com
    - auth.example.com
  allow_ports:
    - 443
  tls_required: true
  max_redirects: 5
```

- Missing `network` falls back to the workflow-level policy, then the global `transist/settings.yaml` `network_policy`, then builtin `mode: any`.
- For HTTP actions, the host adapter enforces `allow_hosts`, `allow_ports`,
  `tls_required`, and `max_redirects` before every request and redirect.
  Other adapters continue to receive the policy as adapter/plugin metadata.
- `tls_required` and `max_redirects` follow the most-specific policy source.
- Duplicate hosts and ports are removed while preserving first-seen order.

See [Network Policy](network-policy.md) for runtime forwarding, enforcement, and local-only behavior.

If an invoke step needs secrets, declare logical references under `with.secret_refs`. The runtime resolves and redacts raw values; the workflow should never contain the raw secret value. Secret references may be literals or immutable `{{inputs.name}}` placeholders. `{{vars.*}}`, `{{steps.*}}`, and indirect step-output values are rejected before the run becomes durable because they would change authority after run birth.

Every entry in `with.secret_refs` must be declared by the target plugin action's `secret_inputs`. If an action marks a secret input as `required: true`, the workflow must include that logical name in `with.secret_refs` or compile/preflight fails.

Keep `with.secret_refs` even when the action declares
`bindings.input_projection: action_data_v1`. That opt-in removes only the
top-level references from the adapter's cloned business input for schema
validation and transport; it does not change secret availability, required or
undeclared reference checks, capability authority, or delivery policy. All
other input keys still reach the action and must satisfy its genuine schema.
Actions without the binding retain legacy input bytes. See
[Plugin Manifest](plugin-manifest.md#opt-in-action-input-projection).

The authored `uses` adapter does **not** determine whether data stays local.
Locality is declared by the target plugin action's manifest `provider_kind`
(`local` or `external`). Under `--local-only`, Transist denies invoke steps
whose effective action `provider_kind` is `external` before dispatch. HTTP,
local process, and local MCP steps are additionally capped at loopback network
access.

### Capability Evaluation

Capability requests are generated by one authority projection from the pinned
recipe, effective filesystem and network policies, provider locality, logical
secret references, and manual gates. The projection also classifies each
authority dependency. Authors do not pass raw capability strings through
`with`. The complete normalized request set is persisted in the run recipe;
preflight, dispatch, retry, resume, cleanup, and child execution select exact
persisted subsets. In `policy.profile: allowlist`, use version-pinned plugin
grants for invoke authority and exact resource capabilities for the remaining
requests:

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

The runtime canonicalizes semver, paths, and URL origins before comparison.
Wildcards, userinfo, fragments, invalid IDNA, parent traversal, unresolved
paths, and unknown operations are rejected. Missing, malformed, removed, or
regenerated persisted authority fails closed with `capability_not_granted`
before secret lookup, preflight transport, or dispatch. Filesystem and network
policy still narrow the effective authority. A changed workspace root on
re-entry is authority drift; use replay for a new snapshot.

`workspace` requests authority; it does not grant it. The exact canonical
workspace resource and operations are generated at run birth. Operators approve
that request through policy, so a grant for the workflow source, the CLI
directory, or a guessed parent directory does not authorize a different
`repo_path` workspace. A `read_only` workspace requests only `list` and `read`.

For a persisted run, `transist runs capabilities <run-id> --format yaml`
prints a concise `policy.allow` fragment: version-pinned selected-action plugin
grants cover plugin load, action invocation, and the matching adapter/provider;
secrets, filesystem, network, and approval authority remain exact capabilities.
Use `--exact` for the normalized diagnostic list. This is read-only guidance;
the effective home `config.yaml`, not workflow inputs, remains the policy authority.
Named grant sets are likewise selected only by an exact operator-authored
workflow binding, never by workflow inputs.

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: call_api
    uses: process:shell/exec_with_secret_env
    with:
      command: ./scripts/call_api.sh
      secret_refs: ["MY_API_TOKEN"]
```

#### Version-Pinned Dependencies

Declare the version once in `plugins`, then reference that plugin with `uses`.
Home-aware validation requires that exact plugin version and action to be
installed. Compilation hashes the resolved plugin directory into the recipe as
`plugin_sha256`, so re-entry detects byte-level plugin drift.

#### Direct Step Output Bindings

Invoke `with`, inline `prompt`, and module `inputs` accept
`{{steps.<id>.out.<field>}}` directly. References stay within the current recipe;
use a module's declared export to cross a module boundary.

```yaml
steps:
  - id: collect_metadata
    uses: process:my-plugin/query
  - id: process_data
    uses: process:my-plugin/process
    with:
      count: "{{steps.collect_metadata.out.count}}"
      records: "{{steps.collect_metadata.out.records}}"
    prompt: "Process {{steps.collect_metadata.out.count}} records"
```

- Whole-value references preserve strings, numbers, Booleans, objects, arrays,
  and explicit JSON null, including inside nested arrays and objects.
- Interpolation in a larger string, and inline prompt text, accepts scalars only.
  Structured values require a whole-value `with` or module-input binding.
- Select object fields with dots and array members with decimal indexes, for
  example `steps.fetch.data.out.records.0.title`. Dotted step IDs are supported;
  selectors resolve against exact recipe-local step IDs. Ambiguous selectors
  matching overlapping step IDs are rejected rather than guessed.
- Unknown IDs, malformed selectors, undeclared module exports, and statically
  proven producer/consumer type incompatibilities fail definition validation.
  Local Draft 2020-12 references and unions are inferred conservatively; an
  uncertain type remains subject to complete dispatch-time schema validation.
- Numeric selectors account for every permitted container kind. For a schema
  allowing both arrays and objects, `value.0` may select array item zero or object
  property `"0"`; neither branch is silently preferred during type inference.
- Preparation records input-dependent work as `deferred_to_dispatch`. It still
  checks policy, exact authority, prerequisites, and secret availability/delivery.
  The consumer's full input schema and live readiness are checked before its
  first external effect, once producer values exist.
- Guards depending on deferred module inputs remain unknown during prospective
  checks. Both potentially reachable branches retain authority, secret, and static
  prerequisite checks. Known Boolean short-circuits still prune impossible paths.
  Composite schemas retain known failures even if another input is deferred.
  Known extra properties forbidden by `unevaluatedProperties` also fail before
  birth; only genuinely uncertain evaluation coverage waits for dispatch. Partial
  const/enum and uniqueness checks use recursive numeric equality (`1` equals
  `1.0`) while keeping distinct integer identities exact.
- Missing fields or unavailable producers fail before dispatch. Skipped, failed,
  in-flight, and prior loop-iteration results are never used as substitutes.
  An explicit null is a value; it is not a missing output.
- Output strings are data, never a second layer of template instructions.
- Step output references cannot select `uses`, permissions, network authority,
  workspace, secret references, `prompt_file`, or `source://` assets. A module
  input that feeds any nested authority field must be birth-known.

Use `set` for intentional mutable workflow state; copying a preceding output
solely to pass it to the next action is unnecessary.

#### Writing Workflow Variables (`set`)

After an invoke step succeeds you can write values into run-scoped variables
using the `set` block. Variables are available in subsequent steps via
`{{vars.<key>}}` in `with`, subflow `inputs`, `prompt`, and `prompt_file`
fields, and as `vars.<key>` in guard expressions.

```yaml
plugins:
  my-plugin: 0.1.0

steps:
  - id: collect_metadata
    uses: process:my-plugin/query
    with:
      target: "{{inputs.env}}"
    set:
    # Literal value.
      threshold: 100
    # Copy from workflow input.
      env_label: "{{inputs.env}}"
    # Capture from this step's own output.
      record_count: "{{steps.collect_metadata.out.count}}"

  - id: process_data
    uses: process:my-plugin/process
    with:
      count: "{{vars.record_count}}"   # resolved from the variable above
```

**Rules:**

- `set` is only allowed on invoke steps (not `control` or `subflow` steps).
- RHS values may reference `{{inputs.<key>}}` or `{{steps.<self-or-prior-id>.out.<key>}}`.
- `{{vars.*}}` is **not** permitted in `set` RHS values (to avoid cycles).
- If the step fails, skipped, or is cancelled, its `set` block is not applied.
- Later steps overwrite earlier writes to the same key (last write wins).

### 2. Control Step (The Manager)

Used to manage the flow of the workflow itself.

- **`control.noop`**: Does nothing. Often used as a placeholder or a target for `if` and `loop` blocks.
- **`control.finish`**: Stops the workflow immediately and marks it as a success.
- **`control.gate.manual`**: Pauses the workflow and waits for a human decision.

```yaml
- id: wait_for_review
  control: control.gate.manual
  with:
    gate_id: manager-approval
```

`gate_id` is optional; when omitted the step id is used. An authored `gate_id`
must be a non-empty ASCII string satisfying the identifier rule (see [Identifier
Rules](#identifier-rules)) and is used verbatim — never trimmed. A
whitespace-only, empty, or non-string `gate_id` value fails before run birth, so
a malformed gate id can never silently fall back to the step id or produce an
ambiguous decision path. Persisted decision records validate their schema, step
id, and decision value (`approved`, `rejected`, or `request_changes`) on load;
an unknown or tampered decision value fails closed instead of being treated as a
pass. Consumed decisions are removed through a descriptor-anchored delete and
directory sync so they can never reappear after a crash.

For a gate referenced by `gate_decision`, `gate_id` must be a literal. This
lets compilation freeze the identity that the runtime later validates against
append-only decision history.

### 3. Module Step (Package-Local Reuse)

Use a root `modules` declaration to run a package-local module as a nested child
run. Authored `subflow` steps are rejected.

```yaml
modules:
  cleanup:
    path: modules/cleanup.yaml

steps:
  - id: run_subtask
    module:
      ref: cleanup
      inputs:
        env: "staging"
      rerun_mode: reuse # or 'fresh'
```

Child inputs are bound against the child workflow's declared `inputs` at runtime.
Omitted optional child inputs receive their declared defaults before the child run
is created. Undeclared child inputs, missing required child inputs, and child-input
type mismatches fail the parent subflow step before the child run starts.

`rerun_mode: reuse` (default) re-enters the existing child run under the same run
id; the child's execution history is append-only, so the next dispatch of each
child step starts at `N+1` and prior execution records, artifacts, and evidence
remain inspectable. `rerun_mode: fresh` creates a distinct child run id with fresh
counters on each re-entry, so the child starts over from the beginning.
Changed child input values fail closed in reuse mode; use `fresh` when a new
iteration supplies new data. Binding failures are durable parent-step failures,
and invalid bindings never create a child run.
During preflight of an existing child, symbolic values are not compared with old
concrete inputs or used to prune guards. The complete current values are checked
against immutable child inputs at dispatch, so unchanged nested reuse succeeds
and changed values produce a durable failure.
A later resume that observes a known concrete child-input mismatch returns failed
preflight evidence and leaves the run durably failed/recoverable, with no additional
producer or consumer effects.

Modules lexically inherit the root workspace, network ceiling, inputs, context,
and artifacts. A module may narrow workspace or network authority, but never
widen it. A second repository, worktree, or parent-directory grant is not a
supported module workspace.

Modules use `transist.module.v1` and require release-version metadata. Maintained
catalog modules use `version: 0.1.0`, independently of the schema marker. They have
no independent workspace, network, or policy identity and cannot be compiled,
installed, or launched independently.

```yaml
# modules/review.yaml
schema: transist.module.v1
id: review
version: 0.1.0
steps:
  - id: finish
    control: control.finish
```

```yaml
modules:
  review:
    path: modules/review.yaml

steps:
  - id: review_release
    module:
      ref: review
      inputs:
        release: "{{inputs.release}}"
```

`module: review` is the equivalent shorthand when no inputs or rerun mode are
needed. Module calls create nested child recipes and child runs with input
binding, retry, replay, and `rerun_mode`. Module declarations are allowed in
both workflow and module documents. Module source and every module-owned
`prompt_file`, `source://`, and skill asset participate in the root workflow
definition digest.

Declare named exports in a module's `outputs` map:

```yaml
schema: transist.module.v1
id: query_module
version: 0.1.0
outputs:
  records: "{{steps.query.out.records}}"
steps:
  - id: query
    uses: process:my-plugin/query
```

A caller reads `{{steps.query_call.out.records}}` after `query_call` succeeds.
Nested modules may export another module call's named output. Exports resolve
against the child's current successful execution and are persisted in the
parent call's result. Root workflows cannot declare `outputs`. Export names
are single identifiers without dots; `subflow_id`, `subflow_version`,
`child_run_id`, `status`, `bound_inputs`, `child_steps`, `failure`, and
`__transist_*` names are reserved for runtime metadata.

---

## Logic and Flow Control

You can add logic to your workflow using `if` and `loop` blocks, but only on steps of type `control.noop`.

### Conditional Branching (`if`)

```yaml
- id: check_condition
  control: control.noop
  if:
    guard: "inputs.is_dry_run == true"
    then:
      - ref: finish # Jump to the end
    else:
      - ref: next_step
```

### Repeating Steps (`loop`)

```yaml
- id: retry_loop
  control: control.noop
  loop:
    body:
      - ref: attempt_connection
    until: "steps.attempt_connection.out.status == 'connected'"
    max_iters: 5
```

---

## Cleanup Phase

The top-level `cleanup` block defines a separate phase after the root workflow
finishes. Every explicitly declared cleanup runs automatically when its `when`
matches the terminal root outcome:

- **`when: [success]`**: Runs automatically after a successful root run.
- **`when: [canceled]`**: Runs automatically after a canceled root run.
- **`when: [failed]`**: Runs automatically after a failed root run.

No matching branch means no action. Failed or unexpected termination never
implicitly selects cancellation cleanup. Cleanup has a separate persisted result
and does not change the primary run status. Repeated `runs cancel` during
cancellation cleanup does not interrupt it or duplicate dispatch.

Abrupt process death can prevent cleanup from starting or finishing. Use
`transist runs cleanup <run-id>` to recover unfinished or failed cleanup from
durable history; it is not a required normal-path trigger for failed cleanup.
A failed run without matching cleanup remains recoverable. Once failure cleanup
begins, existing retry/resume protection applies; use replay for new primary work.

---

## Important Constraints

- **Unique IDs:** Every step ID must be unique within the entire workflow (including cleanup steps).
- **Cross-step data:** Bind workflow inputs with `{{inputs.name}}` and pass JSON values directly with `{{steps.<id>.out.<field>}}`; cross module boundaries through named exports. Keep `set`/`vars` for intentional mutable state. Use bounded root managed documents through supported process action path fields for intermediate files/trees, and workspace paths for intended source/OpenSpec deliverables. `run://` is invocation-private scratch, removed after the invocation. See [Path and Resource Semantics](path-and-resource-semantics.md).
- **Secret references:** Keep logical secret names under invoke-step `with.secret_refs`; never write raw secrets into workflow files, artifacts, evidence, events, or run state.
- Runtime-known raw secret values and their padded or unpadded standard and
  URL-safe base64 forms are redacted before durable records or promoted
  artifact/evidence files are written. UTF-8 files are redacted; binary files
  containing a known variant are rejected. This does not claim DLP coverage for
  arbitrary plugin transformations or external exfiltration.
- **No Nested Logic:** `subflow` and `cleanup` steps cannot contain their own `if` or `loop` blocks.
