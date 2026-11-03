# Artifacts, Evidence, and Events

To provide a complete and trustworthy picture of a workflow run, Transist records distinct kinds of information. Our core philosophy is **Evidence Over Trust**: AI-generated results should not be trusted because they look plausible, but because they are supported by reviewable proof.

## 1. Evidence (The Proof)

Evidence is the most critical component for governance and debugging. It is the supporting material that explains exactly how a step executed.

- **Purpose**: To prove how the output was produced and provide a trail for audit and review.
- **Examples**: raw API responses, tool call records, decision records, execution logs, token usage, and human approval records.
- **Philosophy**: Evidence should make the result reviewable even by someone who wasn't present during the run.

## 2. Artifacts (The Results)

Runtime-managed artifacts are accepted files or structured outputs reported by
adapters and persisted for a specific execution. Not every file a workflow writes
to its workspace is automatically a runtime-managed artifact.

- **Purpose**: To hold the operator-meaningful output of the workflow.
- **Examples**: a generated `README.md`, a test report JSON, a compiled binary, or a diff file.
- **Relationship**: Every artifact should be traceable back to the evidence that supports it.

## 3. Events (The Timeline)

Events are the chronological stream of activity observed by the runtime kernel.

- **Purpose**: To provide a reliable timeline of the run's lifecycle and state transitions.
- **Examples**: `run_started`, `step_started`, `manual_gate_blocked`, `gate_approved`, `run_succeeded`.

Adapters do not write directly into the durable run tree. They receive private
per-invocation staging directories. The kernel scans reported paths and bytes,
redacts accepted UTF-8 files, rejects symlinks, escapes, missing/non-regular
files, secret-bearing names, and binary files containing known secret variants,
then atomically promotes accepted files into the execution-scoped tree.

## Summary Table

| Concept | Question it Answers | Primary Audience |
| :--- | :--- | :--- |
| **Artifact** | What accepted output was persisted? | End users, operators, reviewers |
| **Evidence** | How was it produced? | Reviewers, auditors, developers |
| **Event** | When did it happen? | Monitoring, troubleshooting |

## Managed Intermediate Documents

Maintained delivery workflows share context, drafts, reports and ledgers through
bounded root managed documents outside Git. Process actions receive private
read/write/update copies; successful executions publish exact immutable revisions
atomically. These are distinct from accepted artifact/evidence snapshots: known
secret-bearing document bytes are rejected, not redacted. Inspection identifies
the producing execution, revision ID, digest and size, with content links for
exact review. Resume retains revisions; replay starts a fresh namespace.

Keep intended source/OpenSpec deliverables in the workspace. Other workflows may
explicitly share workspace files, but those remain mutable host state without
automatic capture, redaction or rollback. Invocation-private `run://` scratch is
not a handoff channel. See [Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

## Why the Distinction Matters

By separating these categories, Transist ensures that execution is **operationally usable**:

- **Reviewability**: You can inspect the final artifact without digging through raw logs, but the logs (evidence) are always there if you need to verify the result.
- **Recovery**: Because events and evidence are persisted for every attempt, you can understand exactly why a retry happened and what changed between attempts.
- **Attribution**: Every usage signal or cost metric is attributed to the specific step and evidence that generated it.

When you need a compact report that summarizes trusted workflow/plugin identity, policy outcome, provider/model metadata, and denied-run details in one place, use `transist runs evidence export <run-id>`. It reads the persisted run root and does not mutate execution state.

---

### Related

- [Inspecting Runs](01-inspecting-runs.md)
- [Determinism and Auditability](../04-trust-and-runtime-behavior/01-determinism-and-auditability.md)
- [Cost and Usage Reference](../../reference/metrics.md)

**Navigation:**

- Previous: [Inspecting Runs](01-inspecting-runs.md)
- Next: [Manual Gates](03-manual-gates.md)
