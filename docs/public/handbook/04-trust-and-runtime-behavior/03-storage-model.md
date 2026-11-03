# The Storage Model

Transist is built on a **local-first** storage model. It uses the local file system as its source of truth. This makes run data easy to browse and back up, while leaving access control, retention, and backup policy under the operator's control.

## The Transist Home

Everything Transist needs is stored in a single folder called the **Transist Home**. A typical Home looks like this:

* **`config.yaml`**: Local settings and policy configuration.
* **`plugins/`**: Your library of installed tools.
* **`registry/`**: Your compiled "recipes" (workflows ready to run).
* **`runs/`**: The history of every workflow you've ever started.

## Inside a Run Folder

Every time you start a workflow, Transist creates a unique folder for it under `runs/run_<ID>/`. This folder contains the persisted runtime record for that run:

* **`state.json`**: The high-level summary. (What's finished? What failed?)
* **`events.jsonl`**: The play-by-play log of the runner's actions.
* **`recipe.yaml`**: The exact version of the workflow instructions being followed.
* **`artifacts/`**: The files produced by your steps.
* **`evidence/`**: The proof (logs, API responses) that supports the work.
* **`context/run_inputs.json`**: The data you provided when you started the run.

## Why this model works

1. **Transparency**: You can open any of these files in a text editor to see exactly what happened.
2. **Explicit state**: There's no hidden database state. If you delete a run folder, the run record is gone. If you move the home folder to another machine, the persisted history moves with it.
3. **Auditability**: Step execution outputs are written under execution-specific paths, so later attempts do not overwrite earlier ones.
4. **Operational control**: You do not need to manage a SQL server or object store for local operation, but you are responsible for filesystem access, backups, and retention.

## Operational Tip

Since Transist uses the local file system, ensure you know which "Home" is active. If you run a workflow with `--home ./data` and then try to inspect it without that flag, the CLI might look in the wrong place and tell you the run doesn't exist!

---

### Related

* [Operating the Transist Home Directory Guide](../../guides/inspection/operating-the-transist-home-directory.md)
* [Storage Layout Reference](../../reference/storage-layout.md)
* [Compatibility and Stability Reference](../../reference/compatibility-and-stability.md)

**Navigation:**

* Previous: [Secrets and Policy](02-secrets-and-policy.md)
* Next: [Companion HTTP Server](04-companion-http-server.md)
