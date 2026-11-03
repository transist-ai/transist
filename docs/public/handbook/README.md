# Transist Handbook

Read the handbook after the [Quickstart](../guides/getting-started/quickstart.md), which is an intentionally temporary standalone evaluation.

This is the primary conceptual path for readers who already have one successful
run and now want a sharper mental model for authoring,
recovery, inspection, trust, operations, and extension boundaries.

The handbook keeps one idea in view: Transist is a deterministic runtime for
inspectable, recoverable AI/tool workflows. Agents, CLIs, APIs, MCP servers, and
internal tools can all participate in a run, but the workflow control flow and
run history belong to Transist.

## Who This Is For

- Engineers evaluating Transist locally.
- Workflow authors building repeatable AI/tool workflows.
- Operators and platform engineers evaluating local or self-hosted operation.
- Plugin and adapter authors extending the execution boundary.
- Technical decision-makers deciding where Transist fits in a platform stack.

## Reading Order

Before selecting a home, read [Runtime Modes](../reference/runtime-modes.md).
For regular controlled delivery, follow [Governed Runtime Setup and Delivery](../guides/trust/governed-runtime-setup-and-delivery.md) alongside this conceptual path.

### Part 1: First Steps

- [01 Installation And Local Setup](01-first-steps/01-installation-and-local-setup.md)
- [02 Your First Workflow](01-first-steps/02-your-first-workflow.md)
- [03 Running A Workflow](01-first-steps/03-running-a-workflow.md)
- [04 First Run Inspection](01-first-steps/04-first-run-inspection.md)

### Part 2: Everyday Usage

- [01 Workflow File Structure](02-everyday-usage/01-workflow-file-structure.md)
- [02 Inputs](02-everyday-usage/02-inputs.md)
- [03 Invoke Steps](02-everyday-usage/03-invoke-steps.md)
- [04 Control Steps](02-everyday-usage/04-control-steps.md)
- [05 Guards And Branching](02-everyday-usage/05-guards-and-branching.md)
- [06 Loops](02-everyday-usage/06-loops.md)
- [07 Subflows](02-everyday-usage/07-subflows.md)
- [08 Authoring Patterns](02-everyday-usage/08-authoring-patterns.md)

### Part 3: Inspection And Recovery

- [01 Inspecting Runs](03-inspection-and-recovery/01-inspecting-runs.md)
- [02 Artifacts, Evidence, And Events](03-inspection-and-recovery/02-artifacts-evidence-and-events.md)
- [03 Manual Gates](03-inspection-and-recovery/03-manual-gates.md)
- [04 Resume And Replay](03-inspection-and-recovery/04-resume-and-replay.md)
- [05 Troubleshooting](03-inspection-and-recovery/05-troubleshooting.md)

### Part 4: Trust And Runtime Behavior

- [01 Determinism And Auditability](04-trust-and-runtime-behavior/01-determinism-and-auditability.md)
- [02 Secrets And Policy](04-trust-and-runtime-behavior/02-secrets-and-policy.md)
- [03 Storage Model](04-trust-and-runtime-behavior/03-storage-model.md)
- [04 Companion HTTP Server](04-trust-and-runtime-behavior/04-companion-http-server.md)

### Part 5: Admin And Operations

- [01 Operating Transist Locally](05-admin-and-operations/01-operating-transist-locally.md)
- [02 Run Artifacts, Storage, And Evidence](05-admin-and-operations/02-run-artifacts-storage-and-evidence.md)
- [03 Stack And Governance](05-admin-and-operations/03-stack-and-governance.md)
- [04 Run Metrics And Prometheus](05-admin-and-operations/04-run-metrics-and-prometheus.md)

### Part 6: Architecture And Extension

- [02 Adapters And Plugins](06-architecture-and-extension/02-adapters-and-plugins.md)
- [03 Plugin Authoring](06-architecture-and-extension/03-plugin-authoring.md)
- [04 Adapter Responsibilities](06-architecture-and-extension/04-adapter-responsibilities.md)

### Part 7: Protocols And Internals

- [01 TEP](07-protocols-and-internals/01-tep.md)
- [02 APP](07-protocols-and-internals/02-app.md)
- [03 Internals And Boundaries](07-protocols-and-internals/03-internals-and-boundaries.md)

## Companion Layers

- Complete workflow tutorial: [Build Your First Real Workflow](../guides/authoring/advanced-workflow-authoring.md), with typed input, a direct JSON output handoff, a manual gate, and a final echo using the bundled `shell` plugin.
- Complete plugin tutorial: [Write Your First Plugin](../guides/plugins/write-your-first-plugin.md), a local Python APP greeting plugin invoked from a workflow.
- [Reference](../reference/README.md) for exact lookup material.
- [Guides](../guides/README.md) for practical walkthroughs.
- [Glossary](../../GLOSSARY.md) for normalized terms.
