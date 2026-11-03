# Threat Model

This document describes the public security posture for the standalone Transist
runtime.

## Assets

- persisted run state, artifacts, evidence, and trust decisions under the local
  Transist home
- authored workflows, plugin manifests, plugin payloads, and detached signatures
- imported public trust roots and local policy material
- runtime secrets referenced through `secret_refs`
- local inspection snapshots and evidence exports

## Trust Assumptions

- The local operator controls the machine, filesystem permissions, and chosen
  Transist home path.
- Workflow authors, plugin authors, and model outputs are untrusted unless the
  operator signs or explicitly trusts them.
- Plugins remain external participants behind the `adapter + plugin + action`
  boundary.
- Local persisted state is authoritative. Optional export and sync surfaces do
  not gain execution authority.

## Attacker Model

- A malicious workflow author tries to smuggle unsafe behavior through workflow
  inputs, prompt files, helper scripts, or capability requests.
- A malicious plugin author tries to abuse filesystem, network, process, or
  environment access.
- A compromised adapter or plugin process emits misleading output after partial
  side effects.
- A network attacker interferes with optional outbound metadata delivery.
- A local non-admin user with filesystem access tries to modify trust material,
  signatures, policy, or run state.

## In-Scope Controls

- trusted execution for signed workflows and plugins
- asset locks for plugin bytes and prompt files during re-entry, plus private
  fresh-run capture of resolved workflow assets
- deny-by-default capability handling, scoped secret access, and local policy allowlists
- explicit operating modes and development-only trust-bypass gating
- local-only execution mode for refusing external-provider steps before dispatch
- child-process environment isolation for process and MCP adapters unless inheritance is explicitly restored
- invocation-private staging for process and MCP plugins; durable run state is
  not exposed to plugin processes
- private `workflow_assets/` captures are not inspection or export payloads;
  workflow authors must not place secrets in captured prompt or helper files
- repository-scoped runtime homes are kept outside the workspace mount, and
  dispatch/preflight reject a home that resolves inside that mount
- Linux local sandboxes mount only declared system, plugin, workspace,
  invocation-staging, and exact invocation-temporary roots; operator `PATH`
  directories and general host `/tmp` are not implicitly mounted
- macOS local sandboxes deny reads from the Transist home, shared staging
  parent, and general system temporary parent, then reopen only the current
  plugin package, workspace, declared external paths, current invocation
  staging root, and exact invocation-temporary root; writes remain denied by
  default and OS temporary directories are not blanket write grants
- filesystem grants overlapping protected runtime roots are rejected, and
  process/MCP `runtime://temp` and `runtime://temp/<relative-path>` declarations grant no additional host
  authority; execution plus preflight force `TMPDIR` to the selected private
  runtime-temporary directory after environment processing
- preflight secret-availability checks that do not resolve or deliver secret
  values
- append-only run records, evidence, and inspection exports derived from local
  persisted state
- release validation that binds archives to an exact clean candidate tree,
  verifies checksums and signatures, records build and packaging provenance, and
  validates the generated SBOM before publication

## Out of Scope

- preventing a local administrator from replacing binaries or editing files
- exactly-once guarantees for external side effects
- stronger plugin isolation than the configured adapter and host OS provide
- unrelated files readable by the same user on macOS; `sandbox-exec` protects
  Transist-owned roots but is not a complete same-user host read isolation
  boundary
- treating metadata export as an execution-control channel
- compliance, certification, or tamper-proof claims

## Offline Posture

- Standalone execution requires no network.
- Trusted execution uses trust roots already present on disk.
- Evidence and inspection remain available from local state even when fully
  offline.

## Security Claims We Do Make

- the runtime verifies trust; it does not create private trust material
- executable inputs can be signed outside the runtime and verified before
  dispatch
- cancellations, retries, replay, and cleanup operate from persisted local state
- inspection and evidence export are derived from durable local records

## Security Claims We Do Not Make

- Transist is not tamper-proof against a local administrator.
- Transist does not guarantee deterministic behavior from external tools.
- Transist does not claim regulatory certification by itself.
- Release provenance, signatures, and SBOM validation establish artifact
  lineage and integrity checks; they do not make a host or a local administrator
  trustworthy.

## Related Docs

- [Quickstart](../guides/getting-started/quickstart.md)
- [Compatibility and Stability](../reference/compatibility-and-stability.md)
