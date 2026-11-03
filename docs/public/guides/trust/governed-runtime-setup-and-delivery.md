# Guide: Governed Runtime Setup and Delivery

Use this guide to deliver an operator-owned plugin and workflow to a governed
Transist home. It separates trust creation from runtime enforcement and keeps
workflow source, signed package, installed recipe, and run state distinct.

> **Use this for regular controlled delivery.** For a disposable unsigned
> evaluation, use the standalone [Quickstart](../getting-started/quickstart.md).
> Read [Runtime Modes](../../reference/runtime-modes.md) first to understand
> why governed is the default posture and how it differs from standalone and
> development homes.
> For the complete command contracts, use the [CLI Reference](../../reference/cli.md)
> and `transist-admin/README.md`.

## Before you begin

You need:

- a runtime machine with `transist` and an intended runtime home;
- an administrative or build machine with `transist-admin` and the private key;
- an exact plugin directory, workflow root, representative `inputs.json`, and
  public trust-root destination supplied by the operator.

Never copy the private key into the runtime home. Never search for a private
key, overwrite a policy file, or use a broader filesystem or network grant to
work around a failed check.

## 1. Initialize and inspect the governed runtime

On the runtime machine, create a governed home if one does not already exist:

```bash
transist home init --mode governed
transist home get
transist policy validate
transist policy show --effective
```

Governed homes require `policy.profile: allowlist`, trusted plugin/workflow
verification, and exact capability grants. `home init` does not rewrite an
existing `config.yaml`; resolve the effective home and inspect its policy before
changing delivery inputs.

## 2. Create trust on the administrative machine

Generate a key and export its public trust root outside the runtime home:

```bash
transist-admin key generate --name acme \
  --private-key-file ./trust/acme.key
transist-admin key export-public \
  --private-key-file ./trust/acme.key \
  --trust-root-file ./trust/acme.pub
```

Restrict the private key file to its administrator. The exported
`transist.trust_root.v1` file is the only trust material that moves to the
runtime machine.

## 3. Prepare and sign the plugin

Before signing, ensure `plugin.yaml` declares every executable, schema, and
local runtime asset in `package.files`. Changed plugin bytes require a new plugin
version; a version is content-immutable.

```bash
transist-admin plugin sign ./plugins/my-plugin \
  --private-key-file ./trust/acme.key
transist-admin plugin verify ./plugins/my-plugin \
  --signature ./plugins/my-plugin.sig \
  --trust-root-file ./trust/acme.pub
```

Use [Plugin Lifecycle Management](../plugins/plugin-packaging-install-upgrade-removal.md)
and the [Plugin Manifest Reference](../../reference/plugin-manifest.md) for the
complete package contract.

## 4. Validate, package, and verify the workflow

Validate the authored workflow before private-key use:

```bash
transist workflows check --path ./workflow.yaml
```

For governed delivery, create an immutable package. It captures the workflow's
declared modules, prompt files, and `source://` assets, then signs and
self-verifies the captured package.

```bash
transist-admin workflow package ./workflow.yaml --version 0.1.0 \
  --private-key-file ./trust/acme.key \
  --output ./workflow-package-v1
transist-admin workflow verify ./workflow-package-v1/workflow.yaml \
  --signature ./workflow-package-v1/transist.signature.v1 \
  --trust-root-file ./trust/acme.pub
```

Changing workflow YAML, declared modules, prompt files, or `source://` assets
requires a new version and package. Changing only run inputs requires a fresh
run, not a new signature.

## 5. Import trust and install the signed plugin

On the runtime machine, import the public root and install the plugin with
trusted execution:

```bash
transist trust import ./trust/acme.pub
transist trust list
transist plugins install --path ./plugins/my-plugin/plugin.yaml \
  --trusted-execution
```

If trust or signature verification fails, stop. Preserve the artifact and
trust-root paths for diagnosis; do not generate a replacement signer or re-sign
as a repair.

## 6. Install the signed workflow package

Install verifies and persists immutable package bytes. It does not bind inputs,
evaluate readiness, write policy, create a run, or dispatch work. Interactive
use displays the install plan and asks for confirmation.

```bash
transist workflows install --package ./workflow-package-v1
```

## 7. Check readiness, authorize exact authority, prepare, and start a run

Ordinary installed `check` and authorization use probe-free validation; passing
required validation leaves readiness `not_ready`. Warnings and incomplete checks
block success. The `--output` examples below select live readiness and canonical
evidence; `--live` probes without writing evidence. Already-authorized calls
probe and write nothing. Applying reevaluates current inputs/resources/policy
and compares the exact reviewed fragment/destination; drift aborts without
writes and requires another review. Preparation always checks live readiness.

Check the installed reference without resolving secrets or dispatching work:

```bash
transist workflows check <workflow-id>@0.1.0 \
  --inputs-file ./inputs.json \
  --output readiness.json
```

If the report shows only missing authority, review the exact resources,
operations, and policy destination before applying a separate authorization:

```bash
transist workflows authorize <workflow-id>@0.1.0 \
  --inputs-file ./inputs.json
transist workflows check <workflow-id>@0.1.0 \
  --inputs-file ./inputs.json \
  --output readiness.json
```

By default, `check` groups identical failed and incomplete findings with
occurrence counts and sampled paths, including in pipes. A blocked `authorize`
shows the same readiness blockers and writes no grants. If action inputs fail
schema validation alongside missing capabilities, fix the input first; an
action input path such as `/issue_key` is not necessarily the workflow input
name. Inspect the workflow binding rather than guessing a policy grant. Only
policy-only failures lead to a grant plan and confirmation.

For scripts or a searchable complete report, force JSON and select findings
by category or code:

```bash
transist workflows check <workflow-id>@0.1.0 \
  --inputs-file ./inputs.json --format json \
  | jq '.checks as $checks | .diagnostics.groups.inputs.check_indices[]? | $checks[.]'
```

`--format json` also exposes `diagnostics.authorization.eligible`,
`blocking_codes`, and `missing_capabilities`; these are summaries, not
authorization. See the [CLI reference](../../reference/cli.md) for the full
index. Piped and redirected output stays concise; JSON parsers must explicitly
request `--format json`. `--output readiness.json` writes complete canonical
evidence while displaying a concise summary from the same evaluation.

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

`check` and blocked authorization are read-only prospective diagnostics with
`run_created: false`. They never create a diagnostic run. A policy fragment is
created only by authorization, never by installation; it is no-clobber and
remains independently reviewable.

Both commands accept repeatable `--input key=value` arguments and
`--inputs-file`; when combined, inline values override same-named file values.

Prepare an explicit run to perform live preflight checks, then inspect it before
starting it:

```bash
transist workflows prepare <workflow-id>@0.1.0 \
  --run-ref release-v1-run \
  --inputs-file ./inputs.json \
  --readiness-file readiness.json \
  --trusted-execution
transist runs preflight release-v1-run --trusted-execution
transist runs show release-v1-run
```

Start only the prepared run you intend to execute:

```bash
transist runs start release-v1-run --trusted-execution
transist runs tail release-v1-run --limit 120
```

Add `--local-only` only when every invoked action is declared local and the
loopback network ceiling is appropriate. It is a locality control, not a
replacement for trust or policy.

## Repair the failed contract

| Finding | Correct response |
| --- | --- |
| Workflow definition or package asset error | Correct the source, bump the workflow version, package again, and create a fresh run. |
| Plugin payload change | Bump the plugin version, sign it again, install it, then publish a workflow version pinned to it. |
| Missing public root, plugin, or action | Repair the runtime prerequisite and repeat validation or installation. |
| Signature verification failure | Stop and diagnose the signer, package, and trust root. Do not re-sign as a workaround. |
| Invalid bound input and missing grants together | Repair the input or action binding, rerun `workflows check`, and only then review the remaining grants. |
| Missing capability grant | Review and apply only the exact authorization proposal, then rerun `workflows check`. |
| Secret, process, HTTP, MCP, or live prerequisite failure before run birth | Repair the reported condition and rerun `workflows check`; no run was created. |
| Failure after preparation creates a run | Inspect persisted `runs show`, `runs preflight`, and `runs tail` diagnostics before recovery. |

For inspection and recovery, see [Inspecting Runs, Artifacts, Evidence, and Events](../inspection/inspecting-runs-artifacts-evidence-and-events.md).
