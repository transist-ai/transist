# Transist

[![Mainline Validation](https://github.com/transist-ai/transist/actions/workflows/main-ci.yml/badge.svg?branch=main)](https://github.com/transist-ai/transist/actions/workflows/main-ci.yml)
[![Security Scan](https://github.com/transist-ai/transist/actions/workflows/security.yml/badge.svg?branch=main)](https://github.com/transist-ai/transist/actions/workflows/security.yml)
[![Release](https://github.com/transist-ai/transist/actions/workflows/release.yml/badge.svg?event=workflow_dispatch)](https://github.com/transist-ai/transist/actions/workflows/release.yml?query=event%3Aworkflow_dispatch)
[![MSRV](https://img.shields.io/badge/MSRV-1.88.0-blue?logo=rust)](rust-toolchain.toml)
[![Docs](https://img.shields.io/badge/docs-transist.ai-blue)](https://transist.ai/docs)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL%202.0-blue.svg)](LICENSE)

Transist is a governed local execution runtime for inspectable, recoverable AI
and automation workflows on devices, machines, and edge infrastructure.

Define workflows once. Run agents, CLIs, APIs, MCP servers, and internal tools as controlled execution participants. Keep inputs, outputs, artifacts, approvals, failures, and evidence visible across every run.

Think of it as the runtime boundary around useful but messy work. Agents and tools perform work. Transist governs the workflow run.

This README is for technical evaluators, workflow authors, operators, platform engineers, and technical decision-makers who need to understand whether Transist fits their team infrastructure.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the workspace layout, trust boundary,
and dependency rules.

## What is Transist for?

Use Transist when asking a tool or agent to do the work is not enough. You also need to know what was allowed to happen, what actually happened, and how to recover when it did not finish cleanly.

Transist is for workflows that need to be:

- **Inspectable:** Easily see what ran, what the inputs were, and what happened at every step.
- **Auditable:** Keep a durable record of all actions, artifacts, and evidence for compliance or review.
- **Recoverable:** Resume failed runs or replay them from a known state without losing context.
- **Extensible:** Add new tools and integrations without changing the core engine.
- **Deterministic:** The kernel orchestrates workflows predictably, even if the tools themselves are not.

## How is it different?

Most AI runtimes and agent frameworks are task-driven: a user gives a prompt, an agent decides the next action, tools are used, and an output appears. The workflow is written at runtime, often as executable code, and the definition and the execution logic are the same object.

Transist is run-driven and definition-first. A workflow is authored as an explicit declarative artifact. Preparing a run compiles that artifact into an immutable recipe snapshot; the runtime always executes from the snapshot, never from a freshly reinterpreted definition. This is what makes the other guarantees possible.

Concretely, Transist differs on five points that other AI runtimes generally do not offer, and cannot easily add without breaking their own architecture:

- **The workflow is data, not code.** A declarative definition can be diffed, reviewed, signed, verified, and stored as authoritative execution intent. A runtime whose workflow *is* a program written at runtime cannot make the same guarantees.
- **The runtime verifies trust; it never creates it.** Signing and key management live in a separate administrative surface. The runtime imports trust roots, verifies detached signatures over the full authored artifact, and refuses unsigned or modified executable inputs under `--trusted-execution`.
- **Plugins are content-immutable, runs pin their assets.** A `<plugin, version>` is byte-immutable. Each run persists asset locks (plugin digests, prompt-file digests) and rejects re-entry when anything drifts.
- **Plugin temporary state is declarative and private.** Process and MCP packages
  use `runtime://temp` or `runtime://temp/<relative-path>` only in `entrypoint.env`; the runtime creates and
  sandbox-authorizes it for one invocation without persisting its host path.
- **The LLM is an untrusted participant.** Model outputs and plugin results are data — validated, persisted, audited. They never redefine kernel execution logic or open hidden loops.
- **Determinism is a contract.** The kernel's orchestration semantics are covered by dedicated tests. Prepared runs execute from the frozen recipe; the execution history is append-only; failed runs enter a recoverable state the operator resolves explicitly.

In practical terms:

- **Workflow logic stays explicit** in your declarative YAML files.
- **Execution history is durable, append-only, and inspectable** on your local disk; runtime state files are mutable projections of that history.
- **Tools are external** and integrated through a versioned public `adapter + plugin + action` boundary — never absorbed into the kernel.
- **A plugin is an integration contract, not necessarily a small module.** It
  can expose a script, CLI wrapper, substantial program, HTTP service, or MCP
  server, provided the execution participant conforms to the selected adapter
  protocol.
- **Failures are operational states**, not just terminal output.
- **Data locality is enforceable**, not just documented — `--local-only` refuses external providers before dispatch.

This approach keeps the core engine small and credible, and keeps the trust, immutability, and determinism guarantees credible over time.

## Trust Boundary

Transist does not create trust.

- `transist-admin` is the separate admin CLI in this repository that
  generates signing keys, exports public keys, signs workflow definitions,
  signs plugin packages, and performs verification dry-runs.
- `transist` remains the runtime that verifies and enforces those trust
  decisions.

This repository intentionally does not ship `sign`, `verify`, or `key`
generation commands on the runtime CLI. The runtime imports trust roots and can
enforce trusted execution, but it still never creates trust material.

## Where Transist sits

```text
Workflow definition
  -> Transist runtime kernel
  -> adapter + plugin boundary
  -> agent / CLI / API / MCP server / internal tool
  -> artifacts, evidence, events, gates, recovery state
```

Transist can call Cursor, Codex, Claude, Goose, MCP servers, GitHub Actions, issue trackers, documentation tools, shell scripts, or internal tools through adapters and plugins. It is not trying to replace those tools. It gives teams a predictable runtime boundary around them.

## What Transist is

- A deterministic workflow execution kernel with a contract-tested "recipe is king" guarantee.
- A local-first runtime that verifies cryptographic trust before execution in
  governed mode or when `--trusted-execution` is requested.
- A run recorder that keeps an append-only execution history plus artifacts, evidence, provenance, and asset locks on your disk.
- A versioned public extension surface (`adapter + plugin + action`) so tool-specific behavior never leaks into the core.
- An embeddable local execution layer that remains useful without a mandatory
  hosted service.

## What Transist is not

Transist is **not**:

- A coding agent, an autonomous planner, or an agent framework.
- An IDE, a chatbot, or an LLM abstraction layer.
- A cloud-only automation platform or hosted orchestration service.
- A replacement for existing coding assistants, MCP servers, CI systems, or internal tools.
- A general-purpose background job system, a device-management system, or a SIEM.
- A black-box agent session where model output can quietly redefine what runs.

## Core Principles

- **Deterministic Orchestration:** The kernel executes from an immutable recipe snapshot; orchestration semantics are covered by contract-protecting tests.
- **Trust Verified, Not Created:** Signing and key management live outside the runtime. The runtime verifies and enforces.
- **Content Immutability:** Plugins are byte-immutable at a given version; runs pin their assets and reject drift.
- **Untrusted Model Output:** LLM and plugin results are data — validated, persisted, audited. They never redefine execution logic.
- **Inspectable Records:** Runs are append-only and analyzable long after they finish.
- **Auditable Evidence:** Artifacts, evidence, provenance, and events are first-class citizens.
- **Honest Recovery:** Replay and resume are core features with explicit lineage, not afterthoughts.
- **Safe Extensibility:** Capabilities are added through plugins and adapters, never by expanding the kernel with provider-specific behavior.

## Current Capabilities

- Author workflows in explicit `transist.flow.v1` YAML; compile once to a frozen recipe per run.
- Use `if`, `loop`, manual gates, and subflows for complex logic without leaving the declarative surface.
- Verify detached signatures over the full authored artifact (definition, subflows, prompt files, script references) with `--trusted-execution`.
- Enforce data locality with `--local-only`, refusing any step whose declared provider is external before dispatch.
- Pin plugin and prompt assets per run and reject re-entry on drift.
- Persist append-only execution history, provenance, artifacts, evidence, and gate decisions locally.
- Inspect runs with a read-only companion server, Prometheus metrics, and local UI.
- Resume, retry, and replay from persisted state with explicit lineage.
- Extend Transist with your own plugins and adapters through a versioned public `adapter + plugin + action` boundary.

## Release Status

Transist `0.1.1` is a **public beta (pre-1.0)** patch release. Versioned public
surfaces are protected in the current minor line; stability begins only at
`1.0.0`. Minor releases may break documented surfaces with changelog,
release-note, and migration guidance. Workspace crates are implementation
units (`publish = false`); the only supported distribution is the signed
binary archive on the
[GitHub releases page](https://github.com/transist-ai/transist/releases). See
[Compatibility & Stability](docs/public/reference/compatibility-and-stability.md).
There is no supported public Rust library distribution; workspace crates are
implementation units for the source checkout.

See the [0.1.1 release note](docs/public/releases/0.1.1.md) for the loop-routing
fix and adoption guidance, and the [0.1.0 release note](docs/public/releases/0.1.0.md)
for initial capabilities and limitations. Runtime, admin, and server use `0.1.1`;
bundled packages and exact catalog pins remain `0.1.0`. Runtime-owned schema and
protocol contracts remain `v1`. Start in a fresh home
and preserve matching runtime, reviewed bundle, and home together for recovery.

## Install

Download and extract the archive for your platform from the
[GitHub releases page](https://github.com/transist-ai/transist/releases). From
the extracted archive root, install the binaries on Linux with:

```bash
sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
  bin/transist-sandbox-launcher /usr/local/bin/
```

On macOS (Apple Silicon or Intel), use:

```bash
sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
  /usr/local/bin/
```

The six supported artifact targets are:

- `x86_64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-gnu`
- `aarch64-unknown-linux-musl`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Debian, Alpine, and Ubuntu are build or validation environments, not archive
identities. See [Supported Platforms](docs/public/reference/supported-platforms.md)
for prerequisites. Windows is not supported.

Every archive is a minimal evaluation bundle containing the runtime binaries,
the bundled `shell` plugin, the offline quickstart fixture, and the
getting-started demo workflows. It contains `transist`, `transist-server`, and
`transist-admin`; Linux archives also contain the internal
`transist-sandbox-launcher`, which must remain beside `transist` when installed
this way. They also contain the complete Git-tracked `.agents/skills/` tree as
ancillary authoring guidance. Public and developer documentation is published
online at [transist.ai/docs](https://transist.ai/docs) and is not bundled.

Verify the install with:

```bash
transist --help
transist-server --help
transist-admin --help
```

See [Release Artifact Verification](docs/public/reference/release-artifact-verification.md)
to verify checksums, minisign signatures, the release manifest, SBOMs, and
GitHub attestations for downloaded releases. The committed release
verification key is `transist-release.minisign.pub` (key ID
`202B06AC561F2B56`); compare a downloaded copy byte-for-byte with the
committed file before trusting it. The key ID is only a human-readable
cross-check.

If you want to run the bundled example workflows and plugins from this
repository, keep the released binaries on your `PATH` and run the example
commands from the extracted release archive root (or a source checkout root)
that contains the referenced `workflows/demos/getting_started/` and
`plugins/shell/` paths.

## Build From Source

> **Repository resource:** Building from source requires the full repository
> checkout. Release archives are minimal evaluation bundles — built binaries,
> the bundled plugin, the quickstart fixture, and getting-started demos — and
> do not include the Rust source or the repository documentation.

Rust `1.88.0` is the repository build pin and minimum supported Rust version
(MSRV). It remains authoritative until an explicit policy change; the checked-in
`rust-toolchain.toml` selects it for local builds and CI.

```bash
cargo +1.88.0 build --workspace
cargo +1.88.0 test --workspace
```

Source-based entry points:

```bash
cargo +1.88.0 run -p transist-cli -- --help
cargo +1.88.0 run -p transist-server -- --help
cargo +1.88.0 run -p transist-admin -- --help
```

## Getting Started

The best way to start is with the [Quickstart Guide](docs/public/guides/getting-started/quickstart.md).

From an extracted release archive or a source checkout that includes `plugins/` and `examples/`, run a minimal standalone setup:

Install Python 3 for the bundled shell plugin. Linux also requires bubblewrap
and usable user namespaces; keep the packaged sandbox launcher beside the runtime.
See [Installation and Local Setup](docs/public/guides/getting-started/installation-and-local-setup.md).

```bash
# Export an isolated home and initialize it in standalone mode
export TRANSIST_HOME="$(mktemp -d -t transist-quickstart-XXXXXX)"
trap 'rm -rf "$TRANSIST_HOME"' EXIT
transist home init --mode standalone --home "$TRANSIST_HOME"

# Install the bundled shell plugin (unsigned; standalone accepts without trust)
transist plugins install --path plugins/shell/plugin.yaml

# Install a workflow, run it, and inspect the record
transist workflows install --path examples/quickstart/quickstart.workflow.yaml
transist workflows run quickstart.demo@0.1.0 --run-ref qs --input subject=demo --ui plain
transist runs show qs
```

`--mode standalone` creates a home with permissive policy — no capability
grants or trust roots are required. Use `--mode governed` (the default) when
you need signed trusted execution and explicit allowlist grants.

For the full offline signed trust loop (trust import, governed home, cancel,
retry, evidence export), see the
[Trusted Governed Execution Walkthrough](docs/public/guides/trust/trusted-execution-walkthrough.md).

Transist stores all its data under a per-user default home
(`${XDG_STATE_HOME:-$HOME/.local/state}/transist`) by default. You can change
this with `transist home set <path>`, the `TRANSIST_HOME` environment
variable, or the `--home <path>` flag.

## Where to Go Next

Installed `workflows check <id>@<version>` and authorization use fast validation
without plugin probes. Passing validation leaves readiness `not_ready`; use
`check --live` or `--output readiness.json` for live readiness. Preparation and
execution still enforce current authority and live checks before run birth.

- **To evaluate Transist:** Start with the [Quickstart](docs/public/guides/getting-started/quickstart.md), then explore the [Getting Started Demos](workflows/demos/getting_started/README.md).
- **To automate your first workflow:** Follow the [Quickstart](docs/public/guides/getting-started/quickstart.md), then read [Build Your First Real Workflow](docs/public/guides/authoring/advanced-workflow-authoring.md) and the [Workflow Patterns Cookbook](docs/public/guides/authoring/workflow-patterns-cookbook.md).
- **To explore more examples:** Browse [transist-workflows](https://github.com/transist-ai/transist-workflows) for workflow examples (repository access required) and [transist-plugins](https://github.com/transist-ai/transist-plugins) for plugin integration examples. Review each repository's prerequisites and compatibility guidance before use.
- **To understand the runtime model:** Read [Artifacts, Evidence, and Events](docs/public/handbook/03-inspection-and-recovery/02-artifacts-evidence-and-events.md).
- **To compare Transist with agent automation:** Read [Transist vs Agent Automation](docs/public/transist-governed-execution-positioning.md).
- **To operate or administer Transist:** See the [Handbook](docs/public/handbook/README.md), [CLI Reference](docs/public/reference/cli.md), [Storage Layout](docs/public/reference/storage-layout.md), [Companion Server API](docs/public/reference/companion-server-api.md), and [Metrics Reference](docs/public/reference/metrics.md).
- **To create signing keys or signatures:** Use `transist-admin`; runtime
  verification still stays separate from trust creation.

## AI-Assisted Authoring

> **Repository resource:** The complete Git-tracked `.agents/skills/` tree is
> included in release archives and source checkouts as ancillary guidance.

If you use an assistant that supports repo-local skills, prefer the Transist skill set under `.agents/skills/`. Those skills are maintained alongside the Transist code and docs so the assistant stays closer to the current checked-out behavior.

- `transist-plugin-authoring` for `transist.plugin.v1` packages
- `transist-secrets-and-policy` for invoke-step `with.secret_refs`, policy,
  and secret-delivery boundaries
- `transist-observability-and-metrics` for `transist-server`, `/metrics`, and read-only inspection APIs
- `transist-runtime-setup` for safe installation and first-run onboarding

Start with [AI-Assisted Authoring With Repo Skills](docs/public/guides/authoring/ai-assisted-authoring-with-repo-skills.md)
when you want an AI agent to help author or repair real Transist workflows and
plugins.

## Documentation Map

- [Overview](docs/public/overview.md) for the public documentation entry point
- [Guides](docs/public/guides/README.md) for runnable workflows and task-oriented walkthroughs
- [Quickstart](docs/public/guides/getting-started/quickstart.md) for the standalone offline entry point
- [Compatibility & Stability](docs/public/reference/compatibility-and-stability.md) for public contract expectations
- [Supported Platforms](docs/public/reference/supported-platforms.md) for the exact release target matrix and archive contents
- [Dependency Update Policy](docs/public/reference/dependency-update-policy.md) for automated dependency review and MSRV enforcement
- [Threat Model](docs/public/security/threat-model.md) for the standalone security posture
- [Release Artifact Verification](docs/public/reference/release-artifact-verification.md) for downloaded release archives
- [Platform Distribution Signing](docs/public/reference/platform-distribution-signing.md) for the release-signing posture
- [Release Environment Configuration](docs/developer/release-environment.md) for the GitHub environment and tag protection ruleset
- [Developer Docs](docs/developer/architecture/kernel-contract.md) for runtime and protocol internals
- [Vision](VISION.md) for deeper product framing and kernel boundary decisions
- [Roadmap](ROADMAP.md) for planned product and platform work

## License & Support

Transist is licensed under [MPL-2.0](LICENSE). See [SUPPORT.md](SUPPORT.md)
for support scope and channels.

Repository policies: [CONTRIBUTING.md](CONTRIBUTING.md) · [MAINTAINERS.md](MAINTAINERS.md) · [SECURITY.md](SECURITY.md) · [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## Known Limitations

See [Known Limitations](docs/public/reference/known-limitations.md) for release-time
test-surface constraints and tracked platform/runtime limits.
