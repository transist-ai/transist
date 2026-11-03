# Working with Plugins

In Transist, a **plugin is a versioned integration package**, not necessarily a
small in-process extension. It describes an execution participant's identity,
actions, schemas, and communication entrypoint. The participant behind that
contract can be a shell or Python script, a compiled CLI wrapper, a program
that dispatches to several scripts, a standalone web service, or an MCP
server. Its implementation can be small or substantial and can use any
language or framework.

What makes that software a Transist plugin is the boundary it implements:

- the installed `transist.plugin.v1` manifest and action schemas define the
  stable package contract;
- the selected adapter defines how Transist reaches the participant; and
- the participant speaks APP over process or HTTP, or the supported MCP tools
  protocol that the MCP adapter maps into APP semantics.

For `process` and `mcp`, the package includes the entrypoint and every runtime
file selected by `package.files`. For `http`, the package can instead describe
a separately deployed service through its URL, schemas, and optional
certificate. The service's whole source tree does not need to be inside the
plugin package. In both cases, Transist installs and pins the finite package
contract, while the external software remains outside the kernel.

This section covers how to use and create these integration packages.

Process and MCP packages declare provider-owned temporary directories in
`entrypoint.env` with complete `runtime://temp` or
`runtime://temp/<relative-path>` values. These directories are invocation-only;
they are not workspace paths, artifact destinations, host filesystem grants, or
durable provider homes. Use declared secret bindings for credentials: redirecting
a provider's normal home, configuration, or data lookup means its host login
must not be relied on. This redirection is not complete same-user host read
isolation on macOS.

## Core Guides

- **[Write Your First Plugin](write-your-first-plugin.md)**
    Start here to build a local Python APP greeting plugin, define its schemas and manifest, install it, and invoke it from a workflow.

- **[Plugin Lifecycle Management](plugin-packaging-install-upgrade-removal.md)**
    Learn how to install, inspect, upgrade, and remove plugins using the `transist plugins` command.

- **[Choosing an Adapter: `process`, `http`, or `mcp`](mcp-integration-in-practice.md)**
    When you build a plugin, you must choose an "adapter." This guide explains the three main types and helps you pick the right one for your needs.

## Concepts and Reference

- [Plugin Authoring Handbook](../../handbook/06-architecture-and-extension/03-plugin-authoring.md) explains package concepts and extension boundaries.
- [Plugin Manifest Reference](../../reference/plugin-manifest.md) defines the exact package contract.
- [APP Reference](../../reference/app.md) defines the adapter-to-plugin protocol.

## Workflow Authoring

For a complete workflow tutorial using the bundled `shell` plugin, follow
[Build Your First Real Workflow](../authoring/advanced-workflow-authoring.md).
