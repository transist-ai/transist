# Runtime Home and Settings

## Home Resolution

Transist selects its effective home in this order:

1. `--home <path>`
2. `TRANSIST_HOME`
3. Persisted user default from `transist home set <path>`
4. `${XDG_STATE_HOME:-$HOME/.local/state}/transist`

Use `transist home get` to inspect the effective home and its source. A persisted home is flat across projects; it is not repository-scoped.

Maintained delivery workflows keep intermediate files/trees in bounded root
managed documents, persisted with runtime history outside Git. Process actions
receive private copies; resume retains committed revisions and replay starts a
fresh namespace. Source/OpenSpec deliverables stay in the workspace. Repository
`.transist/` is an optional plugin-specific convention, not home initialization
or the maintained handoff default. Other workflows may explicitly use workspace
files; changing `TRANSIST_HOME` does not relocate or capture those files. Preserve
active-run dependencies: a home backup does not include workspace mutations. See
`docs/public/reference/path-and-resource-semantics.md` in the runtime repository.

## Setup Posture

- `transist home init` defaults to governed mode and an allowlist policy profile.
- Governed homes require trusted workflow and plugin verification plus exact capability grants.
- `standalone` is permissive and accepts unsigned packages; use it only for an explicit local evaluation.
- `development` is an explicit development-only posture.
- `home init` is idempotent and does not rewrite an existing `config.yaml`.

## Configuration Boundaries

- `<home>/config.yaml` is required and owns runtime mode and policy.
- `<home>/settings.yaml` is optional and owns runtime-wide filesystem and network defaults.
- The user-scoped settings file is separate from a home. On macOS it is `~/Library/Application Support/transist/settings.yaml`; use the TUI to manage the documented manual-gate actor setting when practical.
- Runtime homes store public trust roots only. Use `transist-admin` outside the home for private-key creation and signing.

## Safe Inspection

Use `transist --version` and `transist home get` before mutating setup state. For a governed home, inspect and approve policy changes before importing a public root, installing a plugin, or running a workflow.
