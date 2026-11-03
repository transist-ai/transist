# Choosing a Plugin Adapter

When you create a Transist plugin, you must choose an **adapter**. The adapter defines how Transist communicates with your plugin's code. This guide explains the three main adapter types—`process`, `http`, and `mcp`—to help you choose the right one for your plugin.

This guide is for plugin authors, integration engineers, and platform engineers authoring approved plugins.

## The Three Adapter Types

Transist provides three primary adapters for different integration scenarios.

### 1. `process` Adapter

**Use Case:** Wrapping an existing command-line tool or script.

The `process` adapter is the simplest and most common. It runs your plugin as a standard OS process, communicating with it via `stdin`, `stdout`, `stderr`, and environment variables.

**Choose `process` when:**

- You have an existing CLI tool you want to integrate (e.g., `git`, `docker`, `curl`).
- Your plugin is a simple script (Python, Shell, etc.).
- You want the most straightforward integration path.

Most local CLI-style plugins, including the bundled `shell` plugin, use the `process` adapter. Secrets referenced via `with.secret_refs` can be delivered to the process according to policy.

### 2. `http` Adapter

**Use Case:** Integrating with a standalone web service.

The `http` adapter communicates with your plugin over HTTP. It expects your plugin to be a web server that exposes specific endpoints (`/app/handshake` and `/app/invoke`) defined by the Adapter Plugin Protocol (APP).

**Choose `http` when:**

- Your plugin is already a long-running service.
- You want to create a strong isolation boundary between Transist and your plugin's code.
- Your plugin needs to manage its own internal state across multiple calls.

**Note:** By default, secrets are *not* passed to `http` plugins for security reasons. This can be overridden with a policy change, but `process` is the recommended path for tools that need secrets.

### 3. `mcp` Adapter

**Use Case:** Integrating with a service that speaks the Model Context Protocol (MCP).

MCP is a JSON-RPC protocol designed for services that expose multiple tools over a single connection. The `mcp` adapter implements the MCP `2025-06-18` tools subset over stdio, while keeping the workflow run, artifacts, evidence, and policy boundary in Transist.

**Choose `mcp` when:**

- You are integrating with an existing MCP-compliant server.
- You are building a complex plugin that provides many different functions and want to manage them as "tools" under a single long-running process.

Like the `http` adapter, secrets are not passed to `mcp` plugins by default.

#### Stdio Protocol Contract

The MCP server communicates using compact UTF-8 JSON-RPC 2.0 objects, one object
per `stdin` or `stdout` line. LF and CRLF delimiters are accepted. Each message
is limited to 1 MiB excluding its line delimiter; blank, malformed, non-UTF-8,
unterminated, and oversized messages are rejected. Do not use `Content-Length`
framing.

Transist sends `initialize` with protocol version `2025-06-18`, empty client
capabilities, and its name and version. The server must respond with the exact
request ID, the same protocol version, and a declared `tools` capability.
Transist then sends `notifications/initialized`, follows paginated `tools/list`
responses until `nextCursor` is absent, and invokes the manifest-bound tool with
`tools/call`. Request IDs are unique and responses must match the active request
ID. The server may send notifications while Transist waits; Transist ignores
valid notifications, answers `ping`, and returns JSON-RPC `-32601` for other
server requests.

`tools/call` must return a standard `CallToolResult`. `structuredContent`, when
present, becomes the action output. Otherwise the action output is
`{"content": [...]}`. Ordered text content blocks are concatenated into stdout;
non-text blocks remain only in the action output. The mapped output must satisfy
the action's output schema. `isError: true` maps to `tool_execution_error`, and
JSON-RPC or protocol failures map to `protocol_error`. MCP artifacts, evidence,
metrics, and usage are not inferred from tool responses.

Only stdio tools are supported. Resources, prompts, sampling, roots,
elicitation, Streamable HTTP, and provider-specific MCP behavior are not
implemented or claimed. On a timeout or cancellation of an active tool call,
Transist best-effort sends `notifications/cancelled` with the request ID, closes
stdin, and terminates the server process group.

## Decision Summary

| If your plugin is...                                | Then choose... | Why?                                                                |
| --------------------------------------------------- | -------------- | ------------------------------------------------------------------- |
| A standard CLI tool or a simple script              | **`process`**  | It's the simplest way to wrap an executable.                        |
| A standalone web service or a long-running process  | **`http`**     | Provides clear network-based separation.                            |
| A complex service exposing multiple "tools"         | **`mcp`**      | Designed specifically for multi-capability services.                |

## Debugging Tips

- **`process`:** Check the `stdout.txt` and `stderr.txt` artifacts created by the run. The `exit_code.txt` file will tell you if the process terminated successfully.
- **`http`:** Ensure your service is running and that it correctly implements the `/app/handshake` and `/app/invoke` endpoints.
- **`mcp`:** Verify the `initialize`, `notifications/initialized`, paginated `tools/list`, and `tools/call` lifecycle with newline-delimited JSON-RPC.

For any adapter, you can get more insight into a failed step by using the `transist runs show <run_id>` and `transist runs tail <run_id>` commands.
