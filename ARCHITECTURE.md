# Transist Architecture

Transist is a governed local execution runtime for explicit AI and automation
workflows, built around a small, protected execution kernel. The kernel owns
runtime-controlled orchestration semantics,
durable state transitions, recovery rules, and the boundary between the
runtime and external work. External results — scripts, APIs, models, people,
hardware — are not assumed to be deterministic; the kernel's own orchestration
is.

## System Shape

```text
authored workflow (transist.flow.v1)
  -> compiler: validate + freeze
  -> persisted immutable recipe snapshot (recipe.yaml)
  -> package verification + local policy evaluation
  -> kernel execution (state transitions, recovery, gates)
       |-> durable local state
       |-> events / artifacts / evidence
       `-> capability dispatch (adapter + plugin + action)
             -> adapters: process / HTTP / MCP / host capability integrations
             -> external processes, APIs, tools, hardware-facing services, people
```

The kernel never executes directly from a mutable authored workflow. It
prepares or installs a workflow into a persisted recipe snapshot first, then
executes from that snapshot. During execution the kernel continuously reads and
writes durable local state, emits events, records artifacts and evidence, and
dispatches external work through the `adapter + plugin + action` boundary.
Local runtime state is the source of truth for execution and recovery; optional
inspection, export, or synchronization surfaces do not replace it.

## Trust Boundary

Trust creation and trust enforcement are separate, and live in different
binaries.

- `transist-admin` is the admin CLI that creates trust material: Ed25519
  keypairs and detached `transist.signature.v1` signatures for workflows and
  plugins. It owns private-key handling.
- `transist` is the runtime. It imports public `transist.trust_root.v1` keys,
  verifies detached signatures, evaluates policy, persists a
  `transist.trust_decision.v1` per run, and may refuse execution. It never
  creates private trust material.
- Governed homes automatically require trusted workflow/plugin verification for
  install, prepare, workflow run, resume, replay, cleanup, and preflight.
  On standalone or development homes, trusted execution is opt-in via
  `--trusted-execution` on workflows install, plugins install, prepare, workflow run,
  run, and `runs preflight`.
- Local-only execution is opt-in via `--local-only`, denying steps whose
  declared provider is external before dispatch.
- Runtime machines do not generate private keys or sign executable artifacts.

## Workspace Roles

Crates are grouped by layer. Names match the workspace in `Cargo.toml`.

### Protocol and contracts

- `transist-protocol`: wire-level contracts and durable data models; the
  bottom of the dependency graph.
- `transist-trust-digest`: trust-envelope digest rules shared by admin and
  runtime.

### Kernel

- `transist-core`: kernel execution semantics, state transitions, recovery,
  and ports. Stays pure: defines traits, not CLI, HTTP, storage, or adapter
  implementations.

### Preparation and policy

- `transist-compiler`: workflow loading, validation, and immutable recipe
  compilation.
- `transist-plugins`: plugin manifest loading, validation, installation, and
  resolution.
- `transist-policy`: trust, capability, and runtime policy enforcement.

### Infrastructure and extension execution

- `transist-storage-local`: filesystem-backed run state, artifacts, evidence,
  and indexes.
- `transist-adapters`: process, HTTP, and MCP execution surfaces.

### Inspection and metrics

- `transist-inspection`: local inspection models, snapshot building,
  redaction, and export helpers.
- `transist-metrics`: Prometheus-oriented runtime metrics.

### Operator surfaces and composition

- `transist-cli`: operator CLI surface.
- `transist-server`: read-only local inspection companion.
- `transist-admin`: signing and trust-material tooling.
- `transist-prerequisites`: shared static host-prerequisite checks; executable
  work belongs to an explicit adapter/plugin/action boundary.
- root `transist`: default local composition layer used by the CLI and server.

The companion server is composed from the root inspection and metrics facades.
Its HTTP mapping is typed and read-only: persisted run records are decoded into
inspection models, workflow-file responses expose opaque resource IDs and
workspace-relative labels, and no absolute host path is returned by `/api/v1`.
The server has no filesystem mutation API and does not replace local runtime
state as the source of truth.

## Dependency Direction

- Lower-level crates never depend on higher-level interfaces.
- `transist-core` stays pure: it defines ports and execution rules, but not
  CLI, HTTP, storage, or adapter implementations.
- `transist-protocol` stays at the bottom of the graph.
- User-facing binaries depend on the root `transist` crate instead of wiring
  the workspace ad hoc.
- The companion server depends on the root facade and inspection ports rather
  than storage implementations or kernel mutation services.

Repository guards enforce these rules:

- `scripts/ci/check_workspace_dependencies.sh`
- `scripts/ci/check_workspace_dependency_policy.py`
- `scripts/ci/check_unsafe_policy.py`
- `scripts/ci/check_kernel_boundary.sh`
- `scripts/ci/check_companion_server_boundary.sh`
- `scripts/ci/check_no_runtime_signing.sh`

`check_no_runtime_signing.sh` rejects signing helpers, private-key encoding,
and keygen logic in `src/` and `crates/`, keeping trust creation out of the
runtime.

The workspace sets `unsafe_code = "deny"` at the `[workspace.lints.rust]`
level. `allow(unsafe_code)` is permitted only in an approved allowlist of
OS-integration modules, narrow function-level wrappers, and test helpers.
A CI policy checker (`scripts/ci/check_unsafe_policy.py`) rejects
`allow(unsafe_code)` (file-level, item-level, or via `cfg_attr`) outside
the approved list, detects `unsafe` constructs including `unsafe trait`,
strips string literals to avoid false matches, and requires a preceding
`// SAFETY:` comment on every unsafe block, function, implementation, and
trait. `missing_docs` is enforced for every workspace library and binary
via `scripts/ci/check_missing_docs.sh`, and
`RUSTDOCFLAGS="-D warnings" cargo doc` is part of the PR-readiness gate.

## Kernel Contract Summary

The protected runtime behavior is documented in
`docs/developer/architecture/kernel-contract.md`. The public summary:

- runs execute from persisted `recipe.yaml`, not from live re-interpretation
- runtime-owned state transitions are append-only and inspectable
- replay and retry are explicit operator actions with persisted lineage
- all external work crosses the `adapter + plugin + action` boundary
- local state remains authoritative even when optional export or sync surfaces
  are enabled
- external determinism, exactly-once side effects, and multi-node scheduling
  are explicitly not guaranteed in v1

## Build From Source

Rust `1.88.0` is both the repository build pin and the minimum supported Rust
version (MSRV). It remains authoritative until an explicit policy change; see
[Compatibility & Stability](docs/public/reference/compatibility-and-stability.md)
and the [Dependency Update Policy](docs/public/reference/dependency-update-policy.md)
for enforcement.

```bash
cargo +1.88.0 build --workspace
cargo +1.88.0 test --workspace
```

Useful entry points while developing from source:

```bash
cargo +1.88.0 run -p transist-cli -- --help
cargo +1.88.0 run -p transist-server -- --help
cargo +1.88.0 run -p transist-admin -- --help
```

For full contributor setup and validation, see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Related Docs

- [Quickstart](docs/public/guides/getting-started/quickstart.md)
- [Kernel Contract](docs/developer/architecture/kernel-contract.md)
- [Dependency Direction](docs/developer/architecture/dependency-direction.md)
- [Repository Organization](docs/developer/architecture/repo-organization.md)
- [Compatibility and Stability](docs/public/reference/compatibility-and-stability.md)
- [Dependency Update Policy](docs/public/reference/dependency-update-policy.md)
- [Threat Model](docs/public/security/threat-model.md)
- [Platform Distribution Signing](docs/public/reference/platform-distribution-signing.md)
- [Contributing](CONTRIBUTING.md)
