# Plugin Conformance Checklist

Ordinary installed `workflows check` and authorization use fast validation,
not live readiness. A pass leaves readiness `not_ready`; use `--live` or
`--output readiness.json` to probe the plugin. Required warnings and incomplete
validation block success. Preparation still checks live readiness before birth.

1. Inspect the package first:
    - `plugin.yaml`
    - referenced schemas
    - entrypoint assets
    - any workflow meant to exercise the plugin
2. Confirm manifest essentials:
    - `schema` is `transist.plugin.v1`
    - `id`, `version`, and `adapter` are explicit
    - `package.files` is non-empty, unique, package-relative, and lists the
      complete runtime payload; `plugin.yaml` is implicit
    - every action declares `name`, `input_schema`, and `output_schema`
    - both action schemas compile as Draft 2020-12 documents
    - every external `$ref` is a relative, package-local path with no parent traversal or symlink escape
    - bindings and `secret_inputs` match the action's real behavior
3. Confirm lifecycle behavior:
    - `transist plugins install --path <plugin-dir>/plugin.yaml` (directory paths remain accepted as a compatibility input)
    - `transist plugins list`
    - `transist plugins inspect <plugin-id>`
    - `transist plugins uninstall <plugin-id> --version <plugin-version>` when reinstall or cleanup is needed
4. Confirm workflow-level validation:
    - install the plugin before validating the workflow
    - `transist workflows check --path <flow.yaml>` for authored checks
    - install the governed workflow package before prospective checks; source `--path` installation is unsigned standalone/development authoring only
    - `transist workflows check <id@version> --inputs-file <inputs.json> --output readiness.json` before run birth; the report must contain `run_created: false`
    - `transist workflows authorize <id>@<version>` with the same repeatable `--input key=value` arguments or `--inputs-file <inputs.json>` only for exact missing authority, then rerun the check
    - `transist workflows run <id@version> [--run-ref <run-ref>] --readiness-file readiness.json` for the normal happy path
    - `transist workflows prepare <id@version> [--run-ref <run-ref>] --readiness-file readiness.json` plus `transist runs start <run-id>` when staged inspection matters
    - `transist runs status <run-id>` for the quick human-readable summary
    - `transist runs show <run-id>`
5. Confirm reporting behavior when applicable:
    - usage and token data exist only when the adapter or plugin reports `usage`
    - plugins that wrap a model-backed tool populate `usage` with the canonical keys (`input_tokens`, `output_tokens`, `total_tokens`, plus optional `reasoning_tokens` / `cache_read_tokens` / `cache_write_tokens` / `model`); see `references/usage-reporting.md`
    - when the plugin shells out to another model-backed CLI as a raw subprocess, it extracts usage itself rather than relying on sibling plugin accumulators
    - usage extraction is best-effort and never fails the primary action
    - a `usage.json` artifact hint is emitted whenever `usage` is populated
    - numeric plugin or adapter metrics appear in summaries without becoming first-class Prometheus families
6. Return an end-user-facing completion block:
    - files touched or proposed
    - commands run and outcomes
    - blockers, risks, or follow-ups

If a lifecycle or workflow gate fails, report the exact failing command, file, or manifest field and stop before claiming completion.
