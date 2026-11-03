# Stack and Governance

Transist is designed to be a sturdy runtime layer in a technical stack. It does not replace your agents, CLIs, APIs, MCP servers, source control, or ticketing systems. It governs workflow runs across them.

## The Transist Stack

The system is built from several independent layers. This decoupled design keeps workflow semantics separate from tool-specific behavior:

1. **Compiler**: Turns your human-readable YAML into machine-executable "Recipes."
2. **Kernel**: The heart of the system that manages runs, steps, and state.
3. **Adapters**: The bridge to the outside world (e.g., local processes, HTTP, MCP).
4. **Plugins**: The tools that actually do the work.
5. **Policy**: The "rules of the road" that decide what is allowed.
6. **Storage**: The local filesystem where all history is kept.
7. **CLI & Server**: The tools you use to control and view the system.

## Governance (Control and Safety)

Governance in Transist starts with explicit control, not a dashboard. The runtime gives you the pieces needed to understand and bound a workflow run:

- **Explicit logic:** Workflows are structured documents, not black-box scripts.
- **Policy:** Runtime policy can restrict workflow refs, plugin IDs, adapter kinds, and secret refs in managed contexts.
- **Durable records:** Run history, artifacts, evidence, events, and gates remain available for inspection.
- **Manual gates:** Human approval can be a first-class part of the workflow run.
- **Separated trust authority:** Signing keys and detached signatures belong in
   the `transist-admin` tool in this monorepo. The runtime verifies and enforces them;
   it does not mint trust locally.

## Runtime posture

Every home declares one runtime mode. Select it deliberately: `transist home init`
defaults to `governed` for regular controlled operation.

| Mode | Operational role | Boundary |
| --- | --- | --- |
| `governed` | Regular controlled delivery | Requires allowlist policy, trusted plugin/workflow verification, and exact grants before dispatch. |
| `standalone` | Temporary local evaluation | Permissive baseline for isolated, explicitly non-governed work. |
| `development` | Local development and tests | Permissive baseline with visibly recorded development posture. |

Standalone and development homes remain local-first, single-machine, and free of
mandatory central orchestration. They must not be used to bypass a governed
signature, trust, or policy failure. `--local-only` is a per-run locality ceiling,
not a mode or trust grant.

For the matrix and initialization rules, see [Runtime Modes](../../reference/runtime-modes.md). For the full governed lifecycle, see [Governed Runtime Setup and Delivery](../../guides/trust/governed-runtime-setup-and-delivery.md).

## Why it matters

By making every part of the stack explicit, Transist keeps automation inspectable. If you need to change how a secret is handled, or restrict which adapters a workflow can use, the answer should live in policy or workflow authoring instead of hidden tool behavior.

This clarity is what allows you to move fast without breaking things.

---

### Related

- [Determinism, Auditability, and Governance Guide](../../guides/trust/determinism-auditability-and-governance-by-example.md)
- [Governed Runtime Setup and Delivery](../../guides/trust/governed-runtime-setup-and-delivery.md)
- [Runtime Modes](../../reference/runtime-modes.md)
- [TEP (Transist Execution Protocol) Reference](../../reference/tep.md)
- [APP (Adapter-Plugin Protocol) Reference](../../reference/app.md)

**Navigation:**

- Previous: [Managing Run Data and Storage](02-run-artifacts-storage-and-evidence.md)
- Next: [Run Metrics and Prometheus](04-run-metrics-and-prometheus.md)
