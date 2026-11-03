# Guide: Plugin Lifecycle Management

This guide covers the full lifecycle of a Transist plugin: installing, inspecting, upgrading, and removing it. If you plan to use or develop plugins, this is essential reading.

## What's a Plugin?

A plugin is a versioned integration package that extends Transist with new
capabilities. "Plugin" names the contract Transist installs and invokes; it
does not imply a small library loaded into the Transist process. The software
behind a plugin may be a script, a CLI wrapper, a multi-file application, a
standalone web service, or an MCP server.

Every plugin package includes:

- A `plugin.yaml` manifest file describing what the plugin is and what it can do.
- JSON schema files that define the inputs and outputs for each of its "actions."
- The runtime files selected by `package.files`. A local `process` or `mcp`
  package normally selects an executable script or binary and its support
  files. An `http` package can select only the local contract material needed
  to describe and authenticate a separately deployed service.

The execution participant can be written in any language or framework. It must
conform to [APP (Adapter Plugin Protocol)](../../reference/app.md) over process
or HTTP, or the supported MCP tools protocol mapped by the MCP adapter. Use
[Choosing a Plugin Adapter](mcp-integration-in-practice.md) to select between
`process`, `http`, and `mcp` and review the supported MCP tools contract.

Ready to build your own? Follow [Write Your First Plugin](write-your-first-plugin.md)
for a complete local Python APP tutorial. The
[Plugin Authoring Handbook](../../handbook/06-architecture-and-extension/03-plugin-authoring.md)
explains the package concepts and extension boundaries.

## Managing Plugins with the CLI

The `transist plugins` command is your one-stop shop for all plugin management.

### 1. Install a Plugin

You install a plugin from a local plugin directory or its `plugin.yaml` manifest file. Archive packages are not part of the v1 install surface. Transist copies only `plugin.yaml` and the files explicitly declared in `package.files` into your current Transist home directory, making the plugin available to your workflows. Prefer passing the plugin's `plugin.yaml` manifest file for an explicit source boundary.

For a governed home, import the publisher's public trust root with
`transist trust import <trust-root-file>` before installing the plugin. The runtime accepts
public trust roots only and never needs a private signing key.

```bash
# Install a plugin from its manifest file (preferred)
transist plugins install --path /path/to/my-local-plugin-directory/plugin.yaml
```

If you're working inside the main `transist` repository, you can install one of the bundled plugins directly:

```bash
transist plugins install --path plugins/shell/plugin.yaml
```

Plugin versions are content-immutable. Once Transist has seen `shell@0.1.0`
for a specific set of bytes, reinstalling `shell@0.1.0` with different bytes is
rejected even if you removed the old files first. Maintained Transist catalogs
retain release `0.1.0`: changed bytes need a new revision/digest/signature and
validation and promotion of the reviewed signed bundle into a fresh home.
Preserve the old home/bundle for pinned runs and rollback. Independent plugin
authors use a distinct `<new-version>` for changed selected bytes installed in
the same home. Never overwrite installed payloads.

Plugin installation is atomic and crash-recoverable:

- The installed tree is exactly the verified snapshot. After copying the source
  package into a private staging snapshot and verifying its digest and (in
  trusted mode) its detached signature, the staged snapshot is re-digested
  immediately before publication; any mutation during install aborts the
  install. The real plugin tree is then published by a descriptor-anchored,
  no-clobber rename — never a re-copy — so the installed bytes always equal the
  verified digest bytes.
- The package directory, the install-index entry, and (for a trusted install)
  the `transist.signature.v1` sidecar are committed as one recoverable unit. A
  crash before the rename leaves nothing installed; a crash between these steps
  is reconciled on the next successful install attempt. No crash yields an
  installed-but-unindexed package or an installed-but-unsigned trusted package.
- In trusted mode a missing signature sidecar fails closed: `transist plugins
  install --trusted-execution` refuses to install a package whose verified
  sidecar is absent (the reserved development test-version bypass is the only
  exception).

By default, trusted install reads `<plugin-directory>.sig`. When a publisher
stores a detached signature elsewhere, pass it explicitly instead:

```bash
transist plugins install --path /path/to/plugin/plugin.yaml \
  --trusted-execution --signature /path/to/signatures/plugin.sig
```

The explicit path takes precedence and never falls back to the default sidecar.
After successful installation the runtime stores the verified signature beside
the installed snapshot, so the external source file is not needed for later
trusted runs. `--signature` selects a source; it does not enable trusted mode.

Plugin package hardening rules in v1:

- `transist plugins install` accepts a plugin directory or `plugin.yaml` path only.
- `package.files` is required and is the only payload-selection mechanism. List
  the entrypoint, local modules and dynamically loaded assets, certificates,
  action schemas, and every local schema `$ref` target.
- The signed and installed payload is exactly `plugin.yaml` plus
  `package.files`. Non-selected source files are not installed and cannot be
  used as runtime dependencies.
- Symlinks and special files selected by `package.files` are rejected during
  digest, sign, verify, and install.
- Both input and output action schemas must compile as JSON Schema Draft 2020-12.
- Relative `$ref` files must remain inside the canonical package root. Remote,
  absolute, parent-traversing, and symlink-escaping references are rejected
  without network access.

The adapter validates materialized action input before dispatch and validates a
successful action output before converting artifact/evidence hints. An invalid
instance returns `validation_error` and is not retained in the result.

### 2. List Installed Plugins

To see which plugins and versions are available in your current environment, use `list`.

```bash
transist plugins list
```

This command checks the `plugins/` directory under your effective Transist home (`transist home get`) and shows you what it finds.

### 3. Inspect a Plugin

Need to know exactly what a plugin can do? The `inspect` command shows you its full manifest, including all available actions, their parameters, and what they expect as inputs.

```bash
transist plugins inspect <plugin-id>
```

For example, `transist plugins inspect shell` will show you everything about the `shell` plugin.

### 4. Upgrade a Plugin

The `install` command won't overwrite an existing plugin of the same version.
Follow the source repository's version policy:

1. **Maintained Transist catalogs:** retain release `0.1.0` and exact dependency
   pins. Record the new source revision, digest, and corresponding signature,
   validate the reviewed signed bundle in a fresh home, and promote it into a
   fresh target home. Preserve the old home/bundle for existing runs and rollback.
2. **Independent repositories:** use a distinct `<new-version>` for changed
   selected bytes installed in the same home. Install it alongside the old
   version and update workflow pins deliberately.

Forced same-version overwrite is unsupported. Uninstalling packages or deleting
runs does not make different bytes interchangeable at the same installed version.

### 5. Uninstall a Plugin

You can remove a single, specific version of a plugin or get rid of all versions at once.

```bash
# Remove just one version
transist plugins uninstall <plugin-id> --version <version>

# Remove all installed versions of a plugin
transist plugins uninstall <plugin-id>
```

This permanently deletes the plugin's files from your Transist home directory.
Transist blocks removal while any protected run still references that plugin so a
prepared, blocked, failed, or otherwise resumable run cannot lose the exact
plugin bytes it was pinned to.

Uninstall does not remove the plugin's entry from
`<home>/plugins/install-index.json`. That index is immutable installation
history, not the current installed-plugin inventory: it retains the verified
digest for each previously installed `<plugin-id>@<version>` so different bytes
cannot later reuse the same version. Use `transist plugins list` to see what is
currently installed. Reinstalling the same verified bytes is allowed. For
changed content, use the repository-scoped upgrade path above: a fresh home for
fixed `0.1.0` catalog packages, or a distinct version for independent packages
with changed selected bytes installed in the same home.

## Using a Plugin in a Workflow

Once a plugin is installed, declare its exact version at the workflow root and
call an action through an invoke step's `uses` value.

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: run_a_simple_command
    uses: process:shell/exec
    with:
      command: echo
      args: ["Hello from a plugin!"]
```

**Tip:** Before you run a workflow, run `transist workflows check --path <workflow.yaml>` on its authored source. This will tell you if the plugins and actions it needs are installed and configured correctly.
