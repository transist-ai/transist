# Runtime Modes

Every Transist home declares one `runtime.mode` in `<home>/config.yaml`.
Choose it when creating a home with `transist home init`; the command defaults to
`governed`. A mode selects the runtime's trust and policy posture. It does not
select a workflow, plugin, workspace, or network destination.

```bash
transist home init --mode governed
TRANSIST_HOME=/absolute/path/to/demo-home transist home init --mode standalone
TRANSIST_HOME=/absolute/path/to/dev-home transist home init --mode development
```

`home init` is idempotent and does not rewrite an existing `config.yaml`. Inspect
the active home before lifecycle work:

```bash
transist home get
transist policy validate
transist policy show --effective
```

## Choose a mode

| Mode | Use for | Policy profile | Trust posture | Production use |
| --- | --- | --- | --- | --- |
| `governed` | Regular use and controlled delivery | `allowlist` required | Signed plugins and signed workflow packages must be trusted before install or execution | Yes |
| `standalone` | Explicit temporary local evaluation | `permissive` when created | Unsigned local plugins and workflows may be used; `--trusted-execution` remains available | No |
| `development` | Explicit local development and test work | `permissive` when created | Same non-governed baseline; development-only behavior is visibly recorded | No |

New governed homes create a public trust store and a `policy/workflows/`
directory. They default to `policy.profile: allowlist` and deny remote secret
delivery for `process`, `http`, and `mcp`. New standalone and development homes
also initialize remote secret delivery as disabled, but use permissive policy.
Enable delivery only for the adapter a workflow actually needs.

## Governed mode

Governed mode is the default because it requires both trust and exact runtime
authority before dispatch:

- Import only public trust roots with `transist trust import`; private signing
  keys remain on an administrative or build machine.
- Install plugins with verified detached signatures.
- Deliver workflows as immutable packages created by
  `transist-admin workflow package`.
- Install a verified package with `transist workflows install --package
  <package-directory>`. Installation persists immutable code only; it does not
  bind inputs or grant authority.
- Use `transist workflows check <id>@<version>` with repeatable
  `--input key=value` arguments or `--inputs-file <inputs.json>` to review
  input-bound validation without live probes. Use `--live` or `--output` for
  live readiness; a passing default check is not live evidence. Bind the same inputs with `workflows authorize` only
  for the exact missing authority, then rerun the check.
- Grant only the requested version-pinned plugin actions and exact capability
  resources. A missing or partial grant fails before dispatch.

See [Governed Runtime Setup and Delivery](../guides/trust/governed-runtime-setup-and-delivery.md)
for the complete lifecycle.

## Non-governed modes

Standalone and development modes are deliberate local choices, not a shortcut
for governed delivery. They are useful when evaluating an unsigned example or
iterating locally in an isolated home. Keep their homes separate from regular
governed homes, use an explicit `--home` or `TRANSIST_HOME` for disposable work,
and do not use them to bypass a governed install, policy, or signature failure.

The [Quickstart](../guides/getting-started/quickstart.md) is an intentionally
temporary standalone evaluation. The [Trusted Governed Execution Walkthrough](../guides/trust/trusted-execution-walkthrough.md)
uses a pre-signed fixture to demonstrate the runtime-side trust loop.

## Related controls

These controls are separate from `runtime.mode`:

| Control | Purpose |
| --- | --- |
| `policy.profile` | Selects `allowlist`, `permissive`, or `deny_all` policy behavior. Governed homes require `allowlist`; a capability is enforced only under `allowlist`. |
| `--trusted-execution` | Requests signature verification in standalone or development homes. Governed homes require it automatically. It never creates or signs trust material. |
| `--local-only` | Per-run locality ceiling. It denies external-provider actions and caps applicable network access at loopback. It does not make a run trusted or grant policy authority. |
| Management mode | Separate supervision metadata. It is not the local runtime security posture and does not replace `runtime.mode`. |

Runtime mode, policy profile, and per-run flags are all persisted in the
inspection and provenance surfaces so operators can determine the posture under
which a run was evaluated.

## Configuration rules

- `runtime.mode` must be explicitly `governed`, `standalone`, or `development`.
- `policy.profile` must be explicitly `allowlist`, `permissive`, or `deny_all`.
- `runtime.mode: governed` requires `policy.profile: allowlist`.
- A declared workspace, plugin, workflow, or network policy requests authority;
  it does not grant it.
- A successful static check is evidence only. Installation, preparation, and
  dispatch still enforce the active trust, policy, secrets, and live conditions.

For exact command options, see the [CLI Reference](cli.md). For policy shapes,
see [Secrets and Policy](../handbook/04-trust-and-runtime-behavior/02-secrets-and-policy.md)
and [Network Policy](network-policy.md).
