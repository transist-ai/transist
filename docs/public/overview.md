# Transist: Governed Execution for AI and Tool Workflows

Transist is the execution layer for engineering teams building multi-step AI-assisted workflows. It turns ad-hoc AI usage into repeatable, inspectable, verifiable, and recoverable execution that stays under your control.

## Why Governed Execution?

AI-assisted work is moving from single prompts to multi-step execution. A single workflow run might call an LLM, run a shell command, read a file, and query an API — all while handling retries and errors.

Most AI runtimes let an agent decide what to run next and write the "workflow" at execution time. That model is convenient, but it collapses the definition and the execution logic into the same object. There is no stable content to review, sign, or verify. When something goes wrong later, you cannot prove what was allowed to run.

**Transist makes workflow execution inspectable, verifiable, and repeatable by default.** The workflow is a declarative artifact you author once and can sign; the runtime compiles it into a frozen recipe and executes only from that recipe. The default home posture is [governed](./reference/runtime-modes.md): trust and exact policy authority are enforced before dispatch.

## What Sets Transist Apart

These are the capabilities other AI runtimes and agent frameworks generally do not offer, and cannot easily add without breaking their own architecture.

### 1. The workflow is data, not code

A Transist workflow is an explicit declarative definition. When a run is prepared, that definition is compiled into an immutable recipe snapshot; the runtime always executes from the snapshot. This is what makes the definition diffable, reviewable, signable, and independently attestable.

### 2. The runtime verifies trust; it never creates it

Signing, key generation, and approval live outside the runtime, in a separate administrative surface. The runtime imports trust roots, verifies detached signatures over the full authored artifact — definition, referenced subflows, prompt files, and script references — and refuses unsigned or modified executable inputs under `--trusted-execution`. Trust decisions are persisted as durable evidence on the run.

### 3. Plugins are content-immutable; runs pin their assets

A `<plugin, version>` is byte-immutable: reinstalling different bytes under the same version is rejected. Each run records asset locks (plugin digests, prompt-file digests) and refuses re-entry when anything drifts. Snapshot-backed fresh runs retain private captures of resolved prompt and shell `source://` assets and consume those captures for dispatch, preflight, cleanup, resume, and replay without requiring the authored workflow source directory. A run's executable surface is therefore identifiable, while external tool behavior remains nondeterministic.

### 4. The LLM is an untrusted participant

Model outputs and plugin results are data. They flow through the workflow, are validated, persisted, and audited. They never redefine kernel execution logic, extend the runtime, or open hidden loops. This is enforced by architecture — the kernel exposes no interface through which model output can alter control flow.

### 5. Determinism is a contract

The kernel's orchestration semantics are covered by dedicated contract-protecting tests. Prepared runs execute from the frozen recipe. The execution history is append-only. Failed runs enter a recoverable state the operator resolves explicitly. Retries scope artifacts and evidence per execution. Transist does not claim external tools are deterministic; it guarantees that the runtime layer around them is.

### 6. Data locality is enforceable, not just documented

`--local-only` refuses to dispatch any step whose declared provider is external, before execution begins. Provenance records the producing workflow and plugin identities, external transfers, and approver identity for gated decisions. Runtime-resolved secrets are never persisted in events, artifacts, evidence, metrics, or inspection exports.

### 7. The kernel is small on purpose

Provider-specific behavior, planning heuristics, autonomous loops, and vendor integrations live in plugins and adapters, behind a versioned public `adapter + plugin + action` boundary — never in the kernel. This is what keeps the trust, determinism, and immutability guarantees credible over time.

## Core Foundations

### Local-First Execution

Workflows run close to your context — your source code, local tools, and controlled environments. Execution and secrets stay under your control.

### Evidence Over Trust

Transist does not trust AI output because it looks plausible. Concrete evidence — logs, diffs, command outputs, structured tool responses — is preserved as part of the run, so every generated result is reviewable and audit-ready.

### Inspection & Recovery

Every run is a first-class object with an append-only execution history. When a workflow fails, you can inspect the exact step, see the error, and resume or replay with explicit lineage.

### Shared Visibility

Local execution does not mean isolated execution. A read-only companion server exposes structured inspection and Prometheus metrics; team-level aggregation lives above the single-runtime boundary and is optional.

### Explicit Control Boundaries

Risky actions are not autonomous. Manual gates record actor identity and reason; policy governs destructive operations, external writes, and costly execution. Approvals are part of the run record, not out-of-band chat messages.

## Get Started

- **[Quickstart](./guides/getting-started/quickstart.md)**: Install the CLI and run your first workflow in 5 minutes.
- **[Governed Runtime Setup and Delivery](./guides/trust/governed-runtime-setup-and-delivery.md)**: Set up regular controlled plugin and workflow delivery.
- **[Runtime Modes](./reference/runtime-modes.md)**: Choose governed, standalone, or development posture.
- **[Handbook](./handbook/README.md)**: Learn the mental model, runtime behavior, and extension boundaries.
- **[Workflow Schema](./reference/workflow-schema.md)**: Details of authoring `transist.flow.v1` files.

---

> Run AI-assisted workflows under control. Sign what you approve, verify what you run, inspect what happened, and recover when it did not finish cleanly.
