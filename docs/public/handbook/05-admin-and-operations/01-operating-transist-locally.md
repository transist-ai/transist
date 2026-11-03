# Operating Transist Locally

Transist is designed to be a "hands-off" tool, but like any piece of software that stores data on your disk, it requires a small amount of operational care. This chapter covers the basics of maintaining your local Transist environment.

## 1. Know Your Home

The most important rule of Transist operations is: **Always know which Transist Home is in effect.** Before you start debugging or searching for a run, confirm your current directory:

```bash
transist home get
```

The effective home is resolved from one of four sources, in order (first match wins): `--home` flag, `TRANSIST_HOME` env, user-set default (`transist home set`), per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`, used by `transist home init` with no other override). If none resolves (no `HOME` and no `XDG_STATE_HOME`), non-home commands fail with an explicit error. See [Choosing a Home Strategy](../../guides/inspection/operating-the-transist-home-directory.md#choosing-a-home-strategy) for a per-use-case recommendation.

## 2. A Healthy Daily Routine

If you use Transist daily, a good "hygiene" routine looks like this:

1. **Summarize**: Use `transist runs list` to keep track of root runs with one-line status summaries, `transist runs list --all` for nested child runs too, or `transist runs list --ids-only` for ids only.
2. **Inspect**: If something looks wrong, use `transist runs show <id>` to find the failure.
3. **Trace**: Use `transist runs tail <id>` to see the exact timeline of events.
4. **Verify**: Occasionally check the run's `artifacts/` and `evidence/` trees to ensure that artifacts and evidence are being stored as expected.

## 3. Treating the Home as an Asset

Your **Transist Home** is not a temporary cache; it's a repository of your work history. It contains:

* Your installed tools (**Plugins**).
* Your compiled instructions (**Recipes**).
* Your entire operational history (**Runs**).

Treat it as an asset. Don't delete folders inside the Home unless you are sure you no longer need the history of those runs.

## 4. Operational "Don'ts"

To keep your system stable, avoid these common mistakes:

* **Don't mutate files by hand**: While the files are human-readable, modifying `state.json` or `events.jsonl` manually can confuse the Transist runner. Use the CLI commands (like `approve`, `resume`, or `cleanup`) instead.
* **Don't assume the Home is what you expect**: Transist resolves the effective home the same way from any directory (`--home`, `TRANSIST_HOME`, user-set default, then the per-user default); it never depends on the current working directory. If you have multiple homes (e.g. via `--home` overrides), always confirm with `transist home get`.
* **Don't ignore storage growth**: If you run many workflows that produce large artifacts, your Transist Home will grow over time. Monitor your disk usage and perform periodic cleanups of old runs.

## 5. Tuning the Inspection Cache

Each TUI or `transist runs ...` inspection context owns an in-memory **inspection cache** that holds recently-inspected run details. Callers that reuse a context can share its cache explicitly; separate commands do not share run data accidentally. The cache is fingerprint-validated (entries are reused only when the backing files have not changed), bounded by run count, and performs disk reads outside its shared lock. A cache miss for one run does not block unrelated runs.

Run-summary projections are checked by content, not just modification time and
length. Independent readers therefore see same-size summary replacements within
one filesystem timestamp tick. Stable summaries retain the parsed-record cache
hit; this check reads only the small summary sidecar, not artifact/evidence bodies.
Rebuilding a summary reads authoritative state, recipe and blob listings without
input-cache reuse, so a stale warmed state entry cannot be published as a new
summary. This does not disable the parsed-summary cache for ordinary inspection.

Two controls are exposed:

* **`tools.inspection_cache_runs`** (user settings): how many distinct runs the cache may track. Defaults to a small value that suits most laptops. Raise it if you frequently switch between many active runs in the TUI and notice repeated parsing; lower it if memory pressure matters more than recall.
* **Press `r` on the TUI run-detail screen**: explicitly purges the cache for the current run and forces a fresh read from disk. Useful after an out-of-band edit or when you want to confirm what is actually on disk.

To set the cache size, either edit it interactively from the TUI **Preferences** screen ("Set inspection cache size (runs)") or write directly to your user settings file:

```yaml
tools:
  inspection_cache_runs: 50
```

Settings files that omit this field remain valid; see [`Compatibility & Stability`](../../reference/compatibility-and-stability.md#persistence-and-schema-policy).

## 6. Trust Material Placement

Keep `transist-admin` private keys outside the runtime home and outside normal
workflow directories you share with runtime machines.

* Generate private keys on an operator or CI machine with `transist-admin key generate`.
* Store `*.key` files in an operator-private location with restricted filesystem access.
* Export only the `transist.trust_root.v1` public file with `transist-admin key export-public`.
* Import only the public file onto runtime machines with `transist trust import <trust-root-file>`.

Example:

```bash
cargo run -p transist-admin -- key generate --name acme --private-key-file "$HOME/.config/transist-admin/trust/acme.key"
cargo run -p transist-admin -- key export-public --private-key-file "$HOME/.config/transist-admin/trust/acme.key" --trust-root-file "$HOME/.config/transist-admin/trust/acme.pub"
transist trust import "$HOME/.config/transist-admin/trust/acme.pub"
transist trust list
```

Runtime machines should never need the `*.key` file. They only need imported
public trust roots.

### Related

* [Operating the Transist Home Directory Guide](../../guides/inspection/operating-the-transist-home-directory.md)
* [Storage Layout Reference](../../reference/storage-layout.md)
* [CLI Reference](../../reference/cli.md)

**Navigation:**

* Previous: [The Companion HTTP Server](../04-trust-and-runtime-behavior/04-companion-http-server.md)
* Next: [Managing Run Data and Storage](02-run-artifacts-storage-and-evidence.md)
