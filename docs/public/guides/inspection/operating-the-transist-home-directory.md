# Guide: Operating the Transist Home Directory

The Transist "home" directory holds runtime configuration, installed plugins,
run history, and accepted artifacts/evidence. Workflow-selected working files
can live elsewhere. This guide explains home resolution and safe operation.

> **Conceptual Background:** For a deeper dive, see the handbook page on [Operating Transist Locally](../../handbook/05-admin-and-operations/01-operating-transist-locally.md).

## Choosing a Home Strategy

Before you operate a home, decide where it should live. Transist resolves the effective home in this order, first match wins:

1. `--home <path>` flag
2. `TRANSIST_HOME` environment variable
3. User-set default (`transist home set <path>`), used flat
4. Per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`)

Full precedence rules are in the [Storage Layout Reference](../../reference/storage-layout.md#how-the-home-directory-is-found).

If none of these resolve (no `HOME` and no `XDG_STATE_HOME`), non-home commands fail with an explicit error pointing at `transist home init` or `transist home set <path>`.

There are three practical strategies. Pick the one that fits your situation.

| Strategy | What it does | Use when |
|---|---|---|
| **Per-user default** *(`transist home init` with no flags)* | Creates the home at `${XDG_STATE_HOME:-$HOME/.local/state}/transist` and records it as your persisted user home. One shared home across every project on this machine. | You work across many projects and want one place for runs, plugins, and configuration. This is the recommended default. |
| **User-set default at an explicit path** *(`transist home set <path>`)* | Persists a specific shared home path instead of the computed per-user default. The path is used flat — no per-repo namespacing. | You want a central location at a specific path (e.g. on a different volume), or need it to match an existing deployment convention. |
| **`TRANSIST_HOME`** *(or `--home`)* | Points one shell, process, or invocation at one specific home. | Ephemeral overrides: CI/CD jobs that need a job-scoped home, scripted runs, or debugging another home. |

A few operational notes:

- `transist home init` with no `--home` flag and no `TRANSIST_HOME` always uses the per-user default and records it as your persisted user-set home; it never depends on the current working directory.
- `TRANSIST_HOME` overrides the persisted user-set default and the per-user default, but loses to `--home`.
- The TUI dashboard's `Setup` pane always shows the resolved effective home, its source, and the persisted user-set home (or `(none)` when none is set).
- Transist homes are never nested inside, or a parent of, the adapter workspace you run from; the runtime rejects either direction of nesting before dispatch.

**If you're unsure:** run `transist home init` once. It resolves to the per-user default, works from any directory on this machine, and costs nothing.

### Repository working files are not a home

Maintained delivery workflows use bounded root managed documents for operational
intermediates outside Git. The effective home persists their immutable revisions;
process actions receive invocation-private copies, never access to durable home
paths. Resume retains committed revisions; replay starts a fresh namespace.
Intended source/OpenSpec deliverables remain in the workspace.

Other workflows may explicitly use mutable workspace files, including supported
plugin-specific `.transist/` paths. Those are not another runtime home and are not
automatically captured or relocated by `--home` or `TRANSIST_HOME`. Preserve files
needed by active runs: a home backup does not include workspace mutations. See
[Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

## The Layout

Here's the structure of a typical Transist home directory, at whichever absolute path it resolved to (the per-user default, an explicit `--home`/`TRANSIST_HOME`, or a user-set path).

```text
<home>/
├── settings.yaml        # Local runtime defaults and filesystem policy
├── installation.json    # Stable installation id and workspace salt
├── registry/            # Compiled workflow recipes
│   └── recipes/
│
├── plugins/             # Installed plugins
│   └── <plugin-id>/
│       └── <plugin-version>/
│
└── runs/                # Runtime-managed run history and accepted outputs
    ├── _index/          # Internal index for fast run lookup
    └── run_<run-ref>/   # Data for a single run
```

## A Tour of the Directories

- `settings.yaml`: Runtime defaults and filesystem policy. Resolve the effective path with `transist home get`, then edit it directly; the TUI displays it read-only.
- `plugins/`: When you run `transist plugins install`, `plugin.yaml` and the
  manifest-declared `package.files` are copied here. Inspect this directory when
  you need to see exactly which versions of which plugins are installed.
  Installation is atomic: the verified package bytes are written through a
  private staging directory and atomically renamed into place, so a crash never
  leaves a partial or un-`install-index.json`-recorded plugin, and a trusted
  (`--trusted-execution`) install can never be observed without its signature
  sidecar.
- `registry/recipes/`: A successful `transist workflows install` stores the resulting compiled workflow "recipe" here. Inspect this when you need to see the exact, versioned workflow definition that the runtime uses.
- `runs/`: This is where workflow run history lives. Every run gets its own `run_<run-ref>` subdirectory, containing its state, event log, artifacts, and evidence. Inspect this directory when you're debugging a specific run.

## Safe Cleanup and Operations

Because everything is just files in a directory, managing Transist is straightforward.

- **Removing old runs:** It is safe to delete a `run_<run-ref>` directory if you no longer need its history or artifacts.
  - *Pro-tip:* If you manually delete a run directory, also delete the corresponding file in `runs/_index/` to keep the index clean.
- **Managing plugins:** The safest way to remove a plugin is with the CLI: `transist plugins uninstall <plugin-id>`. Avoid deleting files from the `plugins/` directory manually.
- **Be careful with `config.yaml`:** This file contains your security and runtime policies. Don't delete it unless you intend to reset to the defaults.

In general, prefer using the `transist` CLI commands (`plugins uninstall`, `runs cleanup`) for modifications, and use manual file operations only for inspection or backing up data.

## Backup and Retention Strategy

- **Back up the `runs/` directory:** This directory *is* your audit trail. If you need to keep a long-term record of what your automations have done, this is the directory to archive.
- **Back up `config.yaml`:** If you have customized your policies, you should keep this file under version control.
- **Treat `plugins/` as reproducible:** You don't necessarily need to back up your installed plugins, as long as you can re-install them from their source.

By understanding this simple directory structure, you gain full control over your local Transist environment, making it easy to manage, back up, and inspect.
