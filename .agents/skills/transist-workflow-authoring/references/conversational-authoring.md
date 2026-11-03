# Conversational authoring reference

## Practical sequence

Synthetic brief: “Read diagnostics, ask before sending them to a model, retain
advice; do not act on the machine. One attempt; rejection stops.” Capture these
decisions first. Ask only for the missing workspace, model and provider binding.
Do not ask where intermediate notes should live. Model integration is not bundled:
report it as missing unless the operator supplies its exact plugin contract. Do
not search for a sibling catalog checkout. For a local-only baseline, inspect
`plugins/shell/plugin.yaml` and its selected schemas (`exec`, `exec_with_secret_env`
and `echo`), plus the bundled workflows. Generate only supported source, fictional
inputs, validate, explain, and present execution separately.

If publication is requested, require an explicitly supplied consumer contract
before choosing an action or file field. Missing company-wiki integration is an
unresolved prerequisite, not permission to invent `wiki/publish` or use
`shell/exec` with curl. State supported alternatives only for an explicit decision.

## Bindings

```yaml
documents:
  draft: {kind: file, max_bytes: 65536}
  ledger: {kind: file, max_bytes: 4096, initial: '{}'}
  attachments: {kind: tree, max_bytes: 1048576, max_files: 16}
```

The committed offline test plugin at
`.agents/testing/e2e_suites/managed-documents/plugin/plugin.yaml` provides
`fixture.documents/call`. Its `input.json` and `plugin.py` define the `ops` path
fields and read/write operations. This is a test fixture, not a production
integration. A self-contained exact-review fragment using literal initial content:

```yaml
plugins:
  fixture.documents: 0.1.0
documents:
  draft: {kind: file, max_bytes: 65536, initial: 'reviewable draft'}
steps:
- id: review_draft
  control: control.gate.manual
  with:
    review_documents: [draft]
- id: consume
  uses: process:fixture.documents/call
  with:
    ops:
      - path: {$document: draft, access: read, reviewed_by: review_draft}
        op: read
        content: 'reviewable draft'
```

This is a fragment: supply the flow schema, ID, version and root workspace, and
select the fixture package in an isolated test home before resolving its pin.
The fixture tests in `tests/revised_authoring_document_gates.rs` demonstrate full setup.
It does not publish anything or authorize external calls.
An action supporting mutable file input can use
`{$document: ledger, access: update}`; HTTP/MCP cannot materialize these paths.

A module's `outputs: {ready: '{{steps.check.out.approval_ready}}'}` exports the
actual Boolean. Its caller uses `{{steps.review.out.ready}}` directly, without a
JSON-to-shell-to-string conversion. Gate references remain module-local even
though documents are inherited from the root.

## Anti-patterns

| Avoid | Use |
| --- | --- |
| Ask for temp paths/ledger IDs already owned by the runtime | Required behavioral decisions and real input bindings only |
| Copy each output into `vars` or a workspace temp file | Typed direct output references; bounded managed documents for files |
| “Two approvals” in one gate's description | Two distinct sequential gates; protected consumption after the second |
| Approve once, modify the draft, consume old approval | Review the exact current revision again |
| Invent an integration or hide publication in unrestricted shell | Report missing action/contract explicitly |
| Treat a goal, explanation, or green source check as consent | Separate execution approval, exact authority and live readiness |
| Bump to `0.1.1` or replace installed `0.1.0` bytes | Fixed `0.1.0`, new byte-bound signature/bundle and fresh home |

## Source-of-truth bindings

Paths below are relative to this repository. Bundled examples, plugins and
regression tests are available without another repository checkout.

| Question | Authoritative source |
| --- | --- |
| YAML, JSON type preservation, module exports, deferred data | `docs/public/reference/workflow-schema.md`; `crates/transist-compiler/src/validate/dataflow.rs` |
| Process-only document paths and exact review | `docs/public/reference/path-and-resource-semantics.md`; `tests/revised_authoring_documents.rs`; `tests/revised_authoring_document_gates.rs` |
| Actual selected action/pin/binding/limit | Installed `plugin.yaml` and selected `schemas/*`; for the bundled local process contract, `plugins/shell/plugin.yaml` and its selected schemas |
| Read-only validation/explanation commands and flags | `docs/public/reference/cli.md`; actual `transist workflows check --help` / `explain --help` |
| Executable document/review scenarios and revision diff | `tests/revised_authoring_documents.rs`; `tests/revised_authoring_document_gates.rs`; `crates/transist-cli/tests/workflow_explain.rs` |
| Bundled workflow and plugin discovery | `workflows/`; `examples/`; `plugins/` |

Run `bash scripts/ci/validate_docs_and_skills.sh` for skill/docs validation and
`python3 scripts/ci/test_conversational_authoring_skill.py` for instruction
regressions. `bash scripts/ci/run_shipped_workflows.sh` exercises every bundled
workflow using committed fixtures and isolated homes. These deterministic tests
do not prove assistant behavior or authorize external service calls. Model
and publication integrations are not bundled plugins; report them as unresolved
unless the operator has supplied their exact contracts.
