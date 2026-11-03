# Reference Documentation

These reference pages provide exact technical details for workflow authors, operators, platform engineers, and plugin authors working with a Transist environment. They are designed for fast lookup and are strictly aligned with the current implementation.

## Core Contracts

- [**Compatibility and Stability**](compatibility-and-stability.md): The supported v1 product contract and how we handle changes.
- [**Supported Platforms**](supported-platforms.md): The exact release target triples, archive contents, and unsupported targets.
- [**Dependency Update Policy**](dependency-update-policy.md): How automated dependency updates are reviewed, validated, and kept within the MSRV and security guardrails.
- [**Error Taxonomy**](error-taxonomy.md): A guide to the error codes and categories used by the Transist kernel.
- [**Known Limitations**](known-limitations.md): Current release, platform, and runtime constraints.
- [**Runtime Modes**](runtime-modes.md): Choose governed, standalone, or development posture and understand the related controls.

## Schemas and Manifests

- [**Workflow Schema**](workflow-schema.md): The full technical reference for the `transist.flow.v1` YAML format.
- [**Path and Resource Semantics**](path-and-resource-semantics.md): Which URI-like values are runtime capability resources, action conventions, package assets, or manifest-only declarations.
- [**Network Policy**](network-policy.md): The declarative and enforced runtime contract for `policy.network`.
- [**Plugin Manifest**](plugin-manifest.md): How to structure your `plugin.yaml` files.
- [**Plugin Support Matrix**](plugin-support.md): A detailed list of first-party plugins and their capabilities.

## Tools and APIs

- [**CLI Command Reference**](cli.md): A complete list of all `transist` and `transist-server` commands.
- [**Companion Server API**](companion-server-api.md): The full HTTP contract for the read-only inspection server.
- [**Metrics Reference**](metrics.md): Details on every Prometheus metric exported by Transist.
- [**TEP (Transist Extension Protocol)**](tep.md): How the kernel communicates with adapters.
- [**APP (Adapter Plugin Protocol)**](app.md): The exact rules for how adapter-provided tools must behave.

## Release and Distribution

- [**Release Artifact Verification**](release-artifact-verification.md): How to verify checksums, signatures, SBOMs, and attestations for downloaded releases.
- [**Reproducibility**](reproducibility.md): How release artifacts are rebuilt and compared.
- [**Platform Distribution Signing**](platform-distribution-signing.md): The signing posture for supported distribution channels.
- [**Release Notes: 0.1.0**](../releases/0.1.0.md): Initial public beta capabilities, installation, and limitations.

## Architecture and Internals

- [**Storage Layout**](storage-layout.md): A deep dive into how data is organized in the effective Transist home.

---

### Finding the Right Information

If you are looking for a conceptual overview or a step-by-step tutorial, check out our [**Guides**](../guides/README.md) or the [**Handbook**](../handbook/README.md). For a quick look-up of terms, see the [**Glossary**](../../GLOSSARY.md).
