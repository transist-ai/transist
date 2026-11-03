# Adapters and Plugins

While "Adapter" and "Plugin" might sound similar, they perform very different roles in a Transist installation. Understanding this separation is key to building and using tools effectively.

## The Difference

### 1. Adapters (the how)

An **adapter** is responsible for the mechanism of execution. It does not know what an issue ticket is or how to read a file; it knows how to talk to a specific kind of environment.

- **Job:** Manage transport, cancellation, secret delivery posture, and translation of results into Transist records.
- **Current adapter kinds:** `process`, `http`, and `mcp`.

### 2. Plugins (the what)

A **plugin** is a versioned package that contains the tool-specific integration
contract. It defines what actions are available, what input/output schemas they
use, and where the adapter can reach the execution participant.

The name does **not** mean a small module loaded into the Transist process. A
plugin can front a single script, a compiled program, a CLI wrapper, a
dispatcher over a set of scripts, an MCP server, or a separately deployed web
service. That software can be complex. Transist cares that its exposed actions
conform to the declared adapter boundary, not how many files, processes, or
internal components implement them.

- **Job:** Provide `plugin.yaml`, schemas, bindings, and the entrypoint that performs the action.
- **Examples:** `shell` in this repository, and external plugin packages such as GitHub, issue-tracker, or Codex-style CLI wrappers when installed.

The installed package and the implementation are sometimes the same thing and
sometimes not:

- A `process` or `mcp` package includes a local entrypoint plus all selected
  runtime files required to start it.
- An `http` package describes a service that is deployed and operated
  separately. The package pins the integration contract, not the service's
  entire implementation.

In all cases the installed package is finite, versioned, and content-immutable.
The execution participant remains outside the kernel.

## The Execution Identity

In a workflow, you specify these three together to define exactly what should happen:

- **`adapter`**: How should we run it?
- **`plugin`**: Which installed package should we use?
- **`action`**: Which specific command should we run?

## How they work together at Runtime

The Transist **Kernel** never talks to a tool directly. Instead, it follows this sequence:

1. The kernel dispatches an action request to the chosen adapter.
2. The adapter prepares the plugin entrypoint or remote call.
3. The adapter sends a request using the appropriate transport.
4. The plugin execution participant does its work and returns a result.
5. The adapter validates output, then normalizes artifacts, evidence, output, usage, and errors for the kernel.

The manifest selects exactly one adapter kind. The participant's business logic
may be reusable across transports, but changing the plugin's adapter is a
contract change.

## TEP and the runtime boundary

TEP and APP let Transist treat external systems as controlled execution
participants without baking their behavior into the runtime kernel. TEP covers
the kernel-to-adapter boundary. APP covers the adapter-to-plugin boundary for
`process` and `http`; the `mcp` adapter maps the supported MCP tools lifecycle
into APP semantics.

That boundary covers request/response shape, transport assumptions, handshake, cancellation, error taxonomy, artifact/evidence normalization, and secret reference resolution. It is how Transist can call agents, CLIs, APIs, MCP servers, and internal tools without turning the kernel into a vendor-specific integration layer.

The boundary also enforces action schemas. Materialized input is checked before
transport work begins, and successful output is checked before hints or result
data cross into kernel state.

---

### Related

- [MCP Integration in Practice Guide](../../guides/plugins/mcp-integration-in-practice.md)
- [Plugin Manifest Reference](../../reference/plugin-manifest.md)
- [Plugin Support Reference](../../reference/plugin-support.md)

**Navigation:**

- Next: [Plugin Authoring](03-plugin-authoring.md)
