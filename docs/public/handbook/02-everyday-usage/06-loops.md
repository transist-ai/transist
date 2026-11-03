# Loops

Loops allow you to repeat a sequence of steps until a certain condition is met. This is perfect for tasks like "try this until it passes" or "review this draft until it's approved."

## Defining a Loop

Like branching, loops are attached to a `control.noop` step.

Example:

```yaml
- id: fix_it_loop
  control: control.noop
  loop:
    body:
      - ref: run_test
      - ref: fix_code
    until: "steps.run_test.out.status == 'passed'"
    max_iters: 5
    max_duration: 10m
```

### Key Fields

* **`body`**: The list of steps to repeat.
* **`until` / `while`**: The condition that decides when to stop.
* **`max_iters`**: (Required) The maximum number of times to loop. Transist forces you to set a limit to prevent "infinite loops" that waste time and resources.
* **`max_duration`**: (Optional) A time limit for the entire loop (e.g., `30m`).

## Why Loop Limits Matter

Transist is designed for deterministic workflow execution. We believe that "looping forever" is almost never what you actually want. By requiring a `max_iters`, we force you to decide what should happen if a task simply won't finish.

## Managing "Memory" in Loops

A common mistake is assuming that an AI step in a loop will automatically remember what happened in the previous iteration. **In Transist, every iteration is fresh.**

Manage retained content explicitly with a bounded root document:

```yaml
documents:
  draft: {kind: file, max_bytes: 65536, initial: ''}
```

A process action accepting a mutable absolute file path outside the workspace
can use `{$document: draft, access: update}`. Each iteration receives a private
copy of the latest committed revision; successful execution publishes a new
immutable revision, while failure publishes none. HTTP/MCP document paths are
unsupported. Review a changed revision again before a protected read.

Use direct `{{steps.a.out.field}}` references for values produced in the current
iteration. They cannot fall back to an earlier iteration's output. Keep explicit
counters/accumulators in `set`/`vars`; managed documents retain intermediate files
across iterations and resume. Replay starts a fresh document namespace. Keep
source/OpenSpec deliverables in the workspace. `run://` scratch disappears after
each invocation and cannot retain loop data. See
[Path and Resource Semantics](../../reference/path-and-resource-semantics.md).

## Loops in the Run Record

Loops are inspectable. In your run folder, each loop iteration has execution-scoped records, so operators can compare what changed between attempts.

---

### Related

* [Authoring Patterns](08-authoring-patterns.md)
* [Workflow Patterns Cookbook](../../guides/authoring/workflow-patterns-cookbook.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Branching (If/Else)](05-guards-and-branching.md)
* Next: [Subflows](07-subflows.md)
