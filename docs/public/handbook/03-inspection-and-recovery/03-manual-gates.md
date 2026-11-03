# Manual Gates (Human Decisions)

A **manual gate** is a first-class feature in Transist that brings human judgment into a workflow run. It is not just a pause; it is a recorded decision point.

## What happens when a run is "Blocked"?

When a workflow hits a `control.gate.manual` step, the runner stops and waits for a decision. This state is saved in the run record and is visible through the CLI and inspection UI.

## Recording a Decision

To move the workflow forward, an operator must use the CLI to record a decision. There are three options:

1. **`transist runs gate approve <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "Reviewed output and approved"`**: The workflow proceeds to the next step.
2. **`transist runs gate reject <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "..."`**: The workflow stops through the rejection path.
3. **`transist runs gate request-changes <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "..."`**: The workflow records that changes are needed, often inside a loop-backed review path.

Each of these commands creates a permanent record in the `gates/` folder of the run directory. New decisions require a non-empty reason. Actor resolution precedence is `--actor`, then `TRANSIST_ACTOR`, then `manual_gates.actor` in `<home>/config.yaml`, then `identity.actor` in the user settings file.

Gate decisions are stored under the **exact validated gate id**: every authored and persisted gate id must satisfy the identifier rule (ASCII alphanumerics plus `.`, `_`, and `-` after a mandatory alphanumeric first character). No lossy slug normalization is applied, so `review-a`, `review_a`, and `review.a` remain distinct gates. A decision record is only honored when its stored `run_id` and `gate_id` match the request; a mismatched record fails closed rather than being treated as a decision for a different run or gate.

The fullscreen TUI uses the same precedence. You can persist a personal reviewer identity through Preferences -> `Set/Unset gate actor`. If no actor resolves, the TUI prompts for a one-off actor before recording the decision and reminds you that the value can be set in user config.

When selecting `approve` or `request-changes` in the fullscreen TUI or interactive
terminal action picker, enter the required reason, then confirm the action. The
confirmation defaults to **No**. **Yes** records the decision and resumes the
blocked run; requesting changes may rerun preceding work. **No** (or **Esc** in
the TUI) returns to the current screen or picker with the gate unchanged. A blank
or canceled reason stops before confirmation. Direct CLI commands above do not
ask for this additional confirmation.

In the normal case, omit the gate ID and let Transist decide the currently blocked gate from the run lineage. Provide the gate ID only when you need the extra assertion.

## Why use Gates instead of just pausing?

In many systems, a "pause" is invisible. You don't know who approved it, why they approved it, or what they were looking at.

In Transist, a Gate provides **Accountability**:

- **Who:** The identity of the operator who made the decision is recorded on both the active gate record and the append-only history record.
- **When:** The timestamp of the decision is saved.
- **Why:** Operators can provide a reason, such as "The PR description needs a security note."
- **What:** The run record points reviewers to the artifacts and evidence produced before the gate.

This turns a simple "approval step" into a sturdy, auditable piece of your operational history.

## Other gate shapes

### Review exact document revisions

Use `with.review_documents: [notes]` on a manual gate, then
`{$document: notes, access: read, reviewed_by: review_step}` on its consumer.
`review_step` is the gate's step ID in the **same recipe**, not its optional
`gate_id`. Modules share root documents but require their own lexical gates.
The review surface shows names, byte/file counts, exact revision IDs/digests and
opaque content links. Decisions persist that request in history. Restarting while
waiting retains the same review content; a later revision blocks consumption until
another approval, including identical bytes in a different revision. No consumer
probe or invocation starts with a stale/missing/rejected approval. This binds only
managed documents, not arbitrary workspace mutations. See the
[authoring contract](../../reference/workflow-schema.md#document-review-gates).

Manual gates are implemented today. Automated, policy, quality, security, and cost-threshold gates are design directions or policy patterns unless a specific workflow implements them with current steps and plugins.

Examples include requiring tests before success, human review before opening a PR, or approval before an expensive model call. Document those as workflow patterns, not as built-in magic.

---

### Related

- [Inspecting Runs, Artifacts, Evidence, and Events Guide](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
- [CLI Reference](../../reference/cli.md)
- [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

- Previous: [Artifacts, Evidence, and Events](02-artifacts-evidence-and-events.md)
- Next: [Resume and Replay](04-resume-and-replay.md)
