# Reference: Gate Decision Record

A **Gate Decision Record** is a JSON file that stores the result of a manual approval or rejection. It is the bridge between a human decision-maker and the Transist execution engine.

## Purpose

- **Asynchronous Approvals:** It allows someone to approve a workflow even if the original terminal that started it is no longer open.
- **Auditability:** Every manual decision is permanently recorded as part of the run's history.
- **Resumption:** The Transist kernel reads this file to know when it should unblock a paused workflow and continue execution.

## File Location

Decision files are stored in the run's directory:
`<home>/runs/run_<run-id>/gates/<gate-id>.json`

The `<gate-id>` is the exact validated identifier from the workflow's
`with.gate_id` (or the step id when none is declared). It must satisfy the
identifier rule (ASCII alphanumerics plus `.`, `_`, and `-` after a mandatory
alphanumeric first character); no lossy slug normalization is applied, so
`review-a`, `review_a`, and `review.a` are distinct gates. When the kernel
loads a decision it verifies that the stored `run_id` and `gate_id` match the
request and fails closed on any mismatch.

## Wire Format

The file is a simple JSON object:

```json
{
  "schema": "transist.gate_decision.v1",
  "run_id": "my-run-123",
  "gate_id": "security-review",
  "step_id": "await_review",
  "decision": "approved",
  "actor": "Reviewer <reviewer@example.com>",
  "actor_source": "flag",
  "reason": "Security scan passed and docs are updated.",
  "decided_at_ms": 1730800000000
}
```

## Fields

| Field | Description |
| :--- | :--- |
| **`schema`** | Always `transist.gate_decision.v1`. |
| **`run_id`** | The ID of the workflow run. |
| **`gate_id`** | The unique ID of the manual gate (as defined in the workflow YAML). |
| **`step_id`** | The ID of the step that created the gate. |
| **`decision`** | One of `approved`, `rejected`, or `request_changes`. |
| **`actor`** | (Optional for legacy reads, required for new CLI decisions) The recorded operator identity. |
| **`actor_source`** | (Optional) Where the actor came from: `flag`, `env`, `config`, `user-settings`, `prompt`, or a future external identity source. |
| **`reason`** | (Optional for legacy reads, required for new CLI decisions) A human-readable rationale for the decision. |
| **`decided_at_ms`** | (Optional for legacy reads, populated for new CLI decisions) A Unix timestamp of when the decision was made. |

---

## How Decisions Are Made

### Recommended Path (The CLI)

The easiest and safest way to record a decision is using the `transist` CLI:

```bash
transist runs gate approve my-run-123 security-review \
  --actor "Reviewer <reviewer@example.com>" \
  --reason "Security scan passed and docs are updated."
```

The CLI handles all the technical details, such as acquiring a lease and notifying the background runner that it should resume.

### Advanced Path (Custom Tools)

Approved internal tools can integrate with these files directly when that is part of a supported deployment design. Direct writes must avoid race conditions where two processes try to update the same run at once. **Use the CLI commands unless direct filesystem integration is explicitly supported for your deployment.**

---

### Versioned Public Surface

The `transist.gate_decision.v1` schema is a versioned public surface. `v1`
is a format version, not a SemVer 1.0 stability promise. It is protected in
the current minor line: adding a new type of decision (other than `approved`,
`rejected`, or `request_changes`) would require a version bump to `v2` and
migration guidance.
