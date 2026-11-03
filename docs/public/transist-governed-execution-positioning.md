# Transist: Governed Execution for AI and Automation Workflows

> **Transist is a governed local execution runtime for explicit AI and
> automation workflows running on devices, machines, and edge infrastructure.**

Transist governs the boundary between automation and the host or device where
that automation runs. It helps operators answer four questions:

- What workload is trusted and permitted to run?
- Which process must it follow?
- Which tools, data, devices, secrets, and network destinations may it access?
- What state and evidence will remain afterwards?

This is broader than running a model locally. A local model can still operate
inside an opaque loop, invoke unrestricted tools, or leave no reliable record.
Transist keeps the workflow explicit and makes the runtime responsible for
verification, authorization, execution, and recording.

## The Execution Boundary

The workflow is the unit of execution. It is an authored, reviewable process
that may combine deterministic logic, local or remote models, human approvals,
APIs, files, hardware, scripts, and existing software.

AI and tools participate in the workflow. They do not become the authority that
defines the process or expands its permissions.

```text
approved workflow / model / plugin / local policy
        -> device / machine / edge node
        -> Transist runtime
        -> verify -> authorize -> execute -> record
        -> local systems / tools / hardware / models
```

The runtime remains deliberately narrow. Provider integrations, model behavior,
planning heuristics, and domain-specific operations belong in workflows,
plugins, adapters, or host integrations rather than in the kernel.

## What Distinguishes Transist

### Run close to the work

Transist runs near the data, tools, devices, and operational systems a workflow
affects. It can operate on a workstation, local server, industrial machine,
edge computer, kiosk, robot controller, embedded product computer, or dedicated
automation station.

Local-first does not mean local-only. A workflow may use local participants,
remote services, or both when its declared capabilities and local policy permit
them. Basic runtime operation does not require an always-available
Transist-hosted service.

### Keep workflows explicit

A Transist workflow has visible steps, declared inputs and outputs, controlled
transitions, and bounded participants. Preparing a run compiles the authored
definition into a persisted recipe snapshot. Runtime orchestration proceeds
from that snapshot instead of allowing a model or plugin to rewrite the process
while it runs.

Explicit workflows can be reviewed, versioned, tested, packaged, signed,
inspected, resumed, and replayed within documented boundaries. External tools,
models, people, and hardware may remain nondeterministic; the kernel keeps its
own orchestration and state transitions explicit.

### Verify trust; do not create it at runtime

Trust creation and trust enforcement are separate. `transist-admin` creates
signing keys and detached signatures on an administrative system. The
`transist` runtime imports public trust roots, verifies signed workflow and
plugin artifacts, records trust decisions, and fails closed when governed
execution requirements are not satisfied.

The runtime does not generate private keys or sign executable artifacts.

### Derive and constrain authority

Workflows declare authority-bearing structure such as selected plugin actions,
workspace and network policy, logical secret references, and approval gates.
The runtime derives normalized capability requests from the persisted recipe
and pinned plugin metadata, then evaluates them against local policy before and
during execution.

Current capability requests cover adapters, pinned plugins and actions,
provider locality, logical secrets, filesystem resources, network origins, and
approvals. Resource confinement remains platform- and adapter-dependent and is
documented in the [security](security/threat-model.md) and
[policy](reference/runtime-modes.md) references.

The governing principle is:

> Automation receives the authority it needs, and no more.

### Treat external output as untrusted input

Model responses, plugin results, API responses, hardware signals, and human
values are data supplied to the workflow. They may influence declared steps,
but they do not redefine kernel execution semantics, add hidden loops, or widen
runtime authority.

Many Transist workflows need no generative AI. Rules, OCR, scripts, classical
models, approvals, hardware, and ordinary integrations can all participate in
the same explicit execution model.

### Preserve inspectable history and evidence

Persisted runs retain the recipe, inputs, state transitions, execution records,
reported artifacts, runtime-generated evidence, approvals, failures, and
lineage needed for inspection and recovery.

Recording is not indiscriminate. Runtime-resolved secrets must not be persisted
in runtime-owned state, events, artifacts, evidence, metrics, or inspection
exports. External participants may transform or disclose data outside the
runtime's visibility, so Transist does not claim to provide DLP or tamper-proof
host isolation.

### Recover explicitly

Failures are durable operational states rather than lost terminal output.
Operators can inspect a failed or canceled run, retry failed work within the
same run history, or replay the persisted recipe and inputs as a new run with
explicit lineage. Manual gates record the actor, decision, reason, and time.

Transist does not claim that external work is deterministic. It preserves the
runtime context and history required to understand what happened and to make
recovery deliberate.

## What the Current Runtime Enforces

Transist is a pre-1.0 public beta. The current runtime provides:

- immutable run-scoped recipe snapshots;
- signed workflow and plugin verification in governed execution;
- normalized capability requests checked against local allowlist policy;
- version-pinned plugin actions and content-immutable plugin installation;
- filesystem and network policy enforced through platform-specific adapter
  boundaries;
- optional `--local-only` dispatch denial for actions declared as external,
  with applicable local network access capped at loopback;
- persisted state, execution history, artifacts, evidence, provenance, and
  manual-gate decisions;
- inspection through the CLI, a local read-only companion server, and
  Prometheus metrics;
- explicit retry, resume, replay, cancellation, and cleanup operations.

Some governance questions remain more precise than others. Provider locality
depends on signed plugin declarations, platform sandboxes have documented
differences, approver identity is recorded rather than externally authenticated,
and usage is recorded only when an adapter or plugin reports it. See
[Known Limitations](reference/known-limitations.md) for the exact boundaries.

Automated quality, security, policy, and cost-threshold gates are workflow or
extension patterns, not built-in gate types. The built-in gate is the recorded
manual decision point.

## Local Execution and Shared Visibility

Local runtime state remains authoritative for execution and recovery. The
companion server exposes read-only inspection and metrics for one runtime home.
Optional team-level collection or dashboards may be built above that boundary;
they do not replace local state or introduce a mandatory control plane.

This keeps the runtime useful in disconnected or limited-connectivity settings
while allowing operators to integrate it with broader observability systems.

## Deployment Patterns

Transist is hardware-vendor and device-category neutral. Example placements
include:

- engineering workstations and local servers;
- industrial machines and their control computers;
- edge nodes, kiosks, robots, and robotic controllers;
- embedded product computers;
- dedicated automation stations beside scanners, cameras, hardware, or local
  databases.

An automation station may inspect products, process scanner input, provision
hardware, assist a technician, query a local knowledge base, or coordinate a
controlled operational process. It may use a local model, selectively call a
remote model, or use no generative AI.

The important property is governed, explicit execution close to the work, not
a particular hardware form factor.

## Open by Design

Transist executes inside the user's trust boundary, so its execution logic and
security behavior must remain inspectable. Users should be able to audit,
build, test, and embed the runtime independently.

A useful installation does not require a Transist cloud account, permanent
vendor connection, bundled model runtime, container platform, or external
database for basic operation. Extensions can be developed and published
against public workflow, plugin, adapter, and protocol interfaces without being
added to the protected kernel.

Open source and upstream governance are separate concerns. The license permits
use, inspection, modification, and redistribution under its terms. The official
upstream remains maintainer-controlled so changes to execution and trust
contracts receive deliberate review.

## What Transist Is Not

Transist is not:

- a generic AI assistant or autonomous-agent framework;
- a model inference engine;
- a firmware updater or OTA delivery platform;
- a generic device-management system;
- a container orchestrator;
- a replacement for the tools, hardware, services, or systems a workflow
  invokes;
- a mandatory hosted control plane.

Device-management and OTA systems determine how software reaches a machine.
Transist determines whether a supported automation workload is trusted and
permitted to execute once it is there, runs it through explicit adapter
boundaries, and records enough state and evidence to inspect and recover the
run.

## Who Transist Is For

Transist is for teams embedding AI or automation into real operational
processes where execution authority and post-run evidence matter. This includes
platform engineers, product teams, integrators, device operators, industrial
automation teams, and engineering teams coordinating tools or models on local
infrastructure.

Use Transist when you need to:

- keep an operational process explicit and reviewable;
- control what automation may access or change;
- verify approved workflow and plugin artifacts before execution;
- operate close to devices, local data, and existing tools;
- inspect failures, approvals, artifacts, and evidence afterwards;
- recover persisted work without relying on an opaque agent conversation;
- remain useful without a mandatory remote service.

## Positioning

Transist is the governed execution layer beneath AI, tools, and operational
automation when the process must stay explicit, authority must stay bounded,
and the result must remain inspectable and recoverable.

> **Run close to the work. Keep the workflow explicit. Verify what may run.
> Give it only the capabilities it needs. Preserve evidence of what happened.**

## Where to Go Next

- [Overview](overview.md) for the current runtime model
- [Runtime Modes](reference/runtime-modes.md) for governed, standalone, and
  development postures
- [Compatibility and Stability](reference/compatibility-and-stability.md) for
  pre-1.0 guarantees
- [Threat Model](security/threat-model.md) for security boundaries
- [Known Limitations](reference/known-limitations.md) for current constraints
- [Quickstart](guides/getting-started/quickstart.md) for a local evaluation
