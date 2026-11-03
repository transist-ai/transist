# Trust and Security Guides

These guides cover the core features of Transist that help workflow authors and operators make workflow execution more secure, auditable, and trustworthy.

Trust creation is handled by the OSS `transist-admin` CLI that ships inside this
monorepo. Use `transist-admin` for key generation, public-key export, signing,
verification dry-runs, and signature inspection. Use the guides here for
runtime-side governance, secrets, policy, auditability, and trusted-execution
enforcement.

- [**Managing Secrets and Runtime Policy**](secrets-and-runtime-policy-in-practice.md)
    A hands-on look at how Transist handles secrets, how policy controls their delivery, and what raw secret data is redacted from Transist-managed persisted records.

- [**Governed Runtime Setup and Delivery**](governed-runtime-setup-and-delivery.md)
    The end-to-end operator path: create trust outside the runtime, sign plugins and immutable workflow packages, install the package, check prospective readiness, authorize exact policy grants when needed, recheck, and run a prepared trusted workflow.

- [**Trusted Governed Execution Walkthrough**](trusted-execution-walkthrough.md)
    A step-by-step offline walkthrough of the full signed trust loop: import a demo trust root, install signed artifacts with `--trusted-execution`, run with local-only enforcement, cancel, retry, inspect, and export evidence. Verified automatically by `scripts/ci/run_quickstart_smoke.sh`.

- [**Determinism, Auditability, and Governance**](determinism-auditability-and-governance-by-example.md)
    An exploration of how Transist's design helps create workflows with repeatable runtime structure, durable evidence, and reviewable governance boundaries.
