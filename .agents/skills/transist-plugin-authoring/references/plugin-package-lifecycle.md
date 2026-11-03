# Plugin Package Lifecycle

Use this lifecycle for package authoring and local iteration:

1. Create or update `plugin.yaml`.
2. Declare `package.files` with every runtime file: the entrypoint, referenced
   schemas, local modules, dynamically loaded assets, certificates, and local
   schema `$ref` targets. Keep selected files inside the package directory.
3. Install the package with `transist plugins install --path <plugin-dir>/plugin.yaml` (directory paths remain accepted as a compatibility input).
4. Confirm installed versions with `transist plugins list`.
5. Inspect manifest shape and bindings with `transist plugins inspect <plugin-id>`.
6. If the package is meant for governed runtimes, use the default
   `<plugin-dir>.sig` sidecar or install with `--signature <path>` when the
   detached signature is stored elsewhere, and ensure the runtime imports the
    approving public key with `transist trust import <trust-root-file>`.
7. Remove a version with `transist plugins uninstall <plugin-id> --version <plugin-version>` when cleanup or reinstall is needed. Omit `--version` to remove all installed versions at once.

## Package Layout

Each package should contain:

- `plugin.yaml`
- a non-empty `package.files` list containing the complete runtime payload

`plugin.yaml` is implicit and must not be repeated in `package.files`. The
selected payload is exactly `plugin.yaml` plus `package.files`; unselected files
are not digested, signed, installed, or available as runtime dependencies. Do
not use globs, negation, or ignore files.

Action schemas are compiled during installation using JSON Schema Draft
2020-12. Package-local relative `$ref` assets are included in the package
digest; remote, absolute, parent-traversing, and symlink-escaping references
are rejected offline. Runtime adapters validate input before dispatch and
successful output before converting hints.

Installed packages live under `plugins/<plugin-id>/<plugin-version>/` beneath the effective home. Use `transist home get` when `--home`, `TRANSIST_HOME`, or `transist home set` may change where the install landed.

## Real Workflow Validation

Manifest validation is not enough. Validate the plugin in a workflow that exercises the intended `adapter + plugin + action` combination:

1. `transist workflows check --path <flow.yaml>` for authored checks.
2. Install a governed workflow with `transist workflows install --package <package-directory>`; use source `--path` only for unsigned standalone/development authoring.
3. `transist workflows check <id@version> --inputs-file <inputs.json>` for probe-free prospective validation. Required warnings and `not_evaluated` checks block success; passing validation is not live readiness. Add `--live` or `--output readiness.json` for live probes/evidence.
4. When validation reports only exact missing authority, use `transist workflows authorize <id>@<version>` with the same repeatable `--input key=value` arguments or `--inputs-file <inputs.json>`, then rerun check.
5. `transist workflows check <id@version> --inputs-file <inputs.json> --live` confirms plugin readiness; `transist workflows run <id@version> [--run-ref <run-ref>]` is the normal happy path after readiness is ready.
6. `transist workflows prepare <id@version> [--run-ref <run-ref>]` plus `transist runs start <run-id>` when staged inspection matters.
7. `transist runs status <run-id>` for the quick human-readable summary.
8. `transist runs show <run-id>`.

Authorization uses fast validation, not live probes. Already-authorized calls
write nothing. Applying reevaluates current inputs/resources/policy and requires
the exact reviewed fragment and destination to match; drift requires another review.

Use the same effective home for install, validation, compilation, and execution when the workflow depends on the installed plugin's bindings.

## Upgrade Strategy

- Bump the plugin version when package contents or `package.files` changed
  materially.
- Remove and reinstall only when local iteration needs a same-version reset.
- Treat adapter kind changes and action-name changes as compatibility-sensitive.
