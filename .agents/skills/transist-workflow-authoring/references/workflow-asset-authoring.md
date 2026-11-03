# Assets, explicit delivery and recovery

## Assets and intermediate data

`source://` and literal `prompt_file` paths resolve from the declaring flow/module.
Use contained regular files, no parent traversal or symlinks. A module helper
`modules/helpers/check.py` is referenced as `source://helpers/check.py` by a flow
under `modules/`. Inspect the selected action's schema and bindings before using
`prompt_file` or a document path; neither is a universal action field. Start with
the bundled `plugins/shell/plugin.yaml` and its selected schemas for local process
actions. Model and publication contracts are not bundled: report them as missing
unless explicitly supplied, without searching for a sibling catalog checkout.

Use root managed documents for intermediate files/trees, not workspace temp files
or `repo://` conventions. Process actions receive invocation-private copies.
Successful records publish immutable revisions atomically; resume keeps them and
replay has a fresh namespace. Source/OpenSpec deliverables stay in the workspace.
Document review binds exact revisions, not workspace mutations.

`runtime://temp` is a plugin-manifest environment declaration. Runtime forces
private `TMPDIR`; helpers using BSD `mktemp` need a template under it. Invocation
scratch is ephemeral, not a cross-step channel or a secret store. Do not override
it with a host-global directory. Explicit workspace cleanup must preserve active
runs and its authored success/failure conditions.

## Delivery only when requested

After source validation and deterministic explanation, obtain the separately
requested delivery/authority/execution decisions. Commands below are lifecycle
reference, not an instruction to perform them during authoring.

For explicitly requested standalone fixture installation:

```bash
transist workflows install --path workflow.yaml --home "$TRANSIST_HOME"
```

For governed delivery, private-key operations require fresh explicit authorization
on an administrative machine. The runtime never receives private keys:

```bash
transist-admin workflow package workflow.yaml --version 0.1.0 \
  --private-key-file <approved-private-key-file> --output <new-package-directory>
transist workflows install --package <new-package-directory> --home "$TRANSIST_HOME"
```

Installation persists immutable code, not inputs, grants, trust roots, readiness
or a run. Changed `0.1.0` bytes need a new signature and fresh home; never replace
an installed payload. Preserve the preceding home and bundle for rollback.

Inspect exact missing authority separately, applying only a reviewed plan when
requested. `authorize` writes policy; source checks/explanation do not. Never
mask non-policy failures with broader grants. `--policy-output` must be an unused
approved YAML path already covered by `policy.sources`.

```bash
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
transist workflows check demo@0.1.0 --inputs-file inputs.json \
  --output readiness.json --home "$TRANSIST_HOME"
```

Ordinary installed `check` is fast; `--live` or `--output` selects live checking
and may probe plugins. `deferred_to_dispatch` remains deferred, not a static pass;
authority/static data must pass and runtime-dependent checks run before dispatch.
Missing secrets or mutable external state are not established by source validation.
After explicit execution approval and ready evidence:

```bash
transist workflows prepare demo@0.1.0 --run-ref <new-run> \
  --inputs-file inputs.json --readiness-file readiness.json --home "$TRANSIST_HOME"
transist runs start <new-run> --trusted-execution --home "$TRANSIST_HOME"
```

`transist workflows run` is the combined installed birth/dispatch command, not a
validation substitute. Direct gate decisions can immediately resume execution;
interactive confirmation is a UI feature, not a runtime policy boundary.

## Repair and recovery

Runs pin recipe, inputs, workspace and captured asset bytes. Source corrections
require a reviewed new bundle and fresh run/home under the fixed-version policy.
Resume/retry/replay do not adopt authored corrections. Missing input bindings need
new input validation, not re-signing unchanged code; changed run inputs require a
fresh run. Missing plugins/trust/policy/external prerequisites are separate from
authored-byte failures. Never edit persisted requests, decisions or history.

Inspect with `transist runs show`, `runs preflight` and `runs tail`. Follow opaque
document content links from inspection rather than constructing durable home
paths. Never auto-approve a real gate to reach a repaired step.

Canonical references: `docs/public/reference/path-and-resource-semantics.md`,
`workflow-schema.md`, `cli.md`, and `compatibility-and-stability.md`.
