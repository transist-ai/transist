---
name: transist-workflow-authoring
description: Turn a human goal into a validated, deterministically explained Transist workflow; revise, package, or recover it only within the requested execution boundary.
metadata:
  owner: transist
  domain: infra
  triggers: transist workflow, transist module, conversational authoring, workflow revision
  priority: high
  token_budget: medium
---

# Transist Workflow Authoring

## When to Use This Skill

Create or repair `transist.flow.v1` workflows and package-local
`transist.module.v1` modules. Load this checked-out skill **and its references**;
an assistant's cached/global copy may describe an older contract.

## Default Workflow

Follow these seven stages in order. An authoring request is not execution consent.

1. **Capture intent.** Record the goal, inputs, outputs, systems, effects,
   approvals, and repetition. For revisions, record what must change and what
   must remain: conditions, retry/loop limits, cleanup, authority, pins and assets.
2. **Clarify only gaps.** Ask only for missing behavioral decisions or required
   bindings: destination, model, repository, workspace, review/rejection behavior,
   repeat/stop bounds. Reuse supplied answers. Do not ask users to choose temporary
   paths, ledger IDs, document filenames or other runtime-owned path plumbing.
   Synthetic briefs may supply explicit fixture decisions; label them synthetic,
   never invent a human reply or claim a new human permission.
3. **Inspect real contracts.** Start with bundled `plugins/`, `workflows/` and
   `examples/`; this checkout must not require sibling catalog repositories. Read
   actual installed or explicitly supplied `plugin.yaml`, exact pins, selected
   input/output schemas, bindings and limits. Record which was inspected and
   unresolved local pins. Verify file-path support outside the workspace and
   secret delivery.
   **Report missing integrations** as prerequisites; never invent an action or
   conceal an unsupported integration inside unrestricted shell/agent execution.
4. **Generate the package.** Write a `0.1.0` flow, contained prompts (inline when
   the action has no `prompt_file` binding), and portable fictional sample inputs.
   Bind real paths at validation/run time, not to the author's machine. Use one
   canonical root workspace and exact authority. Apply the data/document/review
   contracts below. Preserve the revision's conditions, limits and cleanup.
5. **Validate read-only source.** Run both forms below against an explicit home.
   Inspect every finding; missing bindings remain prerequisites. Source checks
   dispatch no adapter, create no run, install nothing and grant no authority.

   ```bash
   transist workflows check --path workflow.yaml --home "$AUTHORING_HOME"
   transist workflows check --path workflow.yaml --inputs-file inputs.json --home "$AUTHORING_HOME"
   ```

6. **Explain deterministically.** Use the actual CLI, not assistant-written prose
   as a substitute. Read the resolved pin/contracts or explicitly unresolved
   entries, authority, data dependencies, documents, gates, branches, limits,
   cleanup and opaque prompts/scripts. For revisions use `--compare-to`; account
   for every difference, including detached asset digests and pins.

   ```bash
   transist workflows explain --path workflow.yaml --inputs-file inputs.json --home "$AUTHORING_HOME" --format json
   transist workflows explain --path workflow.yaml --inputs-file inputs.json --compare-to previous/workflow.yaml --home "$AUTHORING_HOME" --format json
   ```

7. **Present prerequisites and execution boundary separately.** Deliver authored
   paths, actual validation/explanation receipts, intended changes and unresolved
   inputs/plugins/integrations/secrets/policy/readiness. Ask separately for execution
   approval with exact systems, data, effects and scope. A request to author a
   publication workflow is not approval to publish, send network data, resolve
   operational secrets, install, authorize, sign or dispatch. Perform only explicitly
   requested lifecycle operations. Existing authorization for deterministic fake
   tests applies only to those tests, not real services or future runs.

## Core Content

### Data and authority

- Invoke with `uses: <adapter>:<plugin>/<action>` and exact root `plugins` pins.
  Never author separate `adapter`, `plugin`, `action`, `plugin_version`, or
  `execution_kind` fields on invoke steps.
- Use recipe-local `{{steps.<id>.out.<field>}}` directly in invoke `with`, inline
  prompts and module inputs. Whole-value references preserve JSON types, including
  Booleans, objects, arrays and null; interpolation accepts scalars only.
  Unknown/malformed selectors fail. Missing skipped/failed/previous-iteration
  outputs fail before consumer dispatch; no old-value fallback.
- Modules declare named `outputs`; callers read
  `{{steps.<module-call>.out.<export>}}`, never a child's internal step ID.
  Root inputs/context/documents are lexically inherited. Profiles are action-bound
  reusable settings; modules may narrow, never widen authority.
- `set`/`vars` remain for authored mutable state such as counters, not redundant
  copies of producer outputs. `deferred_to_dispatch` means declared data-dependent
  checking will run before consumer dispatch, not failure or a completed check.
  The complete materialized request must pass the real action schema before probes.
- Workspace, network, filesystem grants, destinations declared by policy, plugin
  identity, executable selection and secret references must be birth-known. Never
  derive authority from model output, documents, variables or prior-step results.

### Managed intermediate documents

- Declare root `documents` with `kind: file|tree` and positive `max_bytes`;
  trees also require positive `max_files`. Files may have literal UTF-8 `initial`.
- Use `{$document: name, access: read|write|update}` recursively **inside process
  invoke action data only**, after inspecting the action's string-path contract.
  `read` copies the latest committed revision privately; `write` starts a new
  output; `update` copies the latest revision into a writable output. Reading or
  updating unpublished content without `initial` fails.
- Do not put bindings in HTTP/MCP action data, authority, executable identity,
  `source://`, or `prompt_file`. HTTP/MCP can still use ordinary JSON output refs.
  Reject symlinks, traversal, special files and exceeded tree/file bounds.
- Successful execution records publish all outputs atomically as immutable
  revisions (run, step, execution, digest, size); failure publishes none. Known
  secret values/recognized encodings are rejected, not silently redacted in bytes.
  Logical references persist, never invocation paths. Resume retains committed
  revisions; replay starts a fresh namespace. These are not secret stores.
- Keep intended source/OpenSpec deliverables in the workspace. Intermediate
  prompts, notes, drafts and ledgers belong in documents, not shared workspace
  files or invocation-private `run://` scratch. Same-workspace mutations still
  need coordination; document isolation does not roll back external effects.

### Exact review

Author manual `with.review_documents: [draft]`, then a protected read such as
`{$document: draft, access: read, reviewed_by: review_draft}`. `reviewed_by` names
the **gate step ID in this lexical recipe**, not `gate_id` or an ancestor's gate.
Review exact IDs, digests, sizes and inspection content links. Approved decisions
must cover the consumed revision; any new revision, even identical bytes, needs
new approval. Waiting restart retains review content. Two approvals require two
distinct authored gates; a repeated label or description is not a second gate.
Document review never approves arbitrary workspace mutations.

### Terminal cleanup and recovery

Every explicitly declared cleanup runs automatically when its `when` matches the
terminal root outcome (`success`, `canceled`, or `failed`). No matching branch
means no action; failed or unexpected termination never implicitly selects
cancellation cleanup. Explain cleanup effects before execution approval. Cleanup
has its own result and never changes the primary status.

`runs cleanup` recovers unfinished or failed cleanup, including after abrupt
process death; it is not a mandatory normal-path failed-branch trigger. Committed
successful cleanup steps are not repeated. Repeated `runs cancel` during
cancellation cleanup does not interrupt or duplicate it. A failed run without
matching cleanup remains recoverable; once failure cleanup begins, primary
retry/resume is blocked. Use replay for new primary work with the same pinned
snapshot, or a fresh workflow run for changed definitions/inputs. Durable history
supports recovery, not guaranteed cleanup completion.

### Repository-scoped version policy

Maintained flows, modules, plugin releases and exact pins in these catalogs stay
`0.1.0`. Schema IDs stay `transist.flow.v1`, `transist.module.v1`,
`transist.plugin.v1`; runtime/admin crates use `0.1.1`. This is not a global release
policy. Signatures bind bytes: changed same-version content needs a new reviewed
bundle/signature and a **fresh home**, never overwrite installed payloads, remove
pinned dependencies or delete old runs. Preserve the preceding home/bundle for
rollback. Signing/private-key use needs its own explicit authorization.
The current authoring/persistence contract requires fresh-home adoption;
there is no legacy reader, version alias or in-place migration. See
`docs/public/reference/compatibility-and-stability.md` for adoption and rollback.

## Hard Rules

- `workspace.path` is authority, not source-directory or CLI CWD selection.
  Grant only its canonical exact path; prefer `read_only` when no mutation is needed.
- `source://` and literal `prompt_file` assets are contained regular files relative
  to their declaring flow/module; no absolute paths, `..`, symlinks or secrets.
- `repo://` is action-specific, not managed storage. `runtime://temp` is a plugin
  manifest declaration, not a workflow path. Never use temporary paths for handoffs.
- No implicit installation, authorization, private-key use or dispatch. Governed
  delivery verifies signed packages; source validation is not trust verification.
  `--trusted-execution` does not authorize signing or supply missing grants.
- Never auto-approve real gates or edit persisted review/state/history. A run pins
  recipe/assets/inputs/workspace; source repairs need a fresh run, not retry/replay
  to retrofit changed code. Cleanup retains declared conditions; never delete
  active-run state as an authoring repair.

## Source Priority

1. Actual checked-out CLI/compiler and installed exact action schemas/bindings;
   persisted recipe/history for an existing run.
2. `docs/public/reference/workflow-schema.md`, `cli.md`, and
   `path-and-resource-semantics.md`.
3. Validated catalog sources at recorded revisions. A catalog action absent from
   the target home remains an installation prerequisite, not a resolved local pin.

## Examples

See the practical sequence, binding examples, anti-patterns and source-of-truth
table in [Conversational authoring](references/conversational-authoring.md).
The three executable synthetic fixtures live in the workflow catalog under
`tests/fixtures/conversational_authoring`; they are tests, not launch defaults.

## References

- [Conversational authoring](references/conversational-authoring.md)
- [Assets, delivery and recovery](references/workflow-asset-authoring.md)
- Public guide: `docs/public/guides/authoring/ai-assisted-authoring-with-repo-skills.md`
- Public lifecycle and inspection: `docs/public/reference/cli.md`
