# Transist: Vision

> **Transist is a governed local execution runtime for explicit AI and
> automation workflows running on devices, machines, and edge infrastructure.**

## Why Transist Exists

AI and automation increasingly participate in operational processes: reading
documents, calling tools, interacting with hardware, updating systems,
preparing decisions, and coordinating work across multiple steps. Running a
model locally does not by itself make that work controlled or understandable.

The important question is not only:

> What can the model or tool produce?

It is also:

> What is this workload allowed to do, which process must it follow, what
> may it access, and what evidence will remain afterwards?

Without an explicit execution layer, AI-assisted automation can become a
collection of hidden scripts, mutable prompts, opaque agent loops, unrestricted
tool access, and fragile integrations that are difficult to inspect or recover
after failure.

Transist exists to govern the execution boundary between automation and the
host or device where that automation runs.

## The Transist Approach

Transist runs close to the data, devices, tools, and operational systems it
affects. The workflow remains the unit of execution: an authored, reviewable
process that may combine deterministic logic, local or remote models, human
approvals, APIs, files, hardware, and existing software.

AI may contribute to a workflow, but it does not become the authority that
defines the process or expands its permissions.

Conceptually, the boundary is:

```text
approved workflow / model / plugin / local policy
        -> device / machine / edge node
        -> Transist runtime
        -> verify -> authorize -> execute -> record
        -> local systems / tools / hardware / models
```

The current runtime verifies detached signatures over authored workflow
definition sets and manifest-declared plugin package payloads in governed
execution. It derives normalized capability requests from a run-scoped
persisted recipe, evaluates them against configured local policy at preflight
and execution checkpoints, executes through built-in adapter boundaries, and
retains local state, execution records, reported artifacts, and
runtime-generated evidence for persisted runs. The broader direction is to
make this boundary precise enough to answer:

- Is this workflow or package trusted?
- Is this exact workflow version permitted?
- Is the selected model identity, version, or content hash permitted?
- Is this plugin trusted?
- Are the requested capabilities allowed?
- May this workflow access this file, API, device, command, secret, or network
  destination?
- What exactly executed, and what evidence remains afterwards?

Not every question above is enforced at the same granularity today. They define
the execution-governance boundary Transist is evolving toward; current
guarantees remain those documented in the runtime, security, and compatibility
references.

## Core Ideas

### 1. Run close to the work

Automation should be able to execute where the process happens:

- on industrial machines and their control computers;
- on edge computers, kiosks, robots, or robotic controllers;
- on local servers and workstations;
- inside embedded product computers;
- on dedicated automation stations;
- beside devices, scanners, cameras, and local databases.

This reduces unnecessary data movement, makes local systems easier to reach,
and allows useful workflows to continue without a mandatory central service.

Local-first does not mean local-only. A workflow may use a local model, call a
remote model, or combine local and remote services when local policy permits
it. The execution boundary remains under the operator's control.

### 2. Keep workflows explicit

A Transist workflow is an authored process with visible steps, declared inputs
and outputs, controlled transitions, and bounded participants. It is not an
invisible autonomous loop hidden behind a generic agent abstraction.

Explicit workflows can be reviewed, versioned, tested, packaged, signed,
compared, inspected, and repeated within known boundaries. Deterministic logic
is used where appropriate; external tools, models, people, and hardware may
remain nondeterministic. Recovery and resumability should be explicit where the
runtime supports them.

### 3. Verify and constrain execution

Local execution must not imply unrestricted access to the host system.
Authored workflows declare authority-bearing structure such as selected plugin
actions, workspace and network policy, logical secret references, and approval
gates. The runtime derives normalized capability requests from that structure
and pinned plugin metadata, persists them with the run recipe, and evaluates
them against local policy at preflight and execution checkpoints.

Current capability requests cover adapters, pinned plugins and actions,
provider locality, logical secrets, filesystem resources, network origins, and
approvals. Authorization determines whether dispatch may proceed. Resource
confinement is platform- and adapter-dependent and must be documented honestly.

The goal is simple:

> Automation receives the authority it needs, and no more.

### 4. Treat AI as a bounded component

A model may classify a document, extract information, propose an action,
generate content, or assist with a bounded decision. Model output is still
untrusted input to the runtime. It stays inside the workflow's declared
structure and does not silently redefine execution semantics or authority.

Many useful workflows use no generative AI. They may combine rules, OCR,
scripts, classical models, approvals, and ordinary integrations. Transist is
not an AI agent framework or a generic policy engine; it is a workflow runtime
with an explicit execution and trust boundary.

### 5. Preserve execution history and evidence

Operational automation must remain understandable after it runs. Transist
should preserve enough structured information to answer:

- Which workflow version and persisted recipe executed?
- Which tools, plugins, models, and capabilities were involved?
- Which decisions and approvals occurred?
- What failed, retried, resumed, or changed?
- Which artifacts were produced?
- What evidence supports the outcome?

The purpose is not indiscriminate recording. Secrets and sensitive data must
not be persisted merely for completeness. Important execution facts should be
durable, inspectable, and retained under explicit policy.

## Deployment Patterns

Transist is hardware-vendor and device-category neutral. It may run on an
industrial machine, edge computer, kiosk, robotic controller, local server,
workstation, embedded product computer, or dedicated automation station.

An independent automation station remains a useful pattern: a dedicated
computer, appliance, or device that performs a defined operational function
using approved workflows. It might process scanner input, inspect products with
a camera, provision hardware, assist a technician, use a local knowledge base,
or operate with limited connectivity. It may use a local model, selectively use
a remote model, or use no generative AI.

The important property is governed, explicit execution close to the work, not
a particular hardware form factor.

## Open by Design

Transist is open because it executes inside the user's trust boundary. Users
should be able to:

- inspect the execution logic;
- audit trust verification and policy behavior;
- build and test the runtime independently;
- embed it into their own devices and products;
- operate it without a mandatory Transist-hosted service.

The project should favor open formats, stable interfaces, inspectable behavior,
and a small operational footprint. A useful installation should not require a
cloud account, a permanent vendor connection, a bundled model runtime, a
container platform, or an external database for basic operation.

Open source and upstream governance are separate concerns. The license permits
inspection, use, modification, and redistribution under its terms. The
official Transist upstream remains maintainer-controlled so changes to its
execution and trust contracts receive deliberate review. Openness does not
imply unrestricted upstream write or merge access.

Extensions do not need to enter the protected kernel. Users can develop and
publish workflows, plugins, adapters, integrations, examples, and operator
tooling against public interfaces, including from independent repositories.

## What Transist Is Not

Transist is not:

- a firmware updater or OTA delivery platform;
- a generic device-management system;
- a container orchestrator;
- a model inference engine;
- a generic autonomous-agent platform;
- a replacement for the tools and systems a workflow invokes.

Device-management and OTA systems determine how software reaches a machine.
Transist determines whether an automation workload is trusted and permitted to
execute once it is there.

Transist may integrate with these systems, but its responsibility is narrower:
verify supported signed workflow and plugin artifacts, evaluate normalized
capability requests, execute an explicit workflow through adapter boundaries,
and record enough state and evidence to inspect and recover persisted runs.

## The Direction

Transist should become a dependable governed execution layer for AI and
automation workflows that must remain close to the systems they affect. It
should be compact enough for machines, products, and edge devices; useful as a
standalone runtime; transparent and embeddable; and explicit enough that people
can understand what their automation is doing.

> **Run close to the work. Keep the workflow explicit. Verify what may run.
> Give it only the capabilities it needs. Preserve evidence of what happened.**
