# Changelog

## [Unreleased]

- Report process stdout closure before the APP handshake response as a transport
  failure rather than a timeout, in both live preflight and invocation diagnostics.
  Handshake deadline expiry remains a timeout; stdout read errors are transport failures.
- Correct external-transfer evidence SHA-256 references to match the exact JSON
  bytes persisted by the artifact store. Historical records are not rewritten.
- Capture actual adapter invocation start and finish timestamps independently of
  monotonic elapsed duration, including failure and cancellation results. Existing
  invocation records retain their original timestamps.

## [0.1.1] — Bounded Loop Routing Fix

- Newly compiled bounded loops take their normal successor at the activation-local
  `max_iters` cap instead of silently skipping it. Approval guards still run and
  must succeed before parent publication; exhaustion never implies approval.
- Recipes explicitly pin loop-routing version 1. Prepared unmarked recipes,
  including their resume and replay paths, retain legacy routing. Adoption requires
  newly compiled recipes and new runs; preserve matching runtime/home pairs and
  never strip routing metadata or downgrade marked homes.
- Marked loop guards use null for a declared body's initial, reset or suspended
  business output, allowing iteration startup/re-entry without reusing old results
  or approvals. Truly unknown references and malformed predicates remain fail-closed.
- Prospective validation, explanations and inspection include cap-inclusive exits
  and check downstream authority, schemas and prerequisites before run birth.
- Updated workspace dependencies and refreshed third-party notices.

Runtime, admin, and server use `0.1.1`. Bundled workflows, modules, plugins, and
exact catalog pins remain `0.1.0`; schema and protocol identifiers remain `v1`.
See the [release notes](docs/public/releases/0.1.1.md) for adoption and recovery.

## [0.1.0] — Initial Public Beta

Transist is a governed local execution runtime for explicit AI and automation
workflows. This initial public release provides:

- Declarative workflows, typed step outputs, reusable modules, managed documents,
  and human review bound to exact document revisions.
- Immutable run recipes, durable local execution history, and explicit retry,
  resume, replay, cancellation, and terminal cleanup.
- Process, HTTP, and MCP adapters behind versioned plugin/action boundaries.
- Governed signature verification, explicit capability policy, secret references,
  and local-only execution; trust creation stays in `transist-admin`.
- CLI and terminal inspection, a read-only companion HTTP API, and Prometheus metrics.
- Signed binary evaluation bundles for six Linux and macOS targets, with a bundled
  shell plugin, offline quickstart, and demo workflows.

Runtime, bundled package releases, and exact pins use `0.1.0`; runtime-owned
schema and protocol identifiers use `v1`. Transist is pre-1.0: no surface is
promised stable or production-ready. Windows, distributed scheduling, and a
public Rust library distribution are not supported.

See the [release notes](docs/public/releases/0.1.0.md),
[compatibility policy](docs/public/reference/compatibility-and-stability.md), and
[known limitations](docs/public/reference/known-limitations.md).
