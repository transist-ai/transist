# The Companion HTTP Server

The CLI is the primary way to control Transist. The **Companion Server** (`transist-server`) is the local read-only surface for viewing and integrating run data.

## What is it for?

The server provides a bridge between Transist's local files and inspection tools. Common uses include:

- **Web dashboards:** View run history in a browser.
- **IDE integrations:** See run status directly in your code editor.
- **Monitoring:** Pull metrics into tools like Prometheus or Grafana.
- **Automated scripts:** Read run data in a structured JSON format.

## It's Read-Only (by design)

The Companion Server is for **inspection only**. You cannot use it to start a workflow, approve a gate, or change data. All control remains strictly in the CLI.

The companion server exposes runtime state. It should not become the place where core execution semantics are redefined.

## Key Features

- **API Access:** Get JSON summaries of runs, steps, events, artifacts, and evidence.
- **Built-in UI:** Browse runs and workflows at `/inspect`.
- **Metrics:** Expose Prometheus-compatible metrics.
- **Workflow Graphs:** Visualize workflow logic such as loops, branches, and subflows.

The `/api/v1` JSON surface is host-independent: it does not return absolute
local filesystem paths. Workflow files expose opaque resource IDs and
workspace-relative `relative_path` labels; retrieve content through the
resource links.

Token and cost metrics appear only when adapters or plugins report usage. Transist does not invent usage data.

## Security and Guardrails

By default, the server only listens to your local machine (`127.0.0.1`). It is not meant to be exposed directly to the public internet. If you need to share access with a teammate, we recommend using a secure tunnel or a VPN.

---

### Related

- [Using the Companion Inspection UI Guide](../../guides/inspection/using-the-companion-inspection-ui.md)
- [Companion HTTP Server in Practice Guide](../../guides/inspection/companion-http-server-in-practice.md)
- [Companion Server API Reference](../../reference/companion-server-api.md)

**Navigation:**

- Previous: [The Storage Model](03-storage-model.md)
- Next: [Operating Transist Locally](../05-admin-and-operations/01-operating-transist-locally.md)
