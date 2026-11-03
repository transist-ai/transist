# Roadmap

## Public Baseline

Transist `0.1.0` is a public beta for governed local workflow execution, with
versioned workflow/plugin contracts, durable run history, explicit recovery,
human review, and local inspection. See the
[release notes](docs/public/releases/0.1.0.md) for current capabilities.

## Priorities

- Improve workflow and plugin authoring with editor JSON Schema validation.
- Broaden deterministic coverage of kernel-contract behaviors and recovery.
- Refine operator diagnostics and onboarding from reproducible user feedback.

These priorities are directional, not delivery dates or support commitments.
Propose improvements through [Issues](https://github.com/transist-ai/transist/issues).

## Stability Horizon: 0.1.0 to 1.0

- Establish stable run-state, inspection-snapshot, and execution-protocol contracts.
- Stabilize the public CLI command and flag surface.
- Preserve explicit trust, authority, side-effect, and recovery boundaries.

Versioned public surfaces are protected within the current minor line. Minor
releases may introduce documented breaking changes; stability begins only at
`1.0.0`. See [Compatibility and Stability](docs/public/reference/compatibility-and-stability.md).
