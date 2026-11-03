# Guide: Managing Secrets and Runtime Policy

This guide is for workflow authors and operators who need a hands-on look at how Transist handles secrets, how policy controls their delivery, and what raw secret data is redacted from Transist-managed persisted records.

> **Prerequisite:** Before starting, you should be comfortable with the concepts in the [Advanced Workflow Authoring guide](../authoring/advanced-workflow-authoring.md).
>
> **Tools:** `jq`, used in the verification steps below to extract the effective home path from `transist home get`.
>
> **Conceptual Background:** For a deeper dive into the "why" behind these features, see the [Secrets and Policy handbook page](../../handbook/04-trust-and-runtime-behavior/02-secrets-and-policy.md).

## The Goal

By the end of this guide, you will have a concrete understanding of:

- How to reference a secret in a workflow.
- How the Transist kernel resolves and delivers secrets.
- How different adapters (`process`, `http`, `mcp`) handle secrets by default and how policy can deny them.
- What secret-related data is never persisted to disk.
- How local allowlists and child-env isolation change what a step can access before dispatch.
- How declared workspaces generate exact filesystem requests distinct from
  workflow source and CLI CWD.
- How `--local-only` interacts with declared plugin `provider_kind` and external-transfer evidence.

## How Transist Handles Secrets

Let's walk through a typical secret-handling scenario.

### 1. Define a Secret

First, make a secret available to your environment. The simplest way is to export it as an environment variable.

```bash
export MY_API_TOKEN="secret-value-12345"
```

*Note: In a real-world scenario, this secret would come from a secure backend like HashiCorp Vault, not an environment variable.*

### 2. Reference the Secret in a Workflow

In your workflow's invoke step, use `with.secret_refs` to declare that your step needs this secret.

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: call_api_with_token
    uses: process:shell/exec_with_secret_env
    with:
      command: ./scripts/my_api_script.sh
    # This tells Transist the script needs the secret named "MY_API_TOKEN".
      secret_refs: ["MY_API_TOKEN"]
```

### 3. Run the Workflow

When you run a workflow that uses this step, here’s what happens behind the scenes:

1. **Preflight Check:** Before running any steps, Transist's "preflight" check verifies that it can resolve all the secrets you've referenced. It also checks if the policy allows delivering them to the specified adapter. If anything is wrong, the run fails immediately with a `remote_secret_policy` or secret-resolution failure, saving you time.
2. **Resolution:** The kernel asks the configured secret provider (by default, the environment) for the value of `MY_API_TOKEN`.
3. **In-Memory Delivery:** The kernel passes the resolved secret value (`secret-value-12345`) directly to the adapter *in memory*.
4. **Redaction:** Before any runtime-owned record becomes durable, the kernel replaces every exact raw secret substring and its padded or unpadded standard and URL-safe base64 forms. Plugins write artifacts and evidence to private per-invocation staging directories; the kernel scans paths and bytes before promotion. Valid UTF-8 is redacted, while binary files containing a known variant are rejected.

Workflow-owned prompt files and `source://` helpers are different from secret
references: fresh run birth may retain their raw bytes in private
`workflow_assets/` storage. Never place credentials, tokens, or other secret
material in those files. Snapshot-backed runs dispatch their captured bytes;
legacy records retain authored-source behavior.

### 4. Verify the Redaction

After the run completes, you can check that this raw secret value was not persisted by Transist-managed run data by searching the effective Transist home directory (resolve it with `transist home get`). This guarantee covers runtime-known values and listed encodings, not arbitrary plugin transformations or external exfiltration.

```bash
# This command should produce no output.
grep -r "secret-value-12345" "$(transist home get | jq -r .effective_home)"
```

## Policy and Adapter Differences

Not all adapters are treated equally when it comes to secrets. Transist uses a deny-by-default policy for remote secret delivery.

- `process`: **Denied** in new governed homes. Explicit existing standalone homes may retain their configured value.
- `http`: **Denied** by default. You must explicitly enable secret delivery for this adapter.
- `mcp`: **Denied** by default. You must also enable secret delivery for this adapter.

This policy lives in the `<home>/config.yaml` file of the effective Transist home
(resolve it with `transist home get`). To allow an adapter to receive secrets,
edit the explicit policy and capability grants in the file:

```yaml
policy:
  profile: permissive
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
```

With `process: false`, any `process` step or cleanup step that declares `with.secret_refs` is denied before adapter dispatch. This is useful for locked-down operator environments that want to prove no local plugin receives resolved secret material.

For governed execution, secret delivery is one part of a broader capability
decision. Use version-pinned `policy.allow.plugin_grants` with explicit actions
for plugin load, action invocation, and the matching adapter/provider. Keep
secrets, filesystem, network, and approval resources under exact
`policy.allow.capabilities`. Missing or partial grants fail with
`capability_not_granted` before secret lookup or adapter dispatch.

```yaml
policy:
  allow:
    plugin_grants:
      - plugin: git.cli@0.1.0
        actions: [checkout_branch, commit_worktree, push_current_branch]
      - plugin: github.cli@0.1.0
        actions: [pr_create, pr_view]
    capabilities:
      - capability: network
        resource: https://api.github.com:443
        operations: [connect, redirect]
```

Do not duplicate matching `adapter`, `provider`, `plugin`, or `action`
capabilities. A matching plugin grant covers those invocation-specific
capabilities; network, secrets, filesystem, and approvals remain explicit.

A declared `workspace.path` likewise requests, but does not grant, filesystem
authority. The runtime resolves the canonical `repo_path` at run birth and
generates its exact resource and operations. `full` can request mutations;
`read_only` requests only list/read. Do not grant the workflow source, a
worktree, or a guessed parent as a substitute. Secondary repository context must
be an exact existing input-bound `read_allow_paths` entry and remains read-only.

For reusable operator policy, define named `policy.grant_sets` and bind them to
an exact `policy.workflow_grants[].workflow` reference. The binding is resolved
from the pinned workflow reference, never from `inputs.yaml`; inputs may change
the authority requested by a run but cannot select or widen a grant set.

### Policy fragments

Large homes can split named grant sets and exact workflow bindings into a
directory of workflow-family fragments:

```yaml
# <home>/config.yaml
policy:
  sources:
    - workflows
```

This loads files such as these relative to `<home>/policy`:

```text
policy/
└── workflows/
    ├── cursor-software-engineer.yaml
    ├── examples.yaml
    └── hardware-backend-activity.yaml
```

Each source is either one `.yaml` file or a directory of immediate `.yaml`
files. Directory files load in sorted order; nested directories and recursive
fragment imports are not supported. Each fragment is standalone, declares
`schema: transist.policy_fragment.v1`, and may contain only `grant_sets` and
`workflow_grants`. Keep operating mode, profile, secret delivery, global
allowlists, and child environment settings in the root config. Sources, path
traversal, symlinks, duplicate grant-set names, duplicate workflow bindings,
unknown fields, and unknown schemas fail closed.

When `policy.allow.workflows` is configured, it remains the run-start
allowlist. Every fragment-bound workflow that should run must also appear
there; a `workflow_grants` binding selects capabilities but does not authorize
the workflow to start.

Run `transist policy validate` before use and `transist policy show --effective` to inspect the flattened policy. Inline grant sets and bindings remain supported.

## Best Practices for Safe Secret Management

- **Always use `with.secret_refs`:** Never hard-code secrets in your workflow files.
- **Least Privilege:** Only enable remote secret delivery (`http`, `mcp`) when absolutely necessary and only for the specific plugins that require it.
- **Be Mindful of Transformation:** If a tool modifies a secret (e.g., uses it to generate a temporary token), Transist won't automatically recognize the *new* value as a secret. Treat derived credentials with the same care as the original.
- **Treat Logs as Public:** Assume that anything written to stdout, stderr, or run artifacts is not secret and could be viewed by anyone with access to the run.

## Local Allowlists and Child Environments

For stricter local operation, Transist can deny steps before dispatch unless they match your configured allowlists:

```yaml
policy:
  profile: permissive
  allow:
    workflows: ["demo_10_secret_env@0.1.0"]
    adapters: ["process"]
    plugins: ["shell"]
    actions:
      - plugin: shell
        action: exec_with_secret_env
    secret_refs: ["MY_API_TOKEN"]
  child_env:
    inherit: false
    allowlist: ["PATH", "HOME"]
```

Important behavior:

- If `allow.workflows`, `allow.adapters`, `allow.plugins`, `allow.actions`, or `allow.secret_refs` is omitted, that dimension stays permissive.
- If one of those fields is present but empty, that dimension becomes deny-all.
- `allow.secret_refs` is checked before provider lookup.
- When `child_env.inherit: false`, process and MCP children lose unrelated parent env vars by default.

## Local-Only and External-Transfer Walkthrough

`--local-only` is about declared provider locality, not secret delivery. The runtime checks the target plugin action's manifest `provider_kind`:

- `local`: the step is allowed to dispatch under `--local-only`
- `external`: the step is denied before dispatch under `--local-only`

For HTTP actions, local-only also imposes a host-enforced loopback network
ceiling. The adapter resolves the destination, rejects non-loopback results,
and disables proxies in restricted modes before sending handshake, health,
invoke, redirect, or cancellation requests.

This declaration is independent of `adapter`. A `process` plugin can still be `external` if it shells out to a remote model or API.

For governed runs that do allow an external-provider step to dispatch, the runtime may write additive `transist.external_transfer.v1` evidence under the step's `evidence/` directory. That record includes destination, provider/provider_kind, adapter kind, and a SHA-256 of the JSON request payload sent. In a governed run, `redaction_applied` is always `false`.

For local-only runs, `provenance.json.external_transfer_summary.count` stays `0` because no external-provider step is allowed to dispatch.
