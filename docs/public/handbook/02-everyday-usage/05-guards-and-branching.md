# Branching (If/Else)

Sometimes your workflow needs to take different paths depending on the situation. Transist supports this through **Guards** and **Branching**.

## The `if` Block

Branching is always attached to a `control.noop` step. It allows you to check a condition (a "guard") and then decide which steps to run next.

Example:

```yaml
- id: route_logic
  control: control.noop
  if:
    guard: "inputs.dry_run == true"
    then:
      - ref: show_preview
    else:
      - ref: actually_publish
```

### How it Works

* **`guard`**: A simple expression that evaluates to true or false. You can use your workflow `inputs` here.
* **`then`**: A list of steps to run if the guard is true.
* **`else`**: A list of steps to run if the guard is false.

## Rules for Branching

1. **Must use a Noop**: You can't put an `if` block on an Invoke step. It must be on a `control.noop`.
2. **Clear References**: The steps listed in `then` and `else` must be valid step IDs defined elsewhere in your workflow.
3. **No Hidden Logic**: While you *could* put this logic inside a shell script, we recommend using Transist branches. This makes your workflow's "decision tree" visible to anyone who looks at the file or the Run record.

## When to use Branching

* **Dry Runs**: Use a `dry_run` input to skip dangerous steps and just show a preview.
* **Operator Input**: Use a branch after a **Manual Gate** to decide the next path based on the human's decision.
* **Environment Checks**: Run different steps based on an `environment` input (e.g., `prod` vs `staging`).

---

### Related

* [Workflow Patterns Cookbook](../../guides/authoring/workflow-patterns-cookbook.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Managing Flow (Control Steps)](04-control-steps.md)
* Next: [Loops](06-loops.md)
