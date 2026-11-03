# Contributor Guardrails

This document is the guardrail for changes that touch the protected runtime,
the kernel contract, or public execution surfaces. Read it before proposing a
change in those areas.

For ordinary workflow, plugin, adapter, documentation, and example
contributions, start at [CONTRIBUTING.md](CONTRIBUTING.md) instead. This file
does not apply to every contribution.

---

## What Transist Is

**Transist** is a governed local execution runtime for explicit AI and
automation workflows running on devices, machines, and edge infrastructure. It
runs close to the data, devices, tools, and operational systems it acts on.

**The Transist kernel** is the small, protected core that owns
runtime-controlled orchestration semantics: deterministic state transitions,
recovery and replay rules, persisted execution history, and the boundaries
between the runtime and external work.

Workflows may combine deterministic logic, scripts, local or remote models,
bounded AI-assisted steps, human approvals, APIs, files, and hardware. The
kernel is the predictable foundation those participants run on.

## What Transist Is Not

Transist is not an agent framework.

The kernel itself is not "smart." It does not own planning loops, memory
systems, opaque automation, or a model runtime. Transist workflows may
coordinate deterministic tools, bounded AI-assisted steps, external services,
hardware, and human actions, but that coordination is explicit and authored —
not an autonomous loop hidden behind a generic agent abstraction.

---

## Core Architectural Principles

- **Deterministic runtime semantics.** Kernel-owned orchestration, state
  transitions, retry and recovery semantics, and side-effect boundaries must
  be explicit and deterministic. External results are not assumed to be
  deterministic: scripts, APIs, models, people, and hardware may be
  nondeterministic.
- **Durable, inspectable runtime history.** State transitions owned by the
  runtime must be durable and inspectable. Artifacts and evidence are retained
  according to explicit storage, redaction, and security policy — not saved
  indiscriminately. Secrets must never appear in logs, artifacts, or state.
- **Narrow boundaries.** The walls between the kernel, adapters, and plugins
  must stay high and obvious. All external work crosses a formal
  `adapter + plugin + action` boundary.
- **Explicit capabilities.** Workflows and extensions declare the
  capabilities they require. The runtime evaluates those capabilities before
  and during execution. Authority is declared and policy-controlled; it is not
  implicit.
- **Local state remains authoritative.** Local runtime state is the source of
  truth for execution and recovery. Optional export, inspection, or
  synchronization surfaces do not replace it.
- **No mandatory remote service.** The runtime must support useful workflows
  that require no network, and its own core execution must not depend on an
  always-available remote service. Individual workflows may still require
  connectivity when they explicitly use remote capabilities.
- **Core is small.** If a behavior can live in a workflow, a plugin, an
  adapter, or a host capability integration without weakening the kernel
  contract, it does not belong in the kernel.

---

## Extension-First Rule

Complexity should be pushed to extensions, not the kernel. If a new behavior
can be implemented as a workflow, a plugin, an adapter, a host capability
integration, or operator tooling without weakening the kernel contract, that
is where it should go.

---

## Trust and Untrusted External Output

**External outputs are untrusted inputs to the runtime.** Model responses,
plugin results, API responses, hardware signals, and human-provided values may
influence declared workflow steps, but they never redefine kernel execution
semantics, silently extend workflow authority, or open hidden control loops.
**LLMs are untrusted participants.** They are an important example of this
principle, not the whole of it.

External output is not authoritative over the kernel. This is not a claim that
all plugin or model output is malicious — only that it is data to be validated,
persisted, and audited, never a source of execution authority.

Auditability and control matter more than convenience shortcuts. If a
"shortcut" makes the system faster but harder to audit, recover, or replay, it
does not go in.

---

## Trust Boundary

Trust creation and trust enforcement are separate.

- **Trust creation** belongs outside the runtime core. Key generation,
  private-key handling, and signing live in the `transist-admin` CLI.
- **Trust enforcement** belongs to the runtime. `transist` imports public
  trust roots, verifies detached `transist.signature.v1` sidecars, evaluates
  policy, and may reject execution. The runtime never creates trust material.

Runtime machines do not generate private keys or sign executable artifacts.

---

## What Must Not Be Added To Core

Do not add the following to the Transist core:

- **Provider-specific logic**: no special cases for OpenAI, GitHub, or any
  other vendor.
- **Planning heuristics**: no autonomous planning or hidden loops inside the
  kernel.
- **Hidden side effects**: no actions that bypass the
  `adapter + plugin + action` boundary.
- **Secret persistence**: no secrets in logs, artifacts, or state.
- **Trust creation**: no key generation, private-key handling, or signing flows
  in the runtime core. Trust is created in `transist-admin`, then verified by
  `transist`.
- **Opaque logic**: no "magic" that trades recovery clarity for developer
  convenience.

---

## When Is a Core Change Justified?

The core is only changed when necessary to protect or evolve the **Kernel
Contract**, and only when the change cannot be implemented elsewhere.

Before you touch the core, align your plan with:

- [The Transist Vision](VISION.md)
- [The Kernel Contract](docs/developer/architecture/kernel-contract.md)
- [Compatibility & Stability](docs/public/reference/compatibility-and-stability.md)
- [The Core Change Skill](.agents/CORE_CHANGE_CHECKLIST.md)

If it doesn't fit the vision, it doesn't go in the core.
