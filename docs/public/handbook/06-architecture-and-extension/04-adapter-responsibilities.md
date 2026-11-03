# Adapter Responsibilities

If Plugins are the "what" (the logic and tools), Adapters are the "how" (the execution environment). Adapters bridge the gap between Transist's deterministic kernel and the messy, unpredictable world of external processes, APIs, and model servers.

## The Role of an Adapter

Adapters aren't just wrappers; they are the enforcement point for Transist's safety and reliability guarantees. Every time a workflow step executes, an adapter is responsible for:

- **Validation**: Ensuring that the requested plugin and action actually exist and are compatible with the execution environment.
- **Schema Validation**: Compiling action schemas during plugin validation,
  checking materialized input before dispatch, and checking successful output
  before converting hints or result data.
- **Just-in-Time Secret Delivery**: Injecting sensitive credentials (like API keys) only at the moment of execution, and only if allowed by the kernel's policy.
- **Result Canonicalization**: Taking whatever output a plugin produces (JSON, raw text, exit codes) and translating it into a standard Transist `result.json`.
- **Artifact & Evidence Persistence**: Writing the execution's trail of breadcrumbs (logs, files, screenshots) to the safe-scopable `execution_NNN` directory.
- **Cancellation**: Gracefully (or forcefully) stopping the work if the user or the kernel interrupts the run.

## Supported Adapter Kinds

### Process Adapter (`process`)

The workhorse for local operations. It runs shell commands, scripts, and compiled binaries on the same machine as the Transist kernel. It’s the most direct way to integrate CLI tools or custom Python/Bash wrappers.

### HTTP Adapter (`http`)

Connects to remote services over `http://` or `https://`. This is how Transist talks to webhooks, external APIs, or specialized tool-servers running in the cloud.
> **Note on Security**: Secret delivery to HTTP endpoints is strictly governed by policy. Transist assumes HTTP boundaries are less secure than local ones and requires explicit trust configuration for remote secret injection.
>
> For HTTPS plugins, `entrypoint.ca_cert` adds a plugin-local trust root only.
> The adapter still validates the certificate chain, expiry, hostname, and
> signatures for handshake, health, invoke, redirect, and cancellation requests.
>
> The host adapter also enforces HTTP network policy for every request phase.
> `none` opens no connection, `loopback` validates every DNS result, and
> `allowlist` checks scheme, normalized host, effective port, and TLS settings.
> Restricted modes disable system/environment proxies. Cross-authority
> redirects are denied by default; an explicitly allowed cross-authority
> redirect strips action secret headers and body fields.

### MCP Adapter (`mcp`)

Specialized for the **Model Context Protocol**. In the current version, this adapter handles local MCP servers. It translates between the Transist execution model and the MCP JSON-RPC protocol, allowing any MCP-compatible tool to be used as a Transist plugin without modification.

## Canonicalization: The Truth-Maker

Plugins can be "chatty," returning extra data or non-standard formats. The Adapter's job is to extract the truth. It filters the noise, redacts any accidentally leaked secrets, and produces the final, immutable record of what happened. This ensures that the Kernel never has to worry about the underlying transport—it only ever sees clean, canonical results.

An output that does not match the action's JSON Schema is rejected with
`validation_error`; its value and hints do not enter the canonical result.

---

## Related

- [Plugin Authoring](03-plugin-authoring.md)
- [TEP: Transist Execution Protocol](../../reference/tep.md)
- [APP: Adapter Plugin Protocol](../../reference/app.md)
- [Trust & Policy](../../guides/trust/secrets-and-runtime-policy-in-practice.md)

---

- Previous: [Plugin Authoring](03-plugin-authoring.md)
- Next: [TEP](../07-protocols-and-internals/01-tep.md)
