# TEP: The Transist Execution Protocol

TEP is the "lingua franca" spoken between the Transist Kernel and its Adapters. It ensures that no matter how complex or exotic a plugin's underlying technology is, the Kernel sees a consistent, predictable interface for execution.

## Why TEP Matters

If you are a workflow author, you’ll never have to write a TEP frame by hand. However, understanding TEP is crucial for understanding how Transist guarantees reliability:

- **Universal Interface**: TEP allows the Kernel to say "Execute this action" and receive a response without knowing if that action is happening in a local Bash script, a remote Python API, or an MCP server.
- **Identity & Traceability**: Every TEP request carries a unique `execution_id`. This ID is the anchor for all logs, artifacts, and evidence produced during that step.
- **Predictable Outcomes**: TEP defines exactly what a "Success," "Failure," or "Error" looks like. This standardization allows the Kernel to handle retries and recovery logic uniformly across all plugins.

## The Protocol Boundary

TEP sits at the boundary between **Orchestration** (the Kernel) and **Execution** (the Adapter).

1. **Request**: The Kernel sends a TEP request containing the plugin name, action name, input arguments, and any scoped secrets.
2. **Execution**: The Adapter translates this TEP request into a format the specific plugin understands (e.g., command line arguments or a JSON-RPC call).
3. **Response**: The Adapter collects the results, cleans them up, and sends back a TEP response containing the status, output data, and pointers to any saved artifacts.

## Key Concepts in TEP

- **Invoke Paths**: TEP uses a structured path to identify exactly which plugin and action to trigger.
- **Canonical Results**: Every result is wrapped in a standard JSON envelope, making it easy for subsequent steps in a workflow to parse and use the data.
- **Error Taxonomy**: TEP categorizes errors (e.g., `plugin_error`, `adapter_error`, `kernel_error`) so the system knows whether to retry automatically or stop and ask for help.

---

## Related

- [Adapter Responsibilities](../../public/handbook/06-architecture-and-extension/04-adapter-responsibilities.md)
- [TEP Technical Reference](../../public/reference/tep.md)
- [APP: Adapter Plugin Protocol](02-app.md)

---

- Previous: [Adapter Responsibilities](../../public/handbook/06-architecture-and-extension/04-adapter-responsibilities.md)
- Next: [APP](02-app.md)
