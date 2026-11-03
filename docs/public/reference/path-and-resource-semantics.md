# Path and Resource Semantics

Transist uses similar-looking strings at different layers. A capability
resource is not necessarily a workflow action path, and an action convention
is not runtime authority.

## Quick Selection

- Pass scalar or structured data directly with `{{steps.<id>.out.<field>}}` in
  action data, inline prompts, or module inputs. Use `set` and `{{vars.name}}`
  for intentional mutable workflow state.
- Pass runtime-managed intermediate files/trees to process actions using
  `{$document: name, access: read|write|update}`. Use workspace-relative paths
  for repository deliverables when the action accepts them.
- Use `run://...` only when the selected action documents invocation-local
  scratch support.
- Use `repo://...` only when the selected action documents it.
- Use `source://...` only for contained workflow helper assets.

Never use documents, workspace files, external files, `run://`, or
`runtime://temp` to persist secrets between steps.

## Syntax by Layer

| Syntax | Layer and resolver | Meaning | Persistence | Authority |
| --- | --- | --- | --- | --- |
| `workspace:///absolute/root` | Runtime capability resource | Canonical workspace authority requested by a workflow. | Persisted with the run's authority record. | Yes. |
| `relative/path` | Selected action; process actions run in the workspace CWD. | File below the declared workspace. | Mutable for the workspace lifetime. | Subject to workspace policy. |
| `{$document: name, access: read\|write\|update}` | Runtime process action-data binding | Invocation-private copy or absent output location, outside durable home/workspace. | Exact immutable revisions published with successful execution records. | No filesystem grant or alternate workspace. |
| `run://artifacts` / `run://evidence` | Runtime capability resources | Permission to create and write invocation artifact or evidence outputs. | Capability record only; accepted outputs are promoted into execution-scoped storage. | Yes. |
| `run://path` | Optional action convention | Invocation-private scratch when supported by an action such as `shell/exec`. | Removed after the invocation. | No. |
| `repo://path` | Optional action convention | Repository-local operational file when supported by an action. | Action-defined. The bundled shell plugin maps it to `<repo_path>/.transist/path`. | No. |
| `source://path` | Shell helper-asset convention with runtime capture | Regular, contained helper asset relative to its declaring workflow or module. | Captured under `workflow_assets/` for snapshot-backed runs. | No. |
| `runtime://temp[/path]` | Process/MCP plugin manifest sentinel | Private provider temporary directory requested in a complete `entrypoint.env` value. | Invocation-private. | No. |
| `file:///absolute/path` | Runtime capability resource | Canonical external filesystem capability derived from policy path grants. | Persisted with the authority record. | Yes. |

`artifact://` and `evidence://` are not Transist schemes.

Direct output bindings carry JSON values, not a new path scheme or grant.
Whole-value references preserve types; interpolated references accept scalars.
Passing a produced path to an action remains subject to that action's path
conventions and the workflow's already-declared authority. Outputs cannot
choose workspace/permission/secret authority, a `prompt_file`, or a `source://`
asset. Module exports expose only declared recipe-local values. See
[Direct Step Output Bindings](workflow-schema.md#direct-step-output-bindings).

## Managed Document Paths

Root declarations define bounded `file`/`tree` storage and are inherited by modules.
Only process invoke action data may bind them. Read copies are sandboxed read-only;
update copies are writable; write paths are initially absent. The selected action
must support ordinary absolute paths outside the workspace. Document copies cannot
select `repo_path`, a workspace/CWD, command, source asset or prompt file. HTTP/MCP
bindings are unsupported. `reviewed_by` on a read binds a recipe-local manual gate
approval to the exact selected revision ID and digest before copying. This adds no capability-resource or
plugin URI scheme. `$document:<name>` in durable nested path text is an inspection
label, not a path plugins can open. See
[Process Document Bindings](workflow-schema.md#process-document-bindings).

The runtime validates the full staged request before probes/transport and all
declared outputs before batch blob persistence. It persists logical bindings,
normalizes reported path metadata and promoted text, and never exposes its durable
home. Business document bytes are exact and secret-bearing publication is rejected.
Resume retains committed revisions; replay creates an independent namespace.

## Invocation Runtime Metadata

`TRANSIST_RUNTIME_METADATA_FILE` names a private runtime-created JSON file, not
durable storage or a filesystem grant. Its additive `run.id` is the exact current
invocation's run ID, independent of staging/workspace/durable directory names.
It carries no root ID, home/run path, secret, or authority. Child invocations see
their own ID. Read a root ID once during root-ledger initialization and share the
managed document with modules; do not reinitialize it from child metadata. Never
derive durable paths or access from an ID. The file/path expires with staging;
see [APP runtime metadata](app.md#invocation-runtime-metadata) for the exact JSON.

## Repository-Local Operational Files

Maintained delivery workflows use bounded root managed documents for issue
context, attachments, prompts, drafts, reports and ledgers. These operational
intermediates live outside Git; they need no repository exclusion rules. Keep
intended source changes and OpenSpec change bundles in the workspace.

Other workflows may deliberately use action-supported workspace files, including
plugin-specific `repo://` paths. That explicit choice is not automatic managed
persistence: workspace files are mutable and are not snapshotted, redacted,
rolled back or isolated by a new run/replay ID. Preserve active-run dependencies
and coordinate shared-path mutations. Changing `TRANSIST_HOME` does not relocate
these files. Do not recommend `.transist/` files as a substitute for managed
intermediate handoffs.

`run://` and `runtime://temp` are invocation-private, not cross-step channels.
Direct plugin access to the durable runtime home is not a supported shared-file
channel. Inspect actual action path contracts before binding documents.

## Capability Resources

The runtime evaluates filesystem authority with canonical capability resources:

- `workspace:///absolute/canonical/workspace`
- `run://artifacts`
- `run://evidence`
- `file:///absolute/canonical/external/path`

These strings describe authority. They are not generic arguments that plugins
must resolve. Write ordinary host paths, not `file://` values, in policy
`read_allow_paths` and `write_allow_paths`; the runtime canonicalizes them to
capability resources.

## Action Path Conventions

Plugins own the path syntax accepted by their actions. Inspect the action schema
and plugin documentation before passing URI-shaped values.

The bundled shell plugin runs commands with the canonical workspace as CWD. Use
ordinary relative paths for workspace files. Its `repo://` support requires an
action `repo_path` value and resolves `repo://path` to
`<repo_path>/.transist/path`. Its `run://` support resolves within the current
invocation staging root, so it cannot transfer data to another step.

Other plugins may support different subsets of these conventions. A supported
path convention never creates authority beyond the workflow's declared and
granted workspace or external filesystem capabilities.

## Workflow Assets and Temporary Directories

`source://` references are package assets, not mutable workflow data. They must
be literal contained regular files. Snapshot-backed runs capture eligible shell
helpers under private `workflow_assets/` storage and use those bytes for
dispatch and re-entry; legacy records retain their authored-source behavior.

`runtime://temp` is valid only as a complete process or MCP `entrypoint.env`
value in a plugin manifest. It is not a workflow action path, host-path grant,
or durable storage channel.

The canonical OS temporary parent (`/tmp` on Linux, `/private/tmp` on macOS) is
independent of the caller's `TMPDIR`. Preparation and staging reject a parent
covered by a workspace, home or external grant. Home and workspace are literal
filesystem paths: valid native brackets/question marks are not grant patterns.
External allow-path declarations retain their grant grammar. Private roots cannot
overlap exposed package, staging, workspace or grant roots, including canonical
aliases. Workflow source-directory
exceptions cannot reopen protected temporary/staging/home trees; captured helper
assets remain available in the current invocation's staging root.

`runtime://temp/documents` and every descendant are reserved, including case
aliases. Manifest validation/install reject them and traversal/encoding variants;
dispatch rejects symlink traversal and checks all declarations before creating
any environment directory. Provider-owned temporary subdirectories remain usable.

The runtime creates the root and declared child directories with private
permissions before launch. A declared `TMPDIR: runtime://temp[/path]` selects
that directory; otherwise it forces `TMPDIR` to the private root after all
manifest, managed, injected, and secret environment processing. Literal
manifest `TMPDIR` values are not honored. The root lasts only through output
validation and promotion, then the runtime removes it after terminal execution
or preflight cleanup and reaps stale owner-marked roots after crashes. It is
therefore unsuitable for provider login, session, cache, or other durable
state.

## Artifacts, Evidence, and Secrets

Artifacts and evidence are invocation outputs reported by adapters, validated,
and promoted into durable execution-scoped inspection storage. They are not a
generic mutable filesystem shared by later steps.

Workspace and externally granted files are mutable host state. They are not
snapshotted, redacted, or rolled back by the runtime. Do not place secrets in
them, workflow assets, artifacts, evidence, or runtime state.

## Related References

- [Workflow Schema](workflow-schema.md)
- [Plugin Manifest](plugin-manifest.md)
- [TEP](tep.md)
- [Storage Layout](storage-layout.md)
- [Workflow Variables](workflow-schema.md#writing-workflow-variables-set)
