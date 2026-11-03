# Plugin Authoring

Transist is built to be extended. You can create your own plugins to bring any
tool or service into your workflows. A plugin is a versioned integration
package, not an in-process module, so its execution participant can be a script,
CLI wrapper, multi-file program, standalone HTTP service, or MCP server written
in any language.

**Writing your first plugin?** Start with
[Write Your First Plugin](../../guides/plugins/write-your-first-plugin.md), the
complete tutorial for a local Python greeting plugin that speaks APP. This
handbook page explains the package concepts and extension boundaries; it is not
the step-by-step tutorial.

## What's in a Plugin Package?

A Transist plugin package is a directory with a `plugin.yaml` manifest and an
explicitly selected payload:

* **`plugin.yaml`**: The manifest that tells Transist what the plugin is and what it can do.
* **`package.files`**: A non-empty list of package-relative regular files that are included in the digest, signature, and installed tree.
* **Package assets**: The entrypoint, schemas, local modules, dynamically loaded assets, certificates, and local schema `$ref` targets listed in `package.files`.

`plugin.yaml` is implicit and must not appear in `package.files`. Files not
listed are not digested, signed, installed, or available as runtime
dependencies. Globs, negation, and ignore files are not supported.

For `process` and `mcp`, the selected payload contains the local entrypoint and
everything it needs at runtime. For `http`, the execution participant is a
separately deployed service; the package contains the manifest, schemas, and
any local certificate material needed to define that integration. A complex
service does not need to put its complete implementation inside the package.

## Choose Your Language

You are not restricted to any specific programming language. The execution
participant must be reachable through one supported **Adapter** (`process`,
`http`, or `mcp`) and conform to that adapter's protocol contract.

* *Want to use Python?* Use the `process` adapter and implement APP over NDJSON in your `.py` entrypoint.
* *Want to use Go?* Compile an APP-speaking entrypoint and select it in a `process` package.
* *Have several scripts?* Put an APP-speaking dispatcher in front of the actions and select every runtime dependency in `package.files`.
* *Want to use a web service?* Use the `http` adapter and expose the APP endpoints.
* *Already have an MCP server?* Use the `mcp` adapter and bind plugin actions to its tools.

APP defines handshake, invocation, progress, result, cancellation, errors, and
artifact/evidence hints. MCP participants speak the supported MCP tools subset;
the adapter maps that lifecycle into APP semantics. Existing software that does
not speak either contract normally needs a thin wrapper or gateway. Its
business logic can remain unchanged behind that boundary.

## The Plugin Manifest (`plugin.yaml`)

The manifest is the "contract" between your plugin and the Transist runner. It defines:

* **Identity**: The ID and version of your plugin.
* **Adapter Kind**: Which adapter should be used to run this plugin.
* **Actions**: A list of the specific commands your plugin supports (e.g., `send_email`, `create_user`).
* **Bindings**: (Optional) Special mappings that allow your plugin to use Transist's first-class AI fields like `prompt` and `skills`.

Changing `plugin.yaml`, the `package.files` selection, or any selected file's
bytes changes the package digest. Independent plugin authors use a distinct
version for changed selected bytes installed in the same home. The maintained
repository catalog retains `0.1.0`: validate and promote each changed signed
bundle into a fresh home, preserving the old home and bundle. Never overwrite
installed payloads. See [Plugin Lifecycle Management](../../guides/plugins/plugin-packaging-install-upgrade-removal.md#4-upgrade-a-plugin)
for the upgrade procedure.

It is the plugin author's responsibility to include every file that the
entrypoint loads dynamically, not only files named directly by the manifest.

### Action Schema Rules

Use JSON Schema Draft 2020-12 for every action's input and output schema.
Transist compiles both files during install. Relative `$ref` files are allowed
only inside the package root; remote, absolute, parent-traversing, and
symlink-escaping references fail installation without network access.

At execution time, the adapter validates the materialized input before
dispatch. A successful action output must validate before the adapter converts
artifact/evidence hints or returns the result. Validation failures use the
canonical `validation_error` and diagnostics contain paths, not invalid values.

Important v1 packaging rules:

* `entrypoint.command` for `process` and `mcp` plugins must point to a file
  inside the plugin directory and that file must be selected by `package.files`.
* MCP actions must declare `bindings.tool_name`.
* Action names must be unique within one manifest.

### Provider Temporary Directories

Process and MCP providers declare invocation-private temporary directories in
their manifest, never in workflow input or plugin code:

```yaml
entrypoint:
  type: process
  command: provider.sh
  env:
    PROVIDER_CACHE: runtime://temp/provider/cache
    TMPDIR: runtime://temp/provider/tmp
```

Only complete `runtime://temp` and `runtime://temp/<relative-path>` values are
valid. Relative path segments use `[A-Za-z0-9][A-Za-z0-9._-]*`; do not use
slashes at the end, traversal segments, queries, fragments, percent encoding,
or embedded tokens. The runtime creates the directories before startup and sets
`TMPDIR` last. These declarations grant no host filesystem authority and are
removed after the invocation, so use workspace or artifact paths for data that
must survive.

Temporary declarations also cannot preserve provider login, session, or cache
state between invocations. Model and service credentials belong in declared
secret bindings with the corresponding runtime policy; do not depend on the
provider's normal host-home login lookup when `HOME`, configuration, or data
directories are declared as `runtime://temp`. On macOS this redirection does not
claim complete same-user host read isolation, and plugins must not probe
alternate host paths for credentials.

## The Developer Workflow

The high-level path for building a new plugin is below. Follow
[Write Your First Plugin](../../guides/plugins/write-your-first-plugin.md) for
the complete files, commands, and workflow invocation.

1. **Draft**: Create your `plugin.yaml` and your entrypoint script.
2. **Install**: Use `transist plugins install --path ./my-plugin/plugin.yaml` to add it to your local Transist Home.
3. **Verify**: Run `transist plugins inspect my-plugin-id` to make sure Transist sees all your actions and schemas.
4. **Test**: Write a small workflow that uses your new plugin and run it!

For trusted distribution, `transist-admin plugin sign ./my-plugin --private-key-file <private-key-file>`
creates the default `my-plugin.sig` sidecar. Use `--output <path>` when the
signature must be stored elsewhere, then install with
`transist plugins install --path ./my-plugin/plugin.yaml --trusted-execution --signature <path>`.
The explicit path is used only for installation; the runtime retains the
verified signature with the installed package.

---

### Related

* [Write Your First Plugin](../../guides/plugins/write-your-first-plugin.md)
* [Plugin Packaging and Installation Guide](../../guides/plugins/plugin-packaging-install-upgrade-removal.md)
* [Plugin Manifest Reference](../../reference/plugin-manifest.md)
* [CLI Reference](../../reference/cli.md)

**Navigation:**

* Previous: [Adapters and Plugins](02-adapters-and-plugins.md)
* Next: [Adapter Responsibilities](04-adapter-responsibilities.md)
