---
name: transist-workflow-setup
description: Guide a user through selecting, signing, validating, promoting, policy-enabling, and testing a first or governed Transist workflow. Use after runtime-home setup or when a user asks to install, sign, validate, prepare, test, or configure policy/capabilities for a workflow.
metadata:
  owner: transist
  domain: infra
  triggers: first workflow, workflow install, workflow signing, workflow validation, workflow promotion, workflow policy, workflow capabilities, workflow test
  priority: high
  token_budget: medium
---

# Transist Workflow Setup

Guide a user from a chosen workflow source to an installed, policy-ready, and
optionally tested workflow. Keep workflow bytes, packages, plugins, trust,
policy, inputs, and run state separate: repair only the contract that failed.

Use `transist ...` commands by default. If the user explicitly identifies a
source checkout and no installed CLI is available, use `cargo run -p
transist-cli -- ...`. Never search for a checkout, workflow, public root, or
private key.

## Default Workflow

1. Confirm the effective home and runtime mode before workflow work:

   ```bash
   transist home get
   transist plugins list
   ```

   Reuse the home, mode, CLI paths, and known release/repository root supplied
   by `transist-runtime-setup`. Ask exactly one decision question per turn when
   choosing a workflow, plugin root, trust root, policy application, or test.
   Read-only inspection and validation need no permission prompt. Mark every
   command as `read-only`, `state-changing`, or `uses a private key`.
2. Select the workflow source. Ask whether the user has a particular
   `workflow.yaml` path or wants a bundled example/demo. For a bundled source,
   resolve only beneath a known release or repository root:
   - `examples/quickstart/quickstart.workflow.yaml` is the signed governed
     quickstart fixture.
   - `workflows/demos/getting_started/<demo-name>/workflow.yaml` contains the
     bundled unsigned demos.

   Do not search for a demo root or invent a demo name. Use the catalog in the
   scenarios reference. Ask for required representative inputs rather than
   inventing them.
3. Inspect and validate the authored workflow before installation or private-key
   use. Root workflows declare exact plugin dependencies, one canonical
   workspace, immutable `context`, typed `artifacts`, optional bounded root
   `documents`, action-bound `profiles`, and package-local `modules`; invoke
   steps use `uses` and never
   carry legacy fields or action-level workspace overrides:

   ```bash
   transist workflows check --path <workflow-yaml> --home <home>
   transist workflows check --path <workflow-yaml> --inputs-file <inputs.json> --home <home>
   ```

   The first command validates authored bytes and their full detached asset
   graph. The second binds known inputs to expose action-schema, workspace,
   policy, context, profile, artifact, and module gaps without dispatching.
   `warning` and `not_evaluated` do not prove mutable remote state.
4. Resolve plugin dependencies before workflow delivery. For each missing
   plugin/action, ask for one exact plugin root and disclose these bounds before
   searching: maximum depth `4`, exact `plugin.yaml`, no symlink following, no
   mount crossing, metadata-only results, maximum `50` candidates. Reject `/`,
   the whole user home, credential directories, network shares, mounted volumes,
   cloud-sync roots, and unspecified CWD. Match exact plugin ID/version only.

   Verify each candidate's existing signature and public root before offering
   `transist plugins install --path <plugin-dir>/plugin.yaml --trusted-execution`.
   Re-run validation against the target home after each installation. The agent must never search for private keys.
5. Select the correct trust and delivery path:
   - **Existing signed package:** inspect its sidecar with
     `transist-admin signature inspect <sidecar>`, authenticate the publisher
     relationship, verify with the user-supplied public root, and ask separately
      before `transist trust import <trust-root-file>`. Verification failure: stop
     installation and signing; never re-sign as repair.
   - **New or changed governed workflow:** confirm the user owns the exact bytes
     or has explicit republishing authority. On an authorized administrative
     machine, ask whether the user already has a signing key. If yes, require
     its exact private-key file path and request fresh consent immediately
     before each private-key command. If no, offer generation only on that
     authorized administrative machine.
     Prefer package signing, which captures the canonical workflow asset graph:

     ```bash
     transist-admin workflow package <workflow-yaml> \
       --version <version> \
      --private-key-file <private-key-file> \
       --output <package-directory>
     transist-admin workflow verify <package-directory>/workflow.yaml \
       --signature <package-directory>/transist.signature.v1 \
      --trust-root-file <trust-root-file>
     ```

     The runtime never receives a private key. Keep it outside the runtime
     home, never print or read it, and use `0600` on Unix.
     Maintained catalog flows/modules and exact plugin pins stay `0.1.0`;
     schemas remain `v1` and runtime/admin crates use `0.1.2`. Changed same-version
     bytes need a newly reviewed signature/bundle and a fresh target home.
     Preserve the preceding home/bundle for pinned runs and rollback; never
     overwrite installed payloads or remove pins/runs to resolve collisions.
     Adopt the current authoring/persistence contract in a fresh home;
     no legacy reader, version alias or in-place migration is provided. Follow
     `docs/public/reference/compatibility-and-stability.md` for adoption and rollback.
   - **Standalone temporary evaluation:** unsigned direct installation is
     permitted only in the selected standalone home. Never use it as a shortcut
     for governed delivery.
6. Install every new or changed governed package after reviewing it. Installation
   verifies signature/trust and persists immutable package bytes, but accepts no
   inputs and does not evaluate readiness, create policy, or create a run:

   ```bash
   transist workflows install --package <package-directory> --home <home>
   ```

   Package-local modules have no independent policy identity. They inherit the
   root workspace and network ceilings lexically and may only narrow them. If
   readiness reports an authority mismatch, do not write a broader policy
   fragment manually. Check the installed workflow with actual inputs. If the
   only blockers are missing grants, show the authorization destination and ask
   whether to apply that exact fragment, then recheck:

   ```bash
   transist workflows check <id>@<version> \
     --inputs-file <inputs.json> --home <home>
   transist workflows authorize <id>@<version> \
     --inputs-file <inputs.json> --home <home>
   transist workflows check <id>@<version> \
     --inputs-file <inputs.json> --home <home>
   ```

   Both commands accept repeatable `--input key=value` and `--inputs-file`.
   Authorize the same prospective invocation that was checked; inline values
   override same-named file values when both forms are present.

   By default, `check` and blocked `authorize` group identical actionable
   findings and separate input/action-schema errors from pending
   authority. If both occur, fix the input and recheck before reviewing
   grants; an action schema's `/issue_key` path is not proof of the workflow
   input name. Read the binding. In scripts use `--format json` and
   `diagnostics.groups.<category>.check_indices` to navigate full `checks[]`;
   `diagnostics.authorization.blocking_codes` names non-policy blockers.
   Default and piped output is concise; JSON parsers must use `--format json`.
   `authorize --dry-run --format json` exposes the plan without writing it.

   Ordinary installed `check` and authorization validate without plugin probes.
   Passing required validation leaves readiness `not_ready`; inspect
   `evaluation_mode` and `validation_status`, not `is_ready`, for this scope.
   Warnings and incomplete validation block success. Already-authorized calls
   perform no probes or writes. Grant application reevaluates current
   inputs/resources/policy and compares the exact reviewed fragment/destination;
   drift aborts without writes and requires another review. Launch review uses
   fast validation; preparation still checks live readiness before run birth.

   `check --live` probes plugins; `check --output readiness.json` selects live
   evaluation and writes full canonical evidence while stdout
   stays concise; both use one evaluation. Add `--format json` for full stdout.

   ```bash
   transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
   transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
   ```

   `authorize` never overwrites an existing policy file, edits `config.yaml`,
   imports trust, or starts execution. Use `--policy-output <path>` only for an approved unused `.yaml` location
   below `<home>/policy` that configured `policy.sources` already loads.
7. For standalone workflows, install only after authored and bound validation
   succeeds. Generate readiness evidence before creating a run:

    ```bash
    transist workflows install --path <workflow-yaml> --home <home>
    transist workflows check <id>@<version> \
      --inputs-file <inputs.json> --output readiness.json --home <home>
    transist workflows prepare <id>@<version> --run-ref <run-id> \
      --inputs-file <inputs.json> --readiness-file readiness.json --home <home>
   ```

    Trusted installation in every mode uses `--package`. A direct source path is
    unsigned standalone/development authoring only; it is not a shortcut for a
    governed delivery.
8. A failed readiness or pre-dispatch check returns `run_created: false` and
   creates no run, locator, event, or artifact. After preparation creates a
   run, inspect persisted preflight and execution diagnostics even when no
   action dispatches:

   ```bash
   transist runs show <run-id> --failed --home <home>
   transist runs preflight <run-id> --trusted-execution --home <home>
   transist runs capabilities <run-id> --format yaml --home <home>
   ```

    If readiness reports secret availability or delivery denial, inspect the
    effective `remote_secret_delivery.process` setting. Show the minimal config
    change and its runtime-wide effect, then re-run readiness. Do not infer or
    persist a secret value.

   A successful preparation is not a test execution. Ask whether to run the
   explicit test. If approved, start only the prepared run and inspect it:

   ```bash
   transist runs start <run-id> --trusted-execution --home <home>
   transist runs tail <run-id> --limit 120 --home <home>
   transist runs show <run-id> --home <home>
   ```

## Failure Triage

| Failure | Repair | Re-sign? |
| --- | --- | --- |
| Authored YAML, nested asset, module, or action schema | Edit, validate, then package again under the repository version policy; use a fresh home for changed same-version bytes. | Yes, new package. |
| Missing input or invalid representative value | Correct the input and revalidate. | No. Use a fresh run if inputs change. |
| Missing plugin/action or public root | Repair the runtime prerequisite and repeat validation. | No. |
| Signature verification failure | Stop; preserve paths and diagnose publisher/trust mismatch. | Never re-sign as repair. |
| Policy/capability-only failure with authorization proposal | Review and apply only the exact emitted fragment, then recheck. | No. |
| Existing policy file or unloaded policy destination | Choose an approved unused loaded destination. Never overwrite or edit config automatically. | No. |
| Readiness, secret availability, prerequisite, policy, or plugin-health failure | Fix the exact reported condition and rerun readiness; no run was created. | No, unless workflow bytes change. |
| External dispatch failure | Inspect the durable run and fix that external/runtime condition. | No, unless workflow bytes change. |
| Changed workflow bytes, detached assets, or version | Create a newly reviewed package and fresh run; use a fresh home for changed same-version bytes. | Yes for governed delivery. |

## Hard Rules

- Before execution, explain workflow-selected working storage separately from
  the effective home. Maintained delivery flows use bounded root managed
  documents for operational intermediates outside Git, not ticket-keyed
  `.transist/` handoffs. Process actions receive private read/write/update copies;
  successful executions publish immutable revisions. Resume keeps committed
  revisions; replay initializes a fresh namespace and reruns producers. Source
  and OpenSpec deliverables stay in the workspace. Other workflows may explicitly
  use workspace files; those are not automatically captured or rolled back.
- Inspect typed `steps.<id>.out.<field>` dependencies and named module exports.
  `deferred_to_dispatch` findings are pending data-dependent checks, not a pass;
  static authority and secret checks still apply before birth. Exact document
  review uses `review_documents` plus read-only `reviewed_by` naming the gate
  step ID in the same lexical recipe, not `gate_id`. A new revision needs review.
- Never treat validation as execution authority. Preparation and dispatch still
  enforce policy, trust, secrets, and live external conditions.
- A workspace declaration requests exact authority but does not grant it. Use
  its canonical `workspace.path`, never CWD, source tree, a worktree, or a
  parent directory as a substitute.
- Preserve generated version-pinned `plugin_grants`, exact `capabilities`, and
  `workflow_grants`. Never add broad permissions, wildcards, guessed resources,
  or source-derived grants.
- `source://` and `prompt_file` assets must be regular contained files. Never
  use symlinks, `..`, absolute paths, or secrets.
- Follow `docs/public/reference/path-and-resource-semantics.md` for cross-step
  files. Do not use `run://` or `runtime://temp` as durable data channels.
- Do not resume, retry, or replay to adopt changed authored bytes. A run is
  pinned to its recipe, inputs, workspace, and assets; publish a newly reviewed
  bundle under the repository version policy and create a fresh run.
- Never auto-start a workflow. Test execution, manual-gate decisions, cleanup,
  retry, resume, and replay require a separate user request.
- Verification failure: stop installation and signing. Preserve artifact and
  public-root paths for diagnosis; never generate a replacement signer or
  re-sign to work around it.

## Source Priority

Use facts in this order when guidance conflicts:

1. Actual checked-out CLI/compiler and installed exact action schemas/bindings;
   persisted recipe/history for an existing run.
2. `docs/public/reference/cli.md`
3. `docs/public/reference/workflow-schema.md`
4. `docs/public/reference/path-and-resource-semantics.md`
5. `docs/public/reference/plugin-manifest.md`
6. `docs/public/guides/trust/trusted-execution-walkthrough.md`
7. `transist-admin/README.md`
8. `crates/transist-cli` and `src/application/workflows`

## References

- [Workflow setup lifecycle](references/workflow-setup-lifecycle.md)
- [Workflow setup scenarios](references/workflow-setup-scenarios.md)
