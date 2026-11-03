# CLI Command Reference

This is the complete technical reference for the `transist` and `transist-server` command-line tools.

Signing, key generation, public-key export, signature inspection, and verify
dry-runs are intentionally not part of the `transist` runtime CLI. Those admin
commands live in the `transist-admin` CLI (shipped with every release archive
and available in the repository as `transist-admin/`) so runtime machines do
not create trust.

The runtime-side trust surface is verification-only: import trusted public keys,
list them, and later use them when trusted-execution enforcement is enabled.

Canonical admin commands:

```bash
transist-admin key generate --name acme --private-key-file ./trust/acme.key
transist-admin key export-public --private-key-file ./trust/acme.key --trust-root-file ./trust/acme.pub
transist-admin workflow sign ./examples/quickstart/quickstart.workflow.yaml --private-key-file ./trust/acme.key
transist-admin plugin sign ./plugins/shell --private-key-file ./trust/acme.key
transist-admin workflow verify ./examples/quickstart/quickstart.workflow.yaml --signature ./examples/quickstart/quickstart.workflow.yaml.sig --trust-root-file ./trust/acme.pub
transist-admin plugin verify ./plugins/shell --signature ./plugins/shell.sig --trust-root-file ./trust/acme.pub
transist-admin signature inspect ./examples/quickstart/quickstart.workflow.yaml.sig
```

See `transist-admin/README.md` for the full admin command reference and JSON
contracts.

## Global Options

These flags can be used with any `transist` command:

- **`--home <path>`**: Manually specify the Transist home directory for this command.
- **`--ui <plain|tui>`**: Control how progress is displayed.
  - `plain` (Default): Disables the rich UI; good for CI/CD and automation.
  - `tui`: Enables the rich fullscreen UI on supported terminals.
- **`--format <json|plain|yaml>`**: Control output format.
  - `plain` (Default): Human-readable output.
  - `json`: Structured JSON output for programmatic consumption.
  - `yaml`: Supported by `runs capabilities` only.

## Environment Variables

- **`TRANSIST_HOME`**: An alternative way to set the home directory path.
- **`TRANSIST_ACTOR`**: Fallback operator identity for manual gate decisions.
- **`TRANSIST_REASON`**: Fallback decision reason for manual gate decisions.
- **`TRANSIST_PROGRESS_RENDER=plain`**: Disables the rich UI (same as `--ui plain`).
- **`TRANSIST_UI`**: Alternative way to set UI mode (`plain` or `tui`).
- **`TRANSIST_PREFLIGHT_PROBE_CONCURRENCY`**: Maximum sandbox-isolated process
  or MCP probes within one adapter preflight call. The default is `4`; valid
  values are integers from `1` through `32`. Invalid values fail built-in
  adapter construction before a probe starts. HTTP probes and local probes
  whose constructed sandbox plan does not prove isolation remain sequential.

---

## Workflow Management

The governed lifecycle is: check authored source; package and sign with
`transist-admin`; install the signed package; check the installed workflow with
the prospective inputs; authorize only exact missing authority; check again;
then prepare or run. Installation persists immutable workflow code. Readiness
and authorization are prospective diagnostics; once preparation creates a run,
use `runs show`, `runs preflight`, `runs capabilities`, and `runs tail` to
inspect persisted diagnostics.

### `workflows explain --path <workflow.yaml> [--inputs-file <file>] [--compare-to <previous-workflow.yaml>] --format text|json`

Explains captured authored definitions using compiler lowering and exact installed
plugin contracts. Defaults to text; `--format json` emits the typed
`transist.workflow_explanation.v1` view, described by
[`workflow-explanation.v1.schema.json`](../../../schemas/runtime/workflow-explanation.v1.schema.json).
`plain` remains accepted for text output. Inputs files accept JSON or YAML objects.

```bash
transist workflows explain --path workflow.yaml --format text
transist workflows explain --path workflow.yaml --inputs-file inputs.json \
  --compare-to previous/workflow.yaml --format json
```

- Shows full action identities, actual package/schema digests, contract schema
  filenames/bindings, profile/context-expanded inputs, variable writes and direct
  output dependencies, named module exports, document read/write/update access,
  review gate step names, branches, bounded loops, cleanup and requested authority.
- Missing required inputs retain their names/types and `state: missing`, without
  fabricated values. Deferred module values are `symbolic`. Supplied values and
  defaults materialize only birth-known bindings; action results remain unknown.
  Omitted optional inputs use the runtime's actual null value (`unset_null`).
  Binding follows execution semantics: `{{inputs.payload}}` preserves a whole
  object, while `{{ inputs.payload }}` is interpolation and rejects structured
  data. Input/variable numbers and booleans inside arrays become strings (also
  through nested objects); direct step outputs retain their types. Numeric and
  Boolean inline prompts convert to text before hashing. Structured/null/empty
  prompts and known invalid interpolation return errors without a request-validity
  claim; missing required values remain explicitly symbolic.
- Workspace, filesystem, network and logical secret references show declared and
  input-bound requests. Paths are **declared, not canonicalized**; effective home
  policy, trust, secret availability/delivery and actual grants are not evaluated.
  Exact canonical authority is still resolved/enforced during preparation. Do not
  supply secret values in authored definitions, assets, or the ordinary inputs file.
- Reads the selected home's local plugin catalog without requiring an initialized
  installation or valid runtime configuration. A read-only or missing home works
  without creation. Missing pins are `unresolved_missing`; unreadable/invalid
  packages are `unresolved_invalid`. Missing actions/adapter mismatches are explicit.
  Contract resolution is not signature verification. No actions are invented.
- Performs no installation, authorization, prerequisite execution, adapter probe,
  action dispatch, remote schema/API download, secret lookup, or home write.
- External actions, arbitrary scripts and agent prompts are **opaque behavior**.
  Prompt/helper digests detect byte changes; prose is not interpreted as enforced
  runtime semantics. Document revisions and gate approvals are not created.

JSON has sorted object keys, sorted dependency lists, explicit authored step/cleanup
order, stable lexical module-call paths and package-relative asset labels. There
are no timestamps, random IDs or generated absolute source/home prefixes. Explicit
authored paths and supplied workspace values remain exact. Identical captured
definitions, inputs and contract snapshots produce identical JSON bytes in this
version. `comparison.changes` is sorted by escaped JSON pointer; before/after retain
JSON types, and arrays are compared as ordered content. Comparisons include changed
actions, bindings, gate requirements/approval counts, limits, authority, pins and
inline/detached prompt/helper digests. An empty diff is not semantic equivalence.

**An explanation is not validation, authorization, readiness, or execution consent.**
Run `workflows check` and the governed lifecycle separately before execution.

### `workflows check (<id>@<version> | --path <path>) [--live] [--input <key=value>]... [--inputs-file <path>] [--trusted-execution] [--local-only] [--output <readiness.json>]`

Checks either authored workflow validity or installed workflow validation without
creating a run. Supply exactly one target: `--path` checks a workflow file for
syntax and logic errors, while the positional workflow reference binds the installed
workflow and validates current inputs and authority. `--output` and `--live`
require an installed target and select live readiness. Ordinary installed checks
perform no plugin startup, APP handshake, MCP initialization, or HTTP health request.
Missing, malformed, extra, or conflicting targets and `--path --live` are usage
errors rejected before policy writes or run creation; versions are never inferred.

For `--path`, Transist evaluates all independent static diagnostics,
including every missing plugin or action it can inspect. Without `--input`
or `--inputs-file`, checking stays in authored mode: it does not bind required
runtime inputs and emits one nonblocking `not_evaluated` check for each.
`--trusted-execution` and `--local-only` require bound inputs and are rejected
when neither input source is provided.

For an installed workflow reference, Transist checks the installed workflow with bound
representative inputs without resolving secrets or dispatching workflow actions.
With `--live` or `--output`, plugin handshake/health checks may start processes or contact their
declared network services. It evaluates definition, assets, dependencies, trust,
workspace, schemas, requirements, policy, secret availability and delivery,
prerequisites, and (in live mode) plugin health. The JSON report uses
`schema: "transist.workflow_readiness.v1"` and always includes
`run_created: false`, `evaluation_mode`, and `validation_status`. In validation
mode, `validation_status: passed` exits zero while `status` remains `not_ready`;
plain output states **Validation passed; live readiness not checked.** Warnings
and genuinely incomplete required validation block success. Full reports and
typed summaries agree on findings, scope, and predicates. In live mode,
`evaluation_mode: live_readiness`, status is `ready` when every check passes,
`not_ready` when static evidence is incomplete, `not_deliverable` when existing
checks fail, or `invalid_definition` when the installed workflow cannot be
loaded or bound. Readiness is evidence, not execution authority.

Declared output dependencies and managed document paths appear as
`deferred_to_dispatch` findings in CLI,
TUI, full reports, and summaries. They permit preparation when all static and
authority requirements pass; only data-dependent validation and probes wait
until the affected invocation. Genuine `not_evaluated` requirements remain
blocking. The runtime checks complete materialized input before even a consumer
readiness probe. Full JSON reports use `checks[]`; typed summaries use `findings[]`
with `status: deferred_to_dispatch` for this work, not a separate
`deferred_findings` field. Report schemas are published in
[`workflow-reports.v1.schema.json`](../../../schemas/runtime/workflow-reports.v1.schema.json).

For process document bindings, run/step request details retain the authored logical
objects. Result details replace exact document paths with those objects and embedded
path text with `$document:<name>` labels. These labels are inspection metadata, not
openable files or capability resources. Successful immutable execution records carry
`document_revisions` (identity, digest, bytes and producing execution); latest heads
are rebuildable storage projections. Gate review surfaces show exact revision IDs,
digests, byte/file counts and opaque companion metadata/content links. There is no
document-path CLI grant; use [managed-document inspection](companion-server-api.md#managed-documents)
for exact bytes. See [managed documents](workflow-schema.md#process-document-bindings).

All `check` modes and `authorize` default to concise text, including piped or
redirected stdout. `--format plain` selects the same summary-only evaluation;
`--format json` explicitly requests complete ordered diagnostics. Scripts that
parse JSON must add `--format json`; piping alone no longer selects JSON.
Summary evaluation counts passing checks without retaining their records and
groups identical actionable findings by status, code, message, and remediation,
with occurrence counts and at most three unique example step paths.
It separates input/action-schema blockers from missing
authority. `authorize` uses the same blocker view if it cannot propose grants;
it shows the exact policy and destination before an interactive grant prompt
only when no other validation findings remain. If it finds missing grants plus
other blockers, correct those blockers before retrying `workflows authorize`
with the same inputs and any original `--input` overrides.
Summary output retains required-input name hints without their values.
Full JSON names required string inputs supplied empty after inline overrides
and required inputs not supplied without defaults, separately at
`diagnostics.inputs.empty_required` and `diagnostics.inputs.missing_required`.
These names are possible causes, not inferred mappings from action-schema
fields: inspect the workflow binding to confirm a relationship. The JSON
report lists these names at `diagnostics.inputs.empty_required` (and, for
`authorize`, under `readiness.diagnostics.inputs.empty_required` as well).
The command writes exact grants under the effective home's policy after review,
not into the workflow definition. `--dry-run` never writes policy.

With `--format plain`, readiness from a blocked `prepare` or `run`
groups checks by concern. When a step depends on an earlier runtime-produced
value, the output identifies deferred work separately from blockers.
Equivalent-authority invoke steps may share one live plugin probe. The report
in full JSON still emits one ordered check per associated `step_path`, including complete
parent prefixes for nested subflows; status, message, and remediation are
identical for every expansion of the shared result.

JSON keeps the full `checks[]` array and `validation.missing_capabilities[]`.
Installed-workflow `check` and `authorize` additionally expose a derived
`diagnostics` index: `groups.<category>.{failed,warning,not_evaluated,deferred_to_dispatch,check_indices}`
and `authorization.{eligible,missing_capabilities,blocking_codes}`. Categories
are `inputs`, `workspace`, `trust`, `dependencies`, `secrets`, `external`,
`authority`, and `other`; unknown codes remain in `other`. Indices are
zero-based positions in the unchanged `checks[]` array. `eligible` means the
nonpassing checks are *only* missing authority, not that grants were applied.
When an existing policy destination blocks an otherwise valid plan,
`authorize` also reports `diagnostics.authorization.plan_blocker` as
`policy_destination_unavailable`. Diagnostics repeat neither input values nor
secret values; individual check messages still carry their usual diagnostic
paths. For example, filter actionable checks without paging through passes:

```bash
transist workflows check my_flow@0.1.0 --inputs-file inputs.json --format json \
  | jq '{status, groups: .diagnostics.groups, authorization: .diagnostics.authorization}'
transist workflows check my_flow@0.1.0 --inputs-file inputs.json --format json \
  | jq '.checks as $checks | .diagnostics.groups.inputs.check_indices[]? | $checks[.]'
transist workflows authorize my_flow@0.1.0 --inputs-file inputs.json --dry-run --format json \
  | jq '{status, blockers: .diagnostics.authorization.blocking_codes, missing: .diagnostics.authorization.missing_capabilities}'
```

A nonzero exit status from `check` or blocked `authorize` is expected in these
inspection pipelines; inspect the result rather than treating `jq` success as
readiness or authorization. Supply actual inputs before authorizing; a grant
suggestion cannot repair invalid action inputs.

Use `--output <readiness.json>` with an installed workflow reference to write canonical JSON
readiness evidence for an explicit later `prepare` or `run`. The evidence
records no input values or resolved secrets: supplied inputs are represented
only by a SHA-256 fingerprint. It binds the installed workflow, canonical
workspace, plugin dependencies, trust state, policy, settings, and
static-preflight result.

`--output` selects full live evaluation and collects the display summary incrementally
in that same evaluation. Stdout stays concise unless `--format json` is also
supplied; the file is always complete canonical evidence. Summary-only checks
do not generate evidence fingerprints. Concise reports end with a reminder to
rerun with `--format json` for the full report.

```bash
transist workflows check --path my_workflow.yaml
transist workflows check my_flow@0.1.0 --inputs-file inputs.json --output readiness.json
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

### `workflows install (--package <directory> | --path <workflow.yaml>) [--dry-run | --approve]`

Reviews and installs immutable workflow code into the active home. The report
uses `schema: "transist.workflow_install.v1"`, always sets
`run_created: false`, and has one status: `blocked`, `planned`, `declined`,
`applied`, or `already_installed`.

In a governed home, `install` requires `--package` and verifies the package's
workflow, assets, digest, detached signature, and imported trust root. It does
not bind inputs, check readiness, create policy, or create a run.

In standalone and development homes, `--path` installs an unsigned authored
source for local authoring only. Trusted installation always uses `--package`.

```bash
transist workflows install --path my_workflow.yaml
transist workflows install --package package-directory
```

`--dry-run` never mutates the home. Human-readable interactive use
prints the complete plan and asks for confirmation with a default of No.
Structured output never prompts. `--approve` is reserved for noninteractive
automation after an operator has reviewed an equivalent plan. Blocked plans,
declined confirmation, and missing noninteractive approval never
create an install, policy file, trust record, or run. An unchanged installed
snapshot reports `already_installed` without confirmation.

### `workflows authorize <id>@<version> [--input <key=value>]... [--inputs-file <path>] [--policy-output <path>] [--dry-run | --approve]`

Plans or applies only the exact missing capabilities reported by prospective
`workflows check` validation. It performs no live plugin probes and never installs a package, imports trust, edits
`config.yaml`, persists inputs, or creates a run. The full JSON report uses
`schema: "transist.workflow_authorization.v1"` and always has
`run_created: false`.

The default destination is a digest-addressed file beneath
`policy/workflow-authorizations`; it must be covered by `policy.sources`.
Existing operator files are never replaced. Interactive use shows the exact
resources, operations, and output path before confirmation; rerun
`workflows check` after a successful authorization.

Passing required validation with no missing capabilities returns
`already_authorized`, with zero plugin probes or policy writes. Before publishing,
the runtime reevaluates current inputs, resources, trust, secrets, and effective
policy and compares the exact missing-grant fragment and destination to the
reviewed plan. Any change aborts without writes and requires another review.
Authorization success is not live readiness; use `check --live` or `--output`
when live readiness evidence is needed. Preparation always checks live readiness.

```bash
transist workflows authorize my_flow@0.1.0 --inputs-file inputs.json
transist workflows authorize my_flow@0.1.0 --input repo_path=/path/to/repo
```

`--input` is repeatable and overrides a same-named value loaded from
`--inputs-file`, matching `workflows check`, `prepare`, and `run`.

Concise outcomes label the exact fragment as **Grants to add** (planned),
**Grants added** (applied), **No changes needed** (already authorized), or
**No grants written** (declined/blocked). JSON never prompts; noninteractive
application requires `--approve`, including when output is piped.

### `workflows list`

Lists compiled workflow recipes currently stored in the local registry.

### `workflows inspect <id>@<version>`

Shows the compiled recipe and workflow summary for one compiled workflow.

### `workflows uninstall <id>@<version>`

Removes an installed workflow recipe and its matching entry from
`<home>/registry/workflow-install-index.json`. Unlike plugin uninstall, workflow
uninstall does not retain that install-index entry as immutable installation
history. Transist refuses uninstall while any protected run still references
that workflow, including created, planned, running, waiting, blocked, canceled,
and failed runs that have not completed failed-run cleanup.

---

## Trust Management

### `trust import <trust-root-file>`

Imports one `transist.trust_root.v1` public-key JSON record into the runtime trust store at `<home>/trust/keys/<key_id>.pub`.

```bash
transist trust import test-org.pub
```

The runtime validates that `key_id` matches the SHA-256 of the raw public-key bytes before persisting the record.

Trust commands that take a `<key-id>` argument (`inspect`, `export`, `remove`)
require the value to match `^[A-Za-z0-9][A-Za-z0-9._-]*$`: an ASCII
alphanumeric first character followed by any number of ASCII alphanumerics,
`.`, `_`, or `-`. An empty value, `.`/`..`, any path separator, an absolute
path, embedded whitespace, or non-ASCII characters are rejected before any
`trust/keys/<key_id>.pub` path is touched.

### `trust list`

Lists the trust roots currently imported into the resolved Transist home.

```bash
transist trust list
```

### `trust inspect <key-id>`

Loads one imported `transist.trust_root.v1` record from the resolved Transist home.

```bash
transist trust inspect 3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55
```

### `trust export <key-id> --output <path|->`

Writes one imported `transist.trust_root.v1` record back out of the resolved Transist home.

```bash
transist trust export 3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55 --output ./test-org.pub
transist trust export 3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55 --output -
```

### `trust remove <key-id> --yes`

Removes one imported trust root from the resolved Transist home.

```bash
transist trust remove 3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55 --yes
```

This is a local trust-store lifecycle operation, not historical revocation. It affects future verification decisions only and does not rewrite existing run records.

### `runs evidence export <run-id>`

Reads the run's persisted `state.json` and `provenance.json` and renders a read-only evidence summary.

- Plain output is markdown for operator review or attachment.
- `--format json` returns the same summary as structured JSON plus the rendered markdown body.

### `runs snapshot export <run-id>`

Builds one validated `transist.inspection.snapshot.v1` envelope from local inspection and metrics state.

```bash
transist runs snapshot export run-123 --source-id src_example --workspace-id example/workflows
transist runs snapshot export run-123 --source-id src_example --workspace-id example/workflows --output ./snapshot.json
transist runs snapshot export run-123 --source-id src_example --workspace-id example/workflows --sync-mode extended --include-workflow-input-values --include-event-payloads
```

- Child run ids resolve to the root run tree.
- `--output -` writes the snapshot JSON to stdout.
- `--output <path>` writes the snapshot JSON to the requested file and returns export metadata on stdout.
- `--max-payload-bytes` enforces the existing snapshot payload cap before any file is written.
- `--sync-mode metadata_only|extended` and the `--include-*` flags reuse the existing snapshot sync-policy contract.
- The command uses local inspection and local metrics only. It does not require central configuration or network access.

---

## Running Workflows

### `workflows run <id@version> [--run-ref <run-id>] [--input <key=value>]... [--inputs-file <path>] [--trusted-execution] [--local-only] [--readiness-file <readiness.json>] [--max-age <seconds>]`

The most common way to run a workflow from a compiled workflow reference. `run <id@version>` expects the compiled recipe to already exist in the local registry. In standalone or development homes, install authored source with `transist workflows install --path <path>`; governed homes install a signed package with `transist workflows install --package <directory>`. `run` then rechecks readiness, prepares, and executes the run in one go. When the workflow version is `v0`, the CLI prints a warning banner to stderr before execution continues.

```bash
# Install the workflow into the local registry first
transist workflows install --path my_workflow.yaml

# Run a workflow and let Transist generate a run ID
transist workflows run my_id@0.1.0 --input key=value

# Run with a specific run ID
transist workflows run my_id@0.1.0 --run-ref my-run-123
```

For a workflow that declares `workspace.path`, run binds and canonicalizes
that path from its inputs before run birth. The path becomes the execution CWD,
sandbox workspace, and capability resource; the directory from which the CLI is
called is only source/command context and cannot override it. Supply a new
`repo_path` only through a fresh run. There is no workspace-root run flag.
Workflows without `workspace` retain the legacy CLI-directory fallback.

In the fullscreen TUI, running follows workflow selection with a fresh
execution-mode step, then run-reference and input steps. Governed homes select
trusted execution automatically; standalone and development homes preselect
standard execution. The TUI shows progress while validating and
preparing the run. Passing validation continues to live preparation without an extra review modal;
blocked validation opens the searchable concise summary with the same grouped
findings, statuses, input-name hints, and exact capability gaps as the default
`workflows check`. Launch reviews use typed summaries without constructing full
reports or readiness evidence files. Use the CLI with `--format json` for a full
diagnostic report.
The input editor prefixes every declared required input with `*`, including
inputs supplied by a file or satisfied by a default. Inputs omitted from the
optional YAML/JSON file show their compiled workflow defaults as editable JSON
values tagged `[default]`. File values take precedence and retain `[file]`;
committing a changed value marks it `[edited]`. Browsing or submitting an
unchanged default does not create an explicit override. Clearing an override
restores the workflow default when one exists.
The input editor rejects a required field cleared from a loaded inputs file
when it has no default;
an explicitly empty value that reaches readiness is called out by name rather
than only as a `workspace.path` or action-schema error. When only exact
authority is missing, the TUI shows one authorization review with the exact
grants and policy destination, using the same concise renderer as
`workflows authorize`. Confirmation defaults to **No**; declining writes no
grants. After application, the TUI shows the **Grants added** receipt before
rerunning fast summary validation. Applying uses the exact reviewed fragment;
changed inputs/resources/policy abort without writes and require another review.
Already-authorized results recheck without another
grant confirmation. Long authority values wrap into scrollable pager rows
without ellipsis, including on small terminals. No run is created
unless validation passes and preparation establishes live readiness. The selected mode and entered values are preserved
while navigating backward; Ctrl+C cancels the complete run flow.

### `workflows prepare <id@version> [--run-ref <run-id>] [--input <key=value>]... [--inputs-file <path>] [--trusted-execution] [--local-only] [--readiness-file <readiness.json>] [--max-age <seconds>]`

Stages a workflow run from a compiled workflow reference and performs "preflight" checks (like verifying secrets and plugins) but **does not start execution**. `prepare <id@version>` also expects the compiled recipe to already exist in the local registry. Fresh preparation captures resolved source-backed prompt and `source://` assets from the recipe's direct steps and cleanup entries into private run-root storage as part of atomic run birth. Snapshot-backed runs use those captured bytes for dispatch and re-entry; legacy records retain authored-source behavior. For normal workflow versions, changed pinned-plugin bytes still surface as failed `asset_lock.plugin` checks. Reachable embedded subflows are also preflighted recursively, and child blockers are surfaced on the root run with parent-prefixed `step_ids` such as `plan_loop/generate_plan`. If a nested child run already exists, recursive preflight reuses that persisted child snapshot for inspection while rebinding the child workflow inputs from the current parent step mapping before evaluating child blockers. When the prepared workflow version is `v0`, the CLI prints a warning banner to stderr and the persisted `state.json` carries `"test_mode": true`.

Both `prepare` and `run` generate a run ID when `--run-ref` is omitted.

Before run birth, `prepare` and `run` apply the same deterministic readiness
checks as `workflows check <id>@<version> --live`. A failed check returns the readiness envelope with
`run_created: false` and creates neither a run directory nor a locator. Failed
work after external dispatch remains a durable execution failure.

`--readiness-file` requires canonical evidence previously created by `workflows
check <id>@<version> --output`. It is revalidated immediately before run birth and is rejected
with `run_created: false` if it is malformed, modified, future-dated, older than
the allowed age, or differs from current workflow, input, workspace,
dependency, trust, policy, settings, or preflight fingerprints. `--max-age`
requires `--readiness-file`, defaults to 300 seconds, and accepts values from
1 through 300. This evidence never resolves secrets and does not replace
prepare-time runtime checks.

```bash
transist workflows prepare my_id@0.1.0 --run-ref my-run-123
```

### `runs start [--trusted-execution] [--local-only]`

Starts or resumes a previously prepared or interrupted run. Before any remaining invoke work is dispatched, Transist re-verifies the run's persisted asset locks so replay, resume, or `workflows run` cannot continue with changed/deleted locked `prompt_file` contents. For normal workflow versions, pinned-plugin drift is also rejected. For `v0` runs, Transist allows the run-scoped `recipe.yaml` to be patched and re-used, and it ignores pinned-plugin drift while still enforcing `prompt_file` locks. The CLI prints a warning banner for those runs.

It also uses the run's persisted workspace, not the current directory. Resume,
retry, cleanup, cancellation recovery, and replay preserve that workspace;
create a fresh run to use another `repo_path`.

```bash
transist runs start my-run-123
```

With `--trusted-execution`, `prepare`, `workflows run`, and `runs start` refuse unsigned, untrusted, malformed, or modified executable inputs before step dispatch. The run record persists `trust_decision` with schema `transist.trust_decision.v1`.

The fullscreen TUI shows installed plugin IDs and versions as a read-only
inventory. Install and uninstall plugins with the `transist plugins` CLI
commands; governed homes require trusted installation.

It also shows installed workflow IDs and versions as a read-only inventory.
Use the CLI for workflow lifecycle work. In governed homes, create the package
with `transist-admin workflow package` and publish it with `transist workflows
install`; use `transist workflows install` for standalone or development
homes. The dashboard supports workflow runs, run browsing and recovery,
local Preferences, refresh, and quit. Its Problems view derives read-only
runtime findings from local state and opens a diagnostic for the selected item.
Dashboard operation failures remain in fullscreen mode as a one-line status
with an `Open Problems for full diagnostic.` hint. Failed runs and persisted
preflight, capability, policy, trust, asset-lock, cleanup, and lease findings
offer `View full diagnostic` before recovery actions. Reports use only persisted
inspection data; plugin, adapter, model, and stderr excerpts are labeled
untrusted external output.

The diagnostic pager supports `c` to emit the exact redacted report through
OSC 52. It sends no clipboard sequence when the unencoded report exceeds
65,536 bytes. `e` writes the same report to the preferred pager, then `$PAGER`;
when neither is configured, it keeps the internal pager open. This pager never
executes suggested commands. Plugin, workflow, trust, policy, and
inspection-sync data shown by the TUI is read-only. Preferences only change
local pager, editor, gate-actor, and inspection-cache settings. Home setup,
trust, packages, workflow/plugin lifecycle, runtime configuration, policy, and
inspection-sync setup remain `transist` CLI or `transist-admin` operations.

With `--local-only`, `prepare`, `workflows run`, and `runs start` deny any invoke step whose effective plugin-action `provider_kind` is `external` before dispatch. Locality is declared in the plugin manifest; it is not inferred from `adapter` or `execution_kind`. HTTP, local process, and local MCP steps are additionally capped at `policy.network.mode = loopback`; authored `any` and `allowlist` modes are narrowed to `loopback`, authored `none` is preserved. This ceiling cannot be widened by workflow policy, DNS, or proxies. Local-only runs persist `state.json.local_only_mode = true`, emit a `local_only` preflight check, and keep `provenance.json.external_transfer_summary.count = 0` because no external step is allowed to dispatch.

---

## Managing Runs

### `runs list`

Shows one-line status summaries for recent root runs. Use `--all` to include nested child runs created by loops or subflows, or `--ids-only` to emit the legacy compact JSON array of run ids.

### `runs cancel <run-id>`

Interrupts an active run (status `running` or `waiting`) by durably requesting
cancellation and immediately signaling the deepest verified local invocation in
the root run tree when one exists. Repeated requests during cancellation cleanup
are idempotent: they do not interrupt cleanup or duplicate its dispatch. Other
terminal runs are rejected.

The command waits up to 15 seconds for the root run to persist status
`canceled`, including automatic `when: [canceled]` cleanup. It exits `0` only
after that settlement and emits JSON with `settled: true` and
`final_status: "canceled"`. It exits nonzero after emitting the same report
shape when cancellation cannot settle, the run reaches another terminal state,
or state/runner recovery cannot be read or started. The durable request remains
in place on failure.

If no live root-tree lease owner remains, `runs cancel` starts one detached
recovery runner for the root run to consume the durable request. A live lease
prevents a competing runner from being started.

### `runs status <run-id>`

Provides a concise, human-readable summary of a specific run's current state and suggests the next logical command.

### `runs show <run-id>`

Displays the complete, raw JSON state of a run, including `contained_runs` when the run has nested children. Use this for deep-dive debugging. The persisted `preflight.checks[]` list may include `asset_lock.plugin` and `asset_lock.prompt_file` failures when `prepare` or `runs preflight` detects run-asset drift. For `v0` runs, the CLI prints a warning banner to stderr and the JSON state includes `"test_mode": true`.

- **`--tui`**: Opens the interactive run-detail viewer for a snapshot of the run.
- **`--failed`**: Only shows steps that failed.

Local CLI/TUI file lists show absolute filesystem paths for workflow files,
artifacts, and evidence, including files displayed under child subflows and gate
reviews. Workflow-file paths come from their validated execution workspace,
repository-state directory, or run locator, not the inspecting terminal's current
directory. The TUI `o` action opens the displayed absolute path. When local path
context is unavailable, details show `path unavailable` and do not offer a
relative path for opening. Companion HTTP responses retain host-independent
relative locators and content links.

### `runs tail <run-id>`

Shows the live (or historical) event log for a run.

```bash
transist runs tail my-run-123 --limit 50
```

### `runs watch <run-id>`

Attaches a live, fullscreen viewer to an ongoing or finished run.

### `runs lease`

Inspect or break run ownership leases.

### `runs lease inspect <run-id>`

### `runs lease break <run-id> --yes-i-understand [--force-live-owner]`

- **`runs lease inspect <run-id>`**: Prints the persisted lease record for a run.
- **`runs lease break <run-id>`**: Force-breaks a run lease, releasing ownership so another process can pick it up. Requires `--yes-i-understand` acknowledgement. Use `--force-live-owner` to break the lease even when the owner process appears to still be alive.

### `runs preflight <run-id> [--trusted-execution] [--local-only]`

Re-runs preflight checks for an existing run without dispatching work. The command leaves the run status unchanged, but it persists the latest preflight snapshot back onto the run record so `runs show` reflects exactly what was checked. When run-asset drift is detected, the returned report and persisted snapshot include failed `asset_lock.plugin` or `asset_lock.prompt_file` checks instead of aborting with a single top-level error. Reachable embedded subflows are also walked recursively, and any child blockers are recorded on the root run's preflight snapshot with parent-prefixed `step_ids` such as `plan_loop/generate_plan`. If a nested child run already exists, the preflight walk reuses that persisted child snapshot for inspection while rebinding the child workflow inputs from the current parent step mapping before evaluating child blockers. For `v0` runs, plugin drift is ignored but `prompt_file` drift is still reported. The CLI still prints the test-run warning banner because recipe snapshot patching remains enabled.

When preflight detects `capability_not_granted`, it keeps stdout as the structured report and writes a stderr hint for `runs capabilities`.

### `runs capabilities <run-id> [--exact]`

Reads the run's persisted normalized capability map and compares it to the current effective local allowlist. YAML is the default and prints a copyable `transist.policy_fragment.v1` document. Save it beneath `<home>/policy/`, then add its directory or filename to `policy.sources` in `<home>/config.yaml`. JSON exposes the same object for automation. `--exact` emits the normalized missing-request root list (or JSON array) for diagnostics and existing scripts.

This command is inspection-only. It does not mutate a run, `inputs.yaml`, or the effective home policy. In governed mode, `<home>/config.yaml` remains the authority enforced by the runtime.

### `policy validate`

Validates the composed local runtime policy.

### `policy show --effective`

`policy validate` loads the effective home configuration and every declared policy fragment without mutating anything. `policy show --effective` prints the flattened configuration after fragment composition; use `--format json` for JSON output.
Global `policy.allow` grants are additive for every workflow. The concise output
uses a named set bound to the exact workflow reference; inputs cannot select it.

With `--trusted-execution`, preflight refreshes the run's trust decision first and emits a `trusted_execution` preflight check.

With `--local-only`, preflight emits a `local_only` check and carries the same dispatch-time denial posture the run would use for external-provider actions.

Trusted-mode re-entry is sticky for persisted runs: after a run is prepared or run with trusted execution, `runs start`, `runs retry`, `runs resume`, `runs replay`, and `runs cleanup` re-verify trust even when you do not repeat the flag.

### `runs sync-recipe <run-id>`

Refreshes an existing test run's `recipe.yaml` from the current compiled registry recipe for the same workflow id and version. This command is limited to `version: v0` runs and updates the stored `recipe_snapshot.sha256` after writing the new bytes. It rejects non-test runs and active `running` or `waiting` runs.

### Test Workflow Reminder

For `version: v0` workflows:

- An approved workflow installation may overwrite the existing compiled registry recipe.
- `runs sync-recipe <run-id>` copies the current registry recipe into that run's `recipe.yaml` and refreshes the stored snapshot digest.
- `prepare`, `workflows run`, `runs start`, `runs resume`, `runs retry`, `runs replay`, `runs cleanup`, `runs preflight`, and `runs show` print a warning banner to stderr.
- Re-entry uses the persisted run-scoped `recipe.yaml`, so manual patching of that file is honored.
- Plugin asset locks are bypassed.
- `prompt_file` asset locks are **not** bypassed.

### `runs cleanup <run-id> [--trusted-execution] [--local-only]`

Recovers unfinished or failed cleanup for a terminal root run. Every explicitly
declared cleanup runs automatically when its `when` matches the root
outcome (`success`, `canceled`, or `failed`); this command is not a mandatory
failed-run trigger. No matching branch means no action, and failure never
implicitly selects cancellation cleanup. Cleanup records its own result without
changing the primary run status.

Failed-run cleanup durably changes recovery to `cleanup_required` before
preflight or dispatch, so `runs retry` and `runs resume` are unavailable once
it starts, whether automatically or during recovery. A failed run without
matching cleanup remains recoverable. Use replay for new primary work after
failure cleanup begins. If cleanup fails or abrupt runner death prevents it
from finishing, run this command again; committed successful cleanup steps are
not repeated. Durable history supports recovery, not guaranteed completion.

In `--ui plain` mode (the default), the CLI waits for cleanup to reach a terminal state (`succeeded`, `failed`, or `skipped`) and prints a compact result summary to stderr. In `--ui tui` mode, the command waits for cleanup to settle, then opens a pager-style modal with the cleanup result. After that, the run-detail screen keeps a compact cleanup status line and offers a `View last cleanup result` action instead of a synthetic cleanup step. Interrupting the foreground CLI while cleanup is in progress does **not** stop the detached runner — cleanup continues independently.

```bash
transist runs cleanup my-run-123
```

### `runs retry <run-id> [--trusted-execution] [--local-only]`

Retries the failed step(s) of a failed or canceled run. This is the preferred alias for `runs resume`: it keeps the same run ID, preserves execution history, and re-enters the run from persisted state to retry only the failed or canceled steps. Before any remaining invoke work is dispatched, Transist re-verifies the run's persisted asset locks so retry, resume, or `workflows run` cannot continue with changed/deleted locked `prompt_file` contents. For normal workflow versions, pinned-plugin drift is also rejected.

```bash
transist runs retry my-run-123
```

For failed runs, retry/resume requires recovery to remain valid: a failed run
without matching cleanup stays recoverable, but once failure cleanup begins
primary retry/resume is blocked. Recover cleanup with `runs cleanup`; use
`runs replay` for new primary work from the same pinned snapshot.

Trusted standalone and governed re-entry re-verifies trust before this recovery
refusal. With unchanged signed inputs and trust roots, a rejected retry/resume
may refresh `state.json.trust_decision.decided_at_ms` and
`state.json.updated_at_ms` audit timestamps. It does not resume execution:
recovery state, primary/cleanup execution counts, resume counters, and execution
history remain unchanged, with no `run_resumed` event. Untrusted standalone
recovery refusal leaves the complete run state unchanged.

### `runs resume <run-id> [--trusted-execution] [--local-only]`

Alias for `runs retry`. Retries the failed step(s) of a failed or canceled run. Kept for backward compatibility; new scripts and TUI flows use `runs retry`.

```bash
transist runs resume my-run-123
```

### `runs replay <run-id> [--trusted-execution] [--local-only]`

Creates a new, independently owned run from the source run's persisted recipe snapshot and persisted inputs. The new run records the source in `source_run_id` and uses its own root for leases, cancellation, cleanup, and storage. Snapshot-backed replay copies the source run's validated workflow-asset manifest and blobs; legacy records retain authored-source behavior. With `--trusted-execution`, the source run's workflow and referenced plugins are re-verified before replay continues. With `--local-only`, the replayed run denies external-provider steps before dispatch even if the source run was created without local-only mode.

Replay deliberately preserves the source run's persisted workspace and inputs.
It is not a way to substitute a new `repo_path`; create a fresh run when the
workspace or any input must change.

---

## Timer Vocabulary

Run inspection surfaces (`runs list`, `runs status`, `runs show`, `runs watch`, the fullscreen TUI, and `transist-server` HTML) all use the same three timer fields. They answer three operator questions:

- **Wall clock** — *how long did it exist?* Lifecycle elapsed time from `started_at_ms` to `finished_at_ms` (or `now` for active/blocked runs). Equivalent to the JSON field `wall_clock_duration_ms`.
- **Active** — *how much work ran?* Sum of concrete step-attempt execution intervals. JSON field: `active_execution_duration_ms`.
- **Blocked wait** — *how long did we wait?* Sum of manual-gate wait intervals between block and resolution. JSON field: `blocked_wait_duration_ms`.

Example: a 10-minute run that spent 7 minutes waiting on a manual gate and 3 minutes doing real work reports `wall clock 10m | active 3m | blocked 7m`.

Backward compatibility: the JSON field `total_duration_ms` remains a wall-clock value for existing API clients.

---

## Manual Gate Decisions

Manual gates with `with.review_documents` retain the exact review content at the
first waiting execution. `runs show` plain blocked summaries, the fullscreen TUI
and server Gates view show names, counts, sizes, IDs, digests and content links.
JSON inspection includes the persisted `review_request`. Approve/reject commands
bind to that request automatically; they do not accept replacement document refs.
Waiting restart retains it, and a read binding with `reviewed_by` requires the
same revision ID and digest before its first consumer probe. A new revision needs
new approval even when its digest is unchanged. Gate IDs and recipe-local step
IDs are distinct; document review never approves arbitrary workspace mutations.

### `runs gate approve <run-id> [<gate-id>] --actor <actor> --reason <reason>`

### `runs gate reject <run-id> [<gate-id>] --actor <actor> --reason <reason>`

### `runs gate request-changes <run-id> [<gate-id>] --actor <actor> --reason <reason>`

When a workflow is blocked on a `control.gate.manual` step, use these commands to move it forward:

- **`approve <run-id> [<gate-id>] --actor "Reviewer <reviewer@example.com>" --reason "Reviewed output and approved"`**: Approves the gate and resumes the workflow.
- **`reject <run-id> [<gate-id>] --actor "Reviewer <reviewer@example.com>" --reason "Stop the run"`**: Rejects the gate, stopping the workflow.
- **`request-changes <run-id> [<gate-id>] --actor "Reviewer <reviewer@example.com>" --reason "Needs another pass"`**: Sends the workflow back for changes (often resetting a loop).

`--reason` is required for all three decisions. Actor resolution precedence is `--actor`, then `TRANSIST_ACTOR`, then `manual_gates.actor` in `<home>/config.yaml`, then `identity.actor` in the user settings file:

```yaml
manual_gates:
  actor: reviewer@example.com
```

```yaml
schema: transist.user-settings.v1
identity:
  actor: reviewer@example.com
```

On macOS the user settings file lives at `~/Library/Application Support/transist/settings.yaml`.
In the fullscreen TUI you can manage this user preference through Preferences -> `Set/Unset gate actor`.
If no actor resolves from any source, TUI gate decisions prompt for a one-off actor before the decision is recorded.

Reason resolution checks `--reason` first and then `TRANSIST_REASON`.

In the fullscreen TUI and interactive terminal action picker, `approve` and
`request-changes` ask for confirmation after a non-empty reason is entered.
The confirmation defaults to **No**. Confirming records the decision and resumes
the blocked run; requesting changes may rerun preceding work. Declining returns
to the current screen or picker without recording a decision or resuming the run.
In the TUI, **Esc** also declines. Direct `runs gate` CLI commands do not add this
confirmation, so scripts retain their existing behavior.

---

## Plugin Management

### `plugins install --path <path> [--trusted-execution] [--signature <path>]`

Installs a plugin into your Transist home.

When the active home is governed and the plugin's signer has not been imported,
use `transist trust import <trust-root-file>` first. The public root must come from the
plugin publisher or your administrator; never import a private key into the
runtime home.

`<path>` is preferred as the plugin's `plugin.yaml` manifest file (for
example `plugins/shell/plugin.yaml`), aligning with
`workflows install --path workflow.yaml`. A plugin directory is still
accepted as a compatibility input. Plugin versions are content-immutable:
reinstalling the same `<plugin-id, version>` with different bytes is rejected
even if the old files were previously removed. Bump the plugin version to
publish changed content.

The installed and signed payload is exactly `plugin.yaml` plus the manifest's
non-empty `package.files` list. Files not selected by `package.files` are not
copied into the installed plugin and cannot be runtime dependencies.

With `--trusted-execution`, install verifies the plugin sidecar before copying
bytes and stores the detached signature next to the installed plugin directory
so trusted runs can re-verify it later. `--signature <path>` selects an
explicit source sidecar; otherwise the runtime uses `<plugin-directory>.sig`.
An explicit path never falls back to that convention. The option does not
itself enable trusted execution, and the external source file may be removed
after a successful trusted install because the verified bytes are persisted in
the runtime home.

### `plugins list`

Lists all installed plugins and their versions.

### `plugins inspect <plugin-id>`

Shows the full technical manifest of an installed plugin.

### `plugins uninstall <plugin-id>`

Removes an installed plugin. Use `--version <v>` to remove only a specific version.
Transist refuses uninstall while any protected run still references that plugin
(`created`, `planned`, `running`, `waiting`, `blocked`, `canceled`, or a failed
run that has not completed failed-run cleanup).

Uninstall removes the installed package files but retains the version's digest
in `<home>/plugins/install-index.json`. The index is immutable installation
history rather than current inventory, and prevents different bytes from later
reusing the same `<plugin-id>@<version>`. Use `transist plugins list` to inspect
the versions that are currently installed.

---

## Server Command (`transist-server`)

The `transist-server` binary starts the read-only companion inspection server.
It exposes local Transist run and workflow state over HTTP without any write or
control surface.

```bash
transist-server [--workspace-root <path>] [--home <path>] [--bind <ip>] [--port <u16>] [--allow-non-local-bind]
```

### Options

- **`--workspace-root <path>`**: Workspace root used to resolve workflows and
  local defaults. Defaults to the current working directory. The legacy alias
  `--repo-root` is also accepted.
- **`--home <path>`**: Override the effective Transist home for this server
  process. Resolution falls through the same chain as the CLI: this flag, then
  `TRANSIST_HOME`, then the persisted user-set home, then the per-user default
  (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`).
- **`--bind <ip>`**: IP address to bind. Defaults to `127.0.0.1` (loopback
  only). Must be an IP address, not a hostname.
- **`--port <u16>`**: TCP port. Defaults to `7777`.
- **`--allow-non-local-bind`**: Required when binding to a non-loopback
  address. The flag must be passed explicitly to opt in; omitting it while
  specifying a non-loopback `--bind` address is a hard error.
  The server has no built-in authentication or TLS; expose it only through a
  trusted reverse proxy that provides authenticated TLS access.

### Usage examples

```bash
# Start with defaults (loopback, port 7777, cwd as workspace root)
transist-server

# Custom port and workspace root
transist-server --port 8080 --workspace-root /path/to/project

# Bind to all interfaces (explicit opt-in required)
transist-server --bind 0.0.0.0 --allow-non-local-bind

# Use a specific home
transist-server --home /path/to/transist-home
```

The server exposes `GET /health`, `GET /metrics`, `GET /api/v1/...` JSON
inspection routes, and the `GET /inspect/...` HTML UI. See the
[Companion Server API reference](companion-server-api.md) for the full endpoint
inventory.

---

## Home Management

### `home init`

Initializes a new Transist home directory. With no `--home` flag and no
`TRANSIST_HOME`, the home is the per-user default
(`${XDG_STATE_HOME:-$HOME/.local/state}/transist`), recorded as the
persisted user home. New homes default to governed execution and the
`allowlist` policy profile. Use `--mode standalone` or `--mode development`
only for an explicitly selected non-governed home. Idempotent: safe to run on
an already-initialized home; an existing `config.yaml` is never rewritten.
See [Runtime Modes](runtime-modes.md) before selecting a non-governed home.

```bash
transist home init [--mode governed|standalone|development]
```

Governed homes require exact capability grants and automatically require trusted
workflow/plugin verification for install, prepare, workflow runs, resume, replay,
cleanup, and preflight. Initialization creates the public trust-root directory at
`<home>/trust/keys`; private signing keys must remain outside the runtime home.
Missing `config.yaml` fails closed.

### `home get`

Prints the currently active Transist home as JSON. The effective home path is in
the `effective_home` field; extract it with
`transist home get | jq -r .effective_home`. Machine-readable output is always
JSON for this command regardless of `--format`.

Runtime-wide filesystem policy defaults live in `<home>/settings.yaml`. The policy has two concerns: `workspace_access` controls access *inside* the workspace, while top-level `read_allow_paths`/`write_allow_paths` grant additive access to paths *outside* the workspace (e.g. git config files, SSH keys).

```yaml
filesystem_policy:
  # Controls access inside workspace_root (full | read_only).
  # custom is rejected with filesystem_custom_mode_unsupported.
  workspace_access:
    mode: full
  # Optional: grant read access to specific paths outside workspace_root.
  # Absolute paths (/…) or home-relative paths (~/…) only.
  read_allow_paths:
    - /etc/gitconfig
    - ~/.gitconfig
    - ~/.config/git
    - ~/.ssh
  # Optional: grant write access to specific paths outside workspace_root.
  write_allow_paths: []
```

Update this directly in `<effective-home>/settings.yaml`. Resolve the effective
home with `transist home get`, then edit the file with any editor. The fullscreen
TUI displays the effective policy read-only.

The TUI `Setup` pane also shows:

- the effective home runtime settings file path
- the current runtime filesystem policy summary

### Filesystem Policy Display

- `transist runs status <run-id>` and the TUI run detail overview show the global default policy and workflow-level policy when available.
- Step detail surfaces show the effective step policy source (`builtin`, `global`, `workflow`, `step`), the effective `workspace_access` mode, and any external path grants.
- Failed-step inspection and runtime event views surface `filesystem_policy.denied` and `filesystem_policy.sandbox_failed` payloads with source, mode, operation, and path.

### `home set <path>`

Persists a user-level default Transist home path into the user-scoped settings file. The path is used flat — there is no per-repository namespacing. An explicit `--home` flag or `TRANSIST_HOME` still takes precedence when present. `save` also initializes the target directory if it does not exist yet.

### `home unset`

Removes the persisted user-level default from the user-scoped settings file. After this, resolution falls through to the per-user default home (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`), unless `--home` or `TRANSIST_HOME` is supplied. If neither `HOME` nor `XDG_STATE_HOME` is set (and no env override exists), non-home commands will fail with a `NoHomeError`.

For guidance on when to use which home strategy, see the [Operating the Transist Home Directory guide](../guides/inspection/operating-the-transist-home-directory.md#choosing-a-home-strategy).
