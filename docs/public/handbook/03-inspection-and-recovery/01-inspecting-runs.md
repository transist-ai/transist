# Inspecting Your Runs

Transist is built on the idea that "something happened" is not enough information. You need to know exactly *what* happened, *why* it happened, and *what* was produced. This chapter covers the tools you have to inspect your workflow runs.

## 1. The CLI Inspection Path

Most of the time, you'll use the Transist CLI to get answers. Here are the most useful commands:

* **`transist runs list`**: See one-line status summaries for recent root runs. Add `--all` to include nested child runs or `--ids-only` for just run ids.
* **`transist runs status <id>`**: Get a quick, human-readable update on a specific run.
* **`transist runs show <id>`**: See the full details of a run, including the status of every step, any errors, and any nested child runs.
* **`transist runs tail <id>`**: Watch the live stream of events as they happen.
* **`transist runs evidence export <id>`**: Render the markdown evidence report derived from persisted local provenance and run state.

These surfaces also show effective filesystem policy details when the run includes local filesystem constraints:

* Global default policy (from the run's prepared snapshot when available)
* Workflow-level policy
* Per-step effective policy source, mode, and allow lists
* `filesystem_policy.denied` event payloads when the runtime blocks a path

## 2. The Interactive TUI (The Power User View)

If you're reviewing a complex run or need to make a decision at a Manual Gate, the default JSON output can be overwhelming. Instead, use the **Interactive TUI**:

```bash
transist runs show <RUN_ID> --tui
```

This opens a fullscreen interface that allows you to:

* **Browse Steps**: See a list of all steps and their status.
* **Inspect Files**: Highlight a file (like an artifact or a log) and press `o` to open it in your pager (like `less` or `bat`).
* **Review History**: See a chronological list of every decision made at every gate.
* **Analyze AI Usage**: View token usage and model responses for AI-powered steps.

The fullscreen dashboard is an operator console. It provides read-only plugin
and workflow inventories, workflow runs, run browsing, recovery, and local
preferences. Install, validate, compile, promote, and uninstall workflows with
the CLI; governed workflow delivery starts with `transist-admin workflow
package` and `transist workflows install`.

Use **Problems** for findings derived from local persisted state. Select a
finding to open its complete diagnostic; failed runs also offer **View full
diagnostic** before recovery actions. Plugin, adapter, model, and stderr text
in reports is untrusted external output, never runtime authority. Dashboard
failures stay in the console and point to Problems rather than closing it.

The diagnostic pager copies only its redacted report with `c` via OSC 52. Copy
is refused, without truncation or an OSC 52 sequence, above 65,536 unencoded
bytes. `e` uses the preferred pager and then `$PAGER`; if neither is set, the
internal pager remains open. The dashboard inventories are read-only, while
Preferences is limited to local pager, editor, gate actor, and inspection-cache
settings. Runtime/home setup, trust, lifecycle, policy, and inspection-sync
configuration are CLI or `transist-admin` work.

The run browser and detail views also distinguish timing semantics explicitly:

* **Wall clock**: how long the run existed.
* **Active**: summed execution work.
* **Blocked wait**: time spent waiting at manual gates.

`total_duration_ms` in JSON/API remains wall-clock for backward compatibility.

## 3. Filesystem Inspection (The Source of Truth)

Because Transist is local-first, the ultimate source of truth is always your file system. Every run has its own folder in your Transist Home:

`runs/run_<RUN_ID>/`

Inside, you'll find:

* **`state.json`**: The complete data model of the run.
* **`events.jsonl`**: The play-by-play log of the kernel.
* **`artifacts/`**: The actual files produced by your steps.
* **`recipe.yaml`**: The exact instructions Transist was following.

For declared-workspace runs, `state.json` and `recipe.yaml` also show the
canonical persisted workspace and access. Use that value to diagnose filesystem
denials; changing the directory from which you run inspection or recovery
commands cannot alter it. The workflow source path is distinct provenance and
package context, not a workspace grant.

## 4. The Companion Server

If you prefer a web-based view or need to integrate Transist with another tool, you can use the **Companion Server**. It provides a read-only HTTP API that exposes all the same run data in a structured JSON format.

For governed runs, the companion server also surfaces trust, locality,
approval, and evidence-export facts through `GET /api/v1/runs/<run_id>`,
`GET /api/v1/runs/<run_id>/trust`, `GET /api/v1/runs/<run_id>/locality`,
and `GET /api/v1/runs/<run_id>/gates`.

---

### Related

* [Inspecting Runs, Artifacts, Evidence, and Events Guide](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
* [CLI Reference](../../reference/cli.md)
* [Storage Layout](../../reference/storage-layout.md)

**Navigation:**

* Previous: [Authoring Patterns](../02-everyday-usage/08-authoring-patterns.md)
* Next: [Artifacts, Evidence, and Events](02-artifacts-evidence-and-events.md)
