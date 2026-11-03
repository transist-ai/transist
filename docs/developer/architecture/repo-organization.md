# Repository Organization

This document serves as the normative guide for where code belongs in the Transist workspace. Following these rules ensures that our system remains modular, testable, and maintainable as it grows.

---

## Workspace Responsibilities

### `transist-protocol`

**The Foundation**: Owns the wire-level contracts and durable data models shared across the entire workspace.

- **Includes**: Protocol message models, recipe and runtime record models, and kernel-safe value representations.
- **Constraints**: Must not contain any workflow execution logic, storage implementations, or CLI/server behavior.

### `transist-core`

**The Brain**: Owns the runtime kernel behavior and the abstract interfaces (traits) that the kernel depends on.

- **Includes**: Run and step state models, execution scheduling, replays, resumes, manual gates, and narrow storage ports.
- **Execution phase structure**: `RuntimeKernel::execute_loaded_run` passes ownership through typed recovery, activation, preflight, scheduling, and finalization states. Invoke authorization, secret resolution, staging, promotion, and step completion are separate internal helpers. The decomposition must preserve persisted bytes, event ordering, dispatch order, and durability boundaries.
- **Constraints**: Must not know about CLI, HTTP, or concrete storage/adapter implementations.

### `transist-compiler`

**The Architect**: Owns the logic for loading, validating, and compiling YAML workflows into immutable recipes.

- **Includes**: Authored flow models, validation rules, and the logic for bundling prompts and skills.
- **Constraints**: Must not touch runtime execution or storage.

### `transist-plugins`

**The Toolroom**: Handles the discovery, manifest loading, validation, and installation of plugins.

- **Includes**: Plugin manifest schema and resolution logic.
- **Constraints**: Stays independent of runtime execution and transport protocols.

### `transist-trust-digest`

**The Hashing Boundary**: Owns canonical byte and named-entry hashing primitives.

- **Includes**: SHA-256 helpers, line-ending normalization, and deterministic entry aggregation.
- **Constraints**: Must not discover workflows or plugins, and must depend only on protocol-level data and cryptographic support.

### `transist-storage-local`

**The Archive**: Implements the filesystem-backed persistence model.

- **Includes**: Run record storage, artifact and evidence persistence, private invocation staging and promotion, leases, and local indexing for queries.
- **Constraints**: Only implements storage; it does not decide *what* to store (that’s the Kernel’s job).

### `transist-adapters`

**The Hands**: Implements the execution environments (Process, HTTP, MCP).

- **Includes**: Preflight checks, handshakes, request/response handling, and secret delivery helpers.
- **Constraints**: Does not own policy, storage layout, or high-level workflow execution.

### `transist-policy`

**The Guard**: Implements the security and trust rules used during execution.

- **Constraints**: Stays focused on policy enforcement; does not touch transport or storage details.

### `transist-cli` & `transist-server`

**The Faces**: The primary interfaces for humans and external systems.

- **Includes**: CLI flag parsing, terminal rendering, HTTP routing, and API mapping.
- **Constraints**: Must remain thin wrappers around the root `transist` library. No core domain logic belongs here.

### Root `transist` crate

**The Glue**: Provides a unified library surface that wires up the default local implementation of all the crates above.

- **Includes**: High-level APIs for starting runs and querying metrics.
- **Role**: This is what the CLI and Server depend on so they don't have to manually wire up the entire workspace.
- **Composition boundary**: The companion server receives inspection and metrics through the root facade contexts; it does not construct local query or storage implementations directly.
- **Distribution status**: Workspace Rust crates are unsupported implementation units, not supported distribution or extension surfaces. The supported distribution is the signed Transist binary archive.

---

## Where Should Your New Code Go?

| If you are adding... | Put it in... |
| :--- | :--- |
| A new message field or data type | `transist-protocol` |
| A new state transition or recovery rule | `transist-core` |
| A new `transist.flow.v1` validation rule | `transist-compiler` |
| A new plugin manifest field | `transist-plugins` |
| A canonical digest primitive | `transist-trust-digest` |
| A new way to index or search local runs | `transist-storage-local` |
| Support for a new protocol or transport | `transist-adapters` |
| A new CLI command or output format | `transist-cli` |
| A new HTTP API endpoint | `transist-server` |

---

## The "Never" List

- **Do not** put kernel behavior in the root `transist` crate just because it's easier to access.
- **Do not** put CLI or HTTP request/response logic in lower-level crates.
- **Do not** put storage implementation details in `transist-core`.
- **Do not** parse plugin manifests, construct installed-plugin paths, or read
  prompt files in `transist-core`; use its plugin metadata and prompt-file
  ports and keep those operations in composition/plugin-management code.
- **Do not** put adapter-specific logic in the general-purpose compiler.
- **Do not** use a process-global router cache; router instances belong to the
  application context, use canonical absolute home keys, and are bounded to
  the documented cache capacity.

By respecting these boundaries, we ensure that Transist remains a clean, professional, and sustainable codebase.
