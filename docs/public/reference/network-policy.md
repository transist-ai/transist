# Network Policy Reference

`policy.network` is the runtime-owned network governance surface for authored workflows, home defaults, adapters, and plugins.

## Authored Shape

Workflow authors declare network policy with a `network:` block at workflow and step scope.

```yaml
network:
  mode: allowlist # any | loopback | allowlist | none
  allow_hosts:
    - api.example.com
  allow_ports:
    - 443
  tls_required: true
  max_redirects: 5
```

Global home defaults live under `<home>/settings.yaml` as `network_policy:`.

## Resolution

- `mode` uses whole-policy replacement: step -> workflow -> global -> builtin.
- `allow_hosts` and `allow_ports` merge additively across all levels with duplicates removed.
- In HTTP allowlists, `allow_hosts` accepts normalized host names or explicit
  `scheme://host:port` origins; `allow_ports` must contain the effective port.
- `tls_required` and `max_redirects` follow the most-specific policy source.
- Builtin default: `mode: any`, empty allow-lists, `tls_required: false`, `max_redirects: 0`.

## Forwarding

When the resolved policy is non-default, the kernel emits `request.policy.network` with:

- `source`
- `mode`
- `allow_hosts`
- `allow_ports`
- `tls_required`
- `max_redirects`

The HTTP adapter enforces this block at the host boundary before every
handshake, health probe, invoke, redirect, and cancellation request.

## Enforcement Matrix

- `mode: any`: unrestricted initial destination subject to normal TLS validation; cross-authority redirects remain denied and must be enumerated under `allowlist`.
- `mode: allowlist`: the HTTP adapter requires the resolved scheme, normalized host, effective port, and TLS requirement to match the authored policy. Every redirect is checked again.
- `mode: none`: the HTTP adapter opens no connection, including to loopback.
- `mode: loopback`: the HTTP adapter resolves the destination and allows it only when every result is loopback. Non-empty authored `allow_hosts` and `allow_ports` additionally narrow the permitted loopback destinations.
- `max_redirects: 0` uses the transport default of 10; a positive value sets the limit.

All HTTP modes retain normal TLS authenticity checks. An HTTP plugin's optional
`ca_cert` only adds a custom trust root; it never disables certificate-chain,
expiry, hostname, or signature validation.

Platform support:

- macOS: enforced with `sandbox-exec`. `loopback` denies `network-outbound` while allowing loopback and `network-bind`; `none` denies `network-outbound` including loopback, while still allowing `network-bind`.
- Linux: enforced with `bwrap --unshare-net`, which isolates the network namespace and blocks external egress. In both `none` and `loopback` modes the runtime requires `transist-sandbox-launcher` and runs it as the private namespace root. The launcher explicitly disables loopback for `none` or enables it for `loopback`, verifies the interface state, then drops all capabilities (including `CAP_NET_ADMIN`) before the plugin executes. Bubblewrap's initial loopback state is not relied upon. Missing launcher or interface-setup errors fail closed before plugin execution. `any` mode leaves the shared namespace unchanged.
- Windows and other unsupported targets: no local sandbox backend; strict local network policy fails closed. See [Supported Platforms](supported-platforms.md).

For local `process` and `mcp` steps, `mode: allowlist` is **cooperative
destination metadata**, not enforcement at the network boundary. The sandbox
does not filter by destination:

- On macOS, `sandbox-exec` either permits or denies outbound networking
  wholesale; it cannot restrict which hosts or ports a child process reaches.
  An `allowlist` is therefore advisory for process and MCP steps on macOS.
- On Linux, `bwrap --unshare-net` isolates the entire network namespace; the
  runtime applies it only when the effective policy is `none` or `loopback`,
  and uses an internal launcher to disable loopback for `none` or enable it
  for `loopback` before dropping capabilities and executing the plugin.
  For an effective `allowlist`, no network namespace is unshared, so the
  allowlist constrains nothing at the sandbox boundary.

`none`, `loopback`, and `--local-only` enforcement are unaffected. HTTP steps
remain enforced at the host boundary as described above.

## Local-Only Behavior

For `--local-only` runs, HTTP invoke steps are always capped at
`policy.network.mode = loopback` unless an authored `none` policy is stricter.
This ceiling is independent of `provider_kind`, cannot be widened by an
allowlist or `any` policy, preserves authored host/port narrowing inside the
loopback ceiling, and prevents proxies and non-loopback DNS results.

Local process and MCP steps whose effective `provider_kind` is `local` are
also capped at `loopback` under `--local-only`. Authored `any` and
`allowlist` modes are narrowed to `loopback`; authored `none` and `loopback`
are preserved as-is. This ensures local plugin processes cannot make
non-loopback connections even when the workflow author declares a wider
network policy. The effective `source` becomes `builtin` when the runtime
narrows the policy.

Invoke steps whose effective `provider_kind` is `external` are still denied before dispatch under `--local-only`.

## Related

- [Workflow Schema](workflow-schema.md)
- [TEP](tep.md)
- [APP](app.md)
