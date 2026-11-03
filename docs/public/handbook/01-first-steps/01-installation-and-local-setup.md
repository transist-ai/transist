# Setting Up Your Environment

Before you can run your first workflow, you need Transist installed from an approved distribution channel and a local Transist home for your data.

> **Looking for a step-by-step guide?** Check out the [Installation and Local Setup Guide](../../guides/getting-started/installation-and-local-setup.md).

## 1. Install and Initialize

Transist is distributed as a binary package. Once you've added it to your `PATH`, choose a runtime mode before initialization. `governed` is the default and is the path for regular controlled delivery. The standalone commands below are only for a disposable local evaluation.

```bash
# Create a separate standalone evaluation home (permissive policy; unsigned install accepted)
TRANSIST_HOME=/absolute/path/to/evaluation-home transist home init --mode standalone

# Install the bundled shell plugin (needed for the first examples)
transist plugins install --path plugins/shell/plugin.yaml
```

* `transist home init --mode standalone` creates an explicitly standalone home with permissive policy so unsigned plugins and workflows are accepted without capability grants. It is not a governed-delivery substitute.
* `transist plugins install` copies an approved plugin into your Transist home so the runner can use it.

For regular use, initialize the default governed home with `transist home init`, then use [Governed Runtime Setup and Delivery](../../guides/trust/governed-runtime-setup-and-delivery.md). It explains trust-root import, signed plugin/workflow delivery, exact policy grants, and trusted run preparation.

## 2. Understanding the "Transist Home"

Your **Transist Home** is the directory where Transist stores everything: your settings, installed plugins, and the history of every workflow you run.

Transist looks for its home in this order (first match wins):

1. **Command line flag:** `--home <path>`
2. **Environment variable:** `TRANSIST_HOME`
3. **User-set default:** A path saved using `transist home set` — used flat, no per-repo namespacing
4. **Per-user default:** `${XDG_STATE_HOME:-$HOME/.local/state}/transist`

If no source resolves (no `HOME` and no `XDG_STATE_HOME`), non-home commands fail with an explicit error pointing at `transist home init` or `transist home set <path>`.

To see where your data is currently being stored, run:

```bash
transist home get
```

## 3. Working with Plugins

You can't run a workflow step unless the required plugin is installed in your Transist Home. You can manage your plugins with these commands:

```bash
# See what's currently installed
transist plugins list

# Get details about a specific plugin (like what actions it supports)
transist plugins inspect shell
```

## 4. What's Happening on Your Disk?

Once you've initialized Transist, you'll see a few new files and folders in your Home:

* `config.yaml`: Your local configuration.
* `plugins/`: The library of tools you've installed.
* `runs/`: (Created later) A detailed history of every workflow run.
* `registry/`: (Created later) Cached "recipes" of your compiled workflows.

---

### Related

* [Installation and Local Setup Guide](../../guides/getting-started/installation-and-local-setup.md)
* [CLI Reference](../../reference/cli.md)
* [Runtime Modes](../../reference/runtime-modes.md)
* [Storage Layout](../../reference/storage-layout.md)

**Navigation:**

* Next: [Your First Workflow](02-your-first-workflow.md)
