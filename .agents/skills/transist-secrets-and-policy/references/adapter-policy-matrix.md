# Adapter Policy Matrix

Current adapter posture:

- `process`: direct local secret consumption path, allowed by default
- `http`: remote secret delivery is policy-gated and denied by default
- `mcp`: remote secret delivery is policy-gated and denied by default

Locality is a separate dimension from adapter policy:

- plugin manifests declare `default_provider_kind` and per-action `provider_kind` as `local` or `external`
- `--local-only` on prepare, workflow run, preflight, retry, resume, replay, and cleanup denies effective `provider_kind: external` actions before dispatch
- locality is not inferred from `process`, `http`, or `mcp`
- HTTP network policy is enforced by the host adapter for every request phase;
  local-only HTTP has a non-widenable loopback ceiling

Current secret-binding families by adapter:

- `process`: `env` or `temp_file`
- `http`: `header` or `body_field`
- `mcp`: `env` or `arg`

## Safe Defaults

- prefer `process` when the plugin runs locally and can consume secrets directly
- enable remote secret delivery only for the adapter that requires it
- keep plugin-side bindings as narrow as possible

## What To Change

If a workflow fails because a remote adapter cannot receive raw secret material, inspect `config.yaml` under the effective home and confirm whether the needed adapter is allowed. Do not broaden policy for unrelated adapters.

If a workflow fails under `--local-only`, inspect locality metadata (`state.json.local_only_mode`, step request `provider_kind`, and `provenance.json.external_transfer_summary`) instead of changing secret-delivery policy.
