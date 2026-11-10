# Guide: Conversational Workflow Authoring

Ask your existing assistant to load the checked-out
`.agents/skills/transist-workflow-authoring/SKILL.md` **and its references**.
These are ancillary assistant instructions, also included in release archives;
they do not add an autonomous runtime planner. Prefer checked-out guidance to a
cached/global skill. The workflow invoke `skills` field instead passes contained
prompt assets to a participant; it is a different concept.

## Start with the goal

> “Read a diagnostic file, let me review it before sending it to a model, and
> retain advice. Never act on the machine. One attempt; rejection stops.”

This goal includes a model integration, which is not bundled. Without an
explicitly supplied plugin contract, report that integration as missing rather
than inventing an action or looking for another repository checkout. The bundled
workflows provide the self-contained starting point for local-only authoring.

The assistant should capture goal, inputs, outputs, systems, effects, approvals
and repetition, then ask only for missing behavioral decisions or required
bindings (for example workspace, provider and model). It should not ask where to
put temporary notes or invent a ledger ID. Supply no secrets in the conversation.

## The seven-stage authoring loop

1. Capture intent and constraints, including what a revision must preserve.
2. Clarify only missing decisions/bindings, reusing supplied answers.
3. Inspect real installed or explicitly selected catalog action contracts, exact
   pins, supported fields, file bindings and limits. Report unsupported integrations.
4. Generate the `0.1.0` flow, contained prompts and portable fictional sample inputs.
5. Run read-only source validation; retain actual findings.
6. Produce the deterministic CLI explanation and compare revisions.
7. Present unresolved prerequisites separately from a request for execution approval.

An unsupported company-wiki publisher remains a missing integration. Inventing
`wiki/publish` or hiding curl inside unrestricted shell/agent execution is not a
valid authoring solution.

## Generate reviewable dataflow

Use [typed output references and module exports](../../reference/workflow-schema.md)
for ordinary action data, and bounded root managed documents for intermediate
files/trees. Process actions receive private `read`, `write` or `update` copies;
HTTP/MCP document paths are unsupported, though ordinary JSON references work.
Keep source/OpenSpec deliverables in the canonical workspace. See
[Authoring patterns](../../handbook/02-everyday-usage/08-authoring-patterns.md) and
[path semantics](../../reference/path-and-resource-semantics.md).

For example, inspect the bundled [shell manifest](../../../../plugins/shell/plugin.yaml)
and its selected schemas before using `shell/exec` or `shell/echo`. Do not assume
an action accepts `prompt_file` or any other file binding merely because another
plugin does. For document-path examples, inspect the committed
[managed-document test fixture](../../../../.agents/testing/e2e_suites/managed-documents/plugin/plugin.yaml)
and its selected schemas and implementation. It is an offline test plugin, not
a model or publication integration. A gate's `review_documents: [draft]` and a read's
`reviewed_by: review_draft` bind the exact current revision in the same lexical
recipe. Two approvals require two distinct gates; description text is insufficient.
Changed revisions require new review. Document review does not approve workspace
mutations. Deferred payload validation is checked before dispatch, not silently passed.

## Validate, explain, compare

Use the intended authoring home and runtime explicitly. These source operations
do not install, authorize, probe, resolve secret values or dispatch participants:

```bash
transist workflows check --path workflow.yaml --home "$AUTHORING_HOME"
transist workflows check --path workflow.yaml --inputs-file inputs.json --home "$AUTHORING_HOME"
transist workflows explain --path workflow.yaml --inputs-file inputs.json --home "$AUTHORING_HOME" --format json
transist workflows explain --path revised/workflow.yaml --compare-to workflow.yaml \
  --inputs-file inputs.json --home "$AUTHORING_HOME" --format json
```

Review actual resolved pin/contract digests, or explicitly unresolved pins and
inputs, plus authority, gates, dependencies, conditions, limits and cleanup. Prompts
and scripts remain opaque. Explain output is not model-inferred effect enforcement.
For a one-gate → two-gate revision, expect the second gate, graph/order and protected
read changes, plus definition/YAML digests. Do not accept unrelated pin, prompt,
authority or limit changes. Source validation is not live readiness or trust proof.

## Execution is a separate decision

“Author a publication workflow” does not authorize publication, network transfer,
operational secret use, installation, policy writes, private-key use or dispatch.
The assistant delivers source and receipts, identifies unresolved prerequisites,
and asks separately for the exact requested execution scope. An approval inside
an authored gate is a later run decision. Deterministic fake-test permission does
not authorize real services. Follow the separate
[governed lifecycle](../../reference/cli.md#workflow-management) when requested.

Runtime and admin use `0.1.1`; catalog releases/pins remain `0.1.0` and schemas
remain `v1`.
Changed bytes need a matching signature and fresh home, not replacement of an
installed same-version payload. Preserve the old home/bundle for rollback; see
[compatibility](../../reference/compatibility-and-stability.md).
Start in a fresh home; there is no legacy document/review reader, version alias,
or in-place home migration.

## Bundled examples and executable checks

This repository is self-contained: authoring guidance and CI/CD use committed
resources, not separate catalog checkouts.

- [Workflows](../../../../workflows) and [examples](../../../../examples)
  contain the bundled workflow sources.
- The [shell plugin](../../../../plugins/shell/plugin.yaml) and its selected
  schemas define the bundled local process contract. Inspect the actual contract,
  not a guessed action. Model and publication integrations are not bundled;
  author their invocations only after the operator supplies exact package contracts.
- [Managed-document tests](../../../../tests/revised_authoring_documents.rs),
  [review-gate tests](../../../../tests/revised_authoring_document_gates.rs) and
  [explanation tests](../../../../crates/transist-cli/tests/workflow_explain.rs)
  cover document dataflow, exact review and deterministic revision comparison.

Run `python3 scripts/ci/test_conversational_authoring_skill.py` for instruction
regressions and `bash scripts/ci/run_shipped_workflows.sh` for hermetic execution
of every shipped workflow. These checks use repository-local fixtures; they do
not require another repository or prove assistant behavior against real services.
