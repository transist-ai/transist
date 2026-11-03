# APP: The Adapter Plugin Protocol

While **TEP** handles the conversation between the Kernel and the Adapter, **APP** handles the final mile: the conversation between the Adapter and the Plugin itself.

## Why a Second Protocol?

You might wonder why we don't just use one protocol for everything. The separation between TEP and APP is a key architectural choice that keeps Transist flexible:

- **Decoupling**: The Kernel doesn't need to know if a plugin is a Python script, a Node.js server, or a remote binary. It only speaks TEP.
- **Protocol Translation**: The Adapter acts as a translator. It takes the high-level request from the Kernel and turns it into the specific dialect (APP) that the plugin understands.
- **Encapsulation**: Transport details (like HTTP headers or shell exit codes) stay within the APP layer and never "leak" into the Kernel's logic.

## APP in Action: Different Dialects

APP isn't a single "wire format" like a JSON file. Instead, it’s a set of rules that Adapters follow when talking to plugins. Depending on the adapter, APP sounds a little different:

- **Process Plugins**: APP is delivered as newline-delimited JSON (NDJSON) over standard input (`stdin`) and output (`stdout`).
- **HTTP Plugins**: APP is delivered as standard POST requests to specific endpoints on the plugin server.
- **MCP Plugins**: APP is mapped directly onto the native **Model Context Protocol** primitives. Transist actions become MCP tool calls.

## Responsibilities of APP

When a plugin "speaks APP," it is agreeing to:

1. **Accept Arguments**: Receive structured inputs from the adapter.
2. **Handle Context**: Understand the execution environment (like the current working directory).
3. **Emit Evidence**: Stream logs or status updates back to the adapter in real-time.
4. **Return a Result**: Provide a final JSON object indicating success or failure, along with any output data.

By following the APP pattern, approved plugin authors can package plugins in different languages and supported transports while preserving the kernel boundary. Compatibility is governed by the APP contract and the support terms for the plugin package.

---

## Related

- [TEP: Transist Execution Protocol](01-tep.md)
- [Adapter Responsibilities](../../public/handbook/06-architecture-and-extension/04-adapter-responsibilities.md)
- [APP Technical Reference](../../public/reference/app.md)

---

- Previous: [TEP](01-tep.md)
- Next: [Internals And Boundaries](03-internals-and-boundaries.md)
