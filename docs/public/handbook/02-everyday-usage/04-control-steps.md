# Managing Flow (Control Steps)

While **Invoke** steps do the work, **Control** steps manage *how* that work happens. They allow Transist to handle logic like waiting for humans, looping over items, or stopping a run early.

## Common Control Primitives

There are three main types of control steps:

1. **`control.noop`**: Does nothing on its own, but acts as a container for logic like `if` and `loop`.
2. **`control.finish`**: Explicitly stops the workflow.
3. **`control.gate.manual`**: Pauses the workflow and waits for a human decision.

## Manual Gates (Waiting for Humans)

The most powerful control step is the **Manual Gate**. It allows you to pause a workflow at a critical point (like a security review or a deployment approval).

```yaml
- id: security_review
  control: control.gate.manual
  with:
    gate_id: prod-deployment
    operator_guidance:
      conditions:
        items:
          - "Check the generated report for any security warnings."
      decisions:
        approve: "Everything looks good, proceed to deploy."
        request_changes: "Something is wrong, go back to the beginning."
        reject: "Stop the run immediately."
      inspect:
        label: "Security Report"
        paths:
          - "reports/security_check.md"
```

### How it works at runtime

1. The workflow runs until it reaches the gate.
2. The run becomes **Blocked**.
3. An operator (a human) uses the CLI to `approve`, `reject`, or `request-changes`, recording both identity and reason.
4. The workflow records who made the decision and then **Resumes**.

Typical commands:

- `transist runs gate approve <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "Reviewed output and approved"`
- `transist runs gate reject <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "Stop the run"`
- `transist runs gate request-changes <run-id> [gate-id] --actor "Reviewer <reviewer@example.com>" --reason "Needs another pass"`

## The Role of Noops

At first glance, a `noop` step might seem useless. However, in Transist, it's the required "anchor" for more complex logic. If you want to use an `if` condition or a `loop`, you must attach it to a `control.noop` step.

## Accountability

Control steps, especially Gates, are where human responsibility is captured in the **Run Record**. Every decision—who made it, when they made it, and what they decided—is stored forever as part of the run's history.

---

### Related

- [Workflow Patterns Cookbook](../../guides/authoring/workflow-patterns-cookbook.md)
- [Inspecting Runs, Artifacts, Evidence, and Events](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
- [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

- Previous: [Calling Tools (Invoke Steps)](03-invoke-steps.md)
- Next: [Guards and Branching](05-guards-and-branching.md)
