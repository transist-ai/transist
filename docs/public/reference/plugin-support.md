# Reference: Plugin Support

This page defines which plugins are supported in the Transist product.

These commitments describe current `0.y` behavior. Transist is pre-1.0: no
surface is stable before `1.0.0`, and the support statements below are bounded
guarantees about the documented lifecycle, not a stability or availability
promise. See
[Compatibility and Stability](compatibility-and-stability.md).

## The Core Support Promise

For any officially supported plugin, we guarantee that it follows the standard Transist lifecycle:

- Its manifest is valid according to the `transist.plugin.v1` schema.
- It can be successfully installed and uninstalled using the `transist plugins` commands.
- Its actions can be inspected and used reliably in your workflows.

*Note: Plugin installation in v1 is local-only and directory-based. Use a plugin directory or its `plugin.yaml` file. Archive packages, automated registry discovery, and remote downloads are not part of the v1 feature set.*

---

## The Official Inventory

### Built-in Plugins

The following plugins are part of the core Transist package and are guaranteed to be available and supported.

| Plugin | Status | Notes |
| :--- | :--- | :--- |
| **`shell`** | **Built-in** | Our baseline plugin for running shell commands. It’s used in most examples and the Quickstart. |

---

Packaging constraints for supported plugins:

- `package.files` must explicitly select every runtime payload file. The
  installed and signed payload is exactly `plugin.yaml` plus those files;
  unselected files are not copied and cannot be runtime dependencies.
- Selected paths must be package-relative regular files; symlinks and special
  files are rejected.
- Every action input and output schema must compile as JSON Schema Draft
  2020-12 during installation. Package-local relative `$ref` assets are
  required in `package.files`; remote, absolute, parent-traversing, and
  symlink-escaping references are rejected offline.
- Adapters validate action input before dispatch and successful action output
  before converting hints or returning result data.

---

## Support and Quality Tiers

Any plugin officially owned by the Transist core must meet these criteria:

- **Full Compatibility**: It must strictly follow the [**Adapter Plugin Protocol (APP)**](app.md).
- **Validated Manifest**: It must pass all `transist plugins inspect` checks.
- **CI Coverage**: It must be included in our automated test suites for installation and basic functionality.

Plugins outside the built-in inventory are not covered by this support promise.

---

## Compatibility and Versioning

- **Breaking Changes**: Changing a plugin's ID, an action name, or its adapter type is considered a breaking change.
- **Versioning**: We recommend using Semantic Versioning for all plugins. When
  the implementation or `package.files` changes, the package digest changes;
  bump the `version` field in `plugin.yaml` rather than reusing an existing
  version.
