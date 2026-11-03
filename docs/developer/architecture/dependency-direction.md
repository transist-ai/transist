# Dependency Direction

This document records the **normal workspace dependency graph**.
It describes the graph that CI enforces, not an idealized layered architecture.

## Accepted Direct Dependencies

| Crate | Direct workspace dependencies |
|---|---|
| `transist-protocol` | None |
| `transist-prerequisites` | `transist-protocol` |
| `transist-plugins` | `transist-prerequisites`, `transist-protocol` |
| `transist-compiler` | `transist-plugins`, `transist-prerequisites`, `transist-protocol`, `transist-trust-digest` |
| `transist-trust-digest` | None |
| `transist-core` | `transist-prerequisites`, `transist-protocol` |
| `transist-policy` | `transist-core`, `transist-protocol` |
| `transist-storage-local` | `transist-core`, `transist-protocol` |
| `transist-adapters` | `transist-core`, `transist-plugins`, `transist-prerequisites`, `transist-protocol` |
| `transist-metrics` | `transist-core`, `transist-protocol`, `transist-storage-local` |
| `transist-inspection` | `transist-compiler`, `transist-core`, `transist-metrics`, `transist-protocol`, `transist-storage-local` |
| Root `transist` | `transist-adapters`, `transist-compiler`, `transist-core`, `transist-inspection`, `transist-metrics`, `transist-plugins`, `transist-policy`, `transist-protocol`, `transist-storage-local` |
| `transist-cli` | Root `transist` |
| `transist-server` | Root `transist` |
| `transist-admin` | `transist-compiler`, `transist-core`, `transist-plugins` |
| `transist-sandbox-launcher` | None |

`transist-trust-digest` owns only canonical hashing primitives. Workflow asset
discovery remains in `transist-compiler`, and plugin asset discovery remains in
`transist-plugins`; neither implementation is reachable from core. Core still
has no direct dependency on the compiler, plugins, CLI, server, storage,
adapters, policy, or digest crates. The CLI and companion server consume only
the root facade; local inspection and metrics composition stays behind the
root contexts.

Presentation concerns must not leak into lower-level crates: CLI parsing stays
in `transist-cli`, and HTTP transport stays in `transist-server`.

The companion server is a read-only control-plane surface. It consumes the root
facade's inspection and metrics ports, maps typed persisted records into typed
API models, and exposes opaque resource identifiers plus workspace-relative
labels. It does not read arbitrary host paths from API requests or mutate the
runtime home.

Runtime persistence follows the same direction: kernel code calls storage ports
for local staging and publication, while `transist-storage-local` owns the
filesystem implementation. The port boundary must preserve run-birth ordering,
artifact/evidence promotion rules, and all existing durable bytes.

The same rule applies to plugin and workflow assets: `transist-core` receives
plugin action metadata and prompt-file resolution/bytes through narrow ports.
Manifest parsing, installed-plugin layout, prompt-file path validation, and
prompt-file reads remain in the local composition and plugin-management
implementations.

## Enforcement

`scripts/ci/check_workspace_dependencies.sh` compares each listed crate's
**normal and build workspace dependencies** (`cargo tree
--depth 1 --edges normal,build`) with an exact allowlist. It detects direct
graph drift; it does not claim to enforce an abstract layering model or
independently reject every transitive path. The
`transist-sandbox-launcher` is also enumerated and required to have no direct
workspace dependencies. Cargo rejects dependency cycles.

- `scripts/ci/check_kernel_boundary.sh`
- `scripts/ci/check_companion_server_boundary.sh` — scans the companion server's
  non-test source for every filesystem-write and mutation API family
  (`fs::write`, `File::create`, writable `OpenOptions`, `remove_file`, `rename`,
  `create_dir_all`, `set_permissions`, and the `std::io::Write` trait), keeping
  the read-only server boundary enforceable in CI. `test_companion_server_boundary.sh`
  exercises a negative fixture for each family.

`scripts/ci/check_workspace_dependency_policy.py` enforces workspace
dependency centralisation: members are derived from `cargo metadata`, target
tables and renamed dependencies are inspected, every dependency offered in
`[workspace.dependencies]` must be referenced from member manifests via
`workspace = true` (no inline version/path/git/registry override), and any
external dependency used by two or more members must be centralised rather
than repeated with inline versions. Negative fixture tests live in
`scripts/ci/test_workspace_dependency_policy.py`.

`scripts/ci/check_unsafe_policy.py` enforces the workspace unsafe-code
policy: `unsafe_code = "deny"` is required, `#![allow(unsafe_code)]` is
rejected outside an approved allowlist of OS-integration modules, and every
`unsafe` block, function, and implementation requires a preceding
`// SAFETY:` comment.

Update the allowlist and this document together when intentionally changing the
accepted graph. Boundary scripts enforce additional source-level constraints.
