# Guide: Using the Companion Inspection UI

In addition to its JSON API, the `transist-server` also hosts a simple, read-only web interface for inspecting workflows and runs. It's a great way to get a visual overview of your automations without leaving your browser.

The UI is server-rendered HTML. It is a read-only view of the data from the companion server's API; it does not read from the Transist home directory directly.

## 1. Start the Companion Server

If it's not already running, start the server from the workspace you want to inspect. The server resolves the Transist home the same way the CLI does: `--home` flag, then `TRANSIST_HOME`, then the user-set default, then the per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`).

```bash
transist-server --port 7777
```

## 2. Open the UI in Your Browser

Navigate to the following URL in your web browser:
**[http://127.0.0.1:7777/inspect](http://127.0.0.1:7777/inspect)**

You will be redirected to the main "Runs" page.

## 3. Browse and Inspect Runs

The "Runs" section is your primary dashboard. From here, you can:

- **Filter runs** by status, workflow ID, or a time range.
- **Page through** a summary of all top-level workflow runs.
- **Click on a run** to open its detailed view.

The run detail view is organized into tabs:

- **Overview:** Metrics plus `Trust & Provenance`, `Data Locality`, and `Evidence Export` panels.
- **Steps:** The status of every step in the run.
- **Events:** The full, ordered timeline of events.
- **Gates:** The status of any manual approval gates, including approver identity, reason, and timestamp when recorded.
- **Artifacts:** A list of all files produced by the run.
- **Evidence:** A list of all audit-trail files.
- **Children:** Any nested runs from `loop` or `subflow` steps.

On governed runs, the `Overview` tab shows whether trusted execution was enabled, the workflow/plugin digests and signature status that were recorded, which providers and models ran, whether data left the machine, and how to generate the markdown evidence export. The Evidence Export panel always shows the `transist runs evidence export` command hint; when no provenance exists yet, it displays "no evidence export generated yet" instead of a location.

## 4. Browse and Inspect Workflows

The "Workflows" section lets you explore the workflow definitions themselves. You can:

- **List all discovered workflows**, both from source files and compiled recipes.
- **Check the validation status** of each workflow.
- **Dive into a workflow** to inspect three server-rendered tabs:
  - `Plugins`: the plugins referenced by the workflow.
  - `Inputs`: declared workflow inputs.
  - `Steps`: root-level steps, with expandable nested branch and loop children.
- **Open the authored source or compiled recipe** from the workflow summary panel when you need the raw definitions.

## 5. View Artifacts and Evidence

When you click on an artifact or evidence file, the UI will show you:

- Its metadata (name, size, creator).
- An inline preview if the file type is supported (e.g., text, images).
- A link to download the raw content.

## A Strictly Read-Only Experience

The inspection UI is designed for safe, passive observation. It **cannot** be used to:

- Run, resume, or replay a workflow.
- Approve or reject a manual gate.
- Install plugins or modify any configuration.

All control and execution commands must be issued through the `transist` CLI. This clear boundary ensures that you can safely grant access to the inspection UI without any risk of unintended changes.
