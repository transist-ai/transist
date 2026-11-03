# Organizing with Modules

As your workflows grow, they can become difficult to manage in a single file. **Modules** let one workflow package break logic into smaller reusable pieces.

## Using a Module

A module is a package-local file declared by the root workflow and called from a module step.

Example:

```yaml
modules:
  test_suite:
    path: modules/test_suite.yaml

steps:
  - id: run_tests
    module:
      ref: test_suite
      inputs:
        target_branch: "{{inputs.branch}}"
```

### Key Fields

* **`modules.<name>.path`**: The package-local module file.
* **`module.inputs`**: A mapping that passes data from the parent workflow to the child.
* **`module.rerun_mode`**: (Optional) Controls what happens when the parent re-dispatches the module step. `reuse` (the default) re-enters the existing child run under the same run id; the child's execution history is append-only, so the next dispatch of each child step starts at `N+1` and prior execution records, artifacts, and evidence remain inspectable. `fresh` creates a distinct child run id with fresh counters on each re-entry.

## Why use Modules?

Subflows aren't just for organization; they provide a "hard boundary" between different parts of your system:

1. **Reusability**: Write a "Security Audit" workflow once and use it as a subflow in ten other workflows.
2. **Encapsulation**: The parent workflow doesn't need to know *how* the tests run; it just needs to know that the "run_tests" subflow finished successfully.
3. **Clean History**: In the inspection UI, module calls appear as nested runs. You can look at the high-level parent run or dive into the detailed history of a specific child.

## Package-Local Modules

An authored `subflow` field is no longer supported. A module is captured,
signed, and compiled with its parent and cannot be launched by itself:

```yaml
modules:
  review:
    path: modules/review.yaml

steps:
  - id: review_release
    module:
      ref: review
      inputs:
        release: "{{inputs.release}}"
```

Each module file requires `schema: transist.module.v1` and release metadata;
maintained catalog modules use `version: 0.1.0`:

```yaml
# modules/review.yaml
schema: transist.module.v1
id: review
version: 0.1.0
steps:
  - id: finish
    control: control.finish
```

## Module Rules

* **Explicit Inputs and Exports**: Map parent values explicitly to child inputs. Modules declare named `outputs`; callers consume `{{steps.run_tests.out.<export>}}`, not the child's internal step IDs. Root context and document declarations are inherited lexically.
* **Child Input Binding**: Before the child run is created, Transist binds the mapped values against the child workflow's declared `inputs`. Omitted optional child inputs receive their declared defaults.
* **Invalid Child Inputs Fail The Parent Step**: If the parent passes an undeclared child input, omits a required child input, or produces the wrong child-input type, the parent module step fails before a child run is created.
* **Prepare-Time Nested Checks Are Recursive**: `prepare` and `runs preflight` walk reachable child modules and surface nested child blockers on the root run with parent-prefixed `step_ids` like `plan_loop/generate_plan`.
* **Deferred Data Is Checked Before Dispatch**: A declared `{{steps.<id>.out.<field>}}` dependency that needs runtime data produces `deferred_to_dispatch` findings, not a passed/skipped check. Static policy, authority, secret and prerequisite checks still apply. The full materialized request is validated before consumer probes or effects; missing/skipped/failed or stale iteration outputs fail rather than using an old value.
* **No Mixed Steps**: A step is either a module call or an invocation, never both.
* **No Loops on Modules**: If you need to loop over a module, wrap the module step in a `control.noop` step that has the loop logic.
* **One Workspace**: A child inherits the parent's resolved canonical workspace.
  If it declares `workspace`, it must resolve to the same path. A child may
  narrow `full` to `read_only`, but cannot widen access or use a different
  `repo_path`. Map the identical `repo_path` explicitly into every child.
* **Shared Documents, Local Review**: Modules use root document names directly,
  without forwarding private paths. A protected document read's `reviewed_by`
  names a gate step in that module's lexical recipe; an ancestor's `gate_id` or
  approval cannot authorize it. Exact changed revisions require review again.

---

### Related

* [Advanced Workflow Authoring Guide](../../guides/authoring/advanced-workflow-authoring.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Loops](06-loops.md)
* Next: [Authoring Patterns](08-authoring-patterns.md)
