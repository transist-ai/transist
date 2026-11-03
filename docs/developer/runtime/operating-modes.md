# Runtime Operating Modes

Transist now parses an explicit `runtime.mode` from `config.yaml`.

## Modes

- `standalone`: explicit operator-selected local mode. It may use permissive policy and records that capability enforcement was bypassed.
- `governed`: fail-closed mode. A run without an allowed trust decision is denied before execution.
- `development`: explicit development mode. The runtime emits a warning and must not be selected by workflow or plugin version metadata.

## Defaults

New homes write governed execution with exact capability grants:

```yaml
runtime:
  mode: governed
policy:
  profile: allowlist
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
```

Use `transist home init --mode standalone` or `--mode development` only when
the operator explicitly wants a non-governed home. Governed homes reject
`policy.profile: permissive` and automatically verify workflow and plugin
signatures for lifecycle and recovery commands. A missing `config.yaml` is a
configuration error, not a permissive fallback.

The `v0` workflow/test-plugin bypass is not tied to any mode. It is compiled only with the non-default `dev-version-bypass` feature.

The fullscreen TUI displays the effective home, configured mode, and policy
profile read-only. Initialize or change a home only with `transist home` CLI
commands. Invalid mode/profile combinations are rejected rather than displayed
as an active posture.
