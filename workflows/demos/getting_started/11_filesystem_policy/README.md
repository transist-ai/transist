# Demo 11: Filesystem Policy

This demo shows the workspace-scoped filesystem policy contract for local `process` execution,
including the top-level external path grants and the unsupported `custom` workspace mode.

What it proves:

- A global default `workspace_access.mode: full` allows normal workspace access but still denies host paths outside the workspace.
- A step-level `read_only` override denies workspace mutation.
- `workspace_access.mode: custom` is **not supported**; the runtime rejects it before dispatch with `filesystem_custom_mode_unsupported` on both Linux and macOS.
- Top-level `read_allow_paths` in `settings.yaml` grants read access to paths outside the workspace (e.g. `~/.gitconfig`, `~/.ssh`). These lists are additive across levels. Note: a step-level `filesystem_policy` with only external grants still replaces the workflow-level `workspace_access` mode — set `workspace_access.mode` explicitly on the step if you need to preserve an inherited `read_only`.
- **External grant targets must exist before dispatch on Linux**: both
  `read_allow_paths` and `write_allow_paths` become `bwrap` mounts, so the
  runtime rejects a missing target instead of silently omitting the grant.
  Only grant paths known to be present on the machine that runs the workflow.
- The shipped demo reads home-relative `~/.gitconfig` (granted globally in `settings.yaml`) and `~/.ssh` (granted at the step), so it never depends on a host file such as `/etc/gitconfig`.
- External path grants are additive — step-level entries extend global and workflow-level grants rather than replacing them.
- Persisted run state keeps the effective policy source and `workspace_access` shape for each step.

## Two separate concerns

```yaml
filesystem_policy:
  # Controls access INSIDE workspace_root (whole-policy replacement per level)
  workspace_access:
    mode: full | read_only   # custom is rejected with filesystem_custom_mode_unsupported

  # Controls access OUTSIDE workspace_root (merged/union across all levels).
  # Every entry must exist at run birth.
  read_allow_paths:
    - ~/.gitconfig          # home-relative; ~/ expands to $HOME
    - ~/.config/git
    - ~/.ssh
  write_allow_paths:
    - ~/.cache/git
```

## `custom` mode is unsupported

`workspace_access.mode: custom` and the `read_allow_paths` / `write_allow_paths`
fields inside `workspace_access` were never enforced precisely by either
sandbox backend. The runtime now rejects `custom` uniformly before dispatch
with the `filesystem_custom_mode_unsupported` reason code, on both Linux and
macOS. Workspace-relative glob allow-lists are no longer accepted; use
`read_only` for workspace write protection and the top-level
`read_allow_paths` / `write_allow_paths` for external path grants.

## Run it

```bash
transist home init --mode standalone
transist plugins install --path plugins/shell/plugin.yaml

# Grant ~/.gitconfig globally with the shipped example. Only list entries that
# exist on this machine — every read_allow_paths entry must exist at run birth
# or the run is rejected before dispatch. This demo also requires ~/.ssh to
# exist because its external_path_probe grants that directory at step scope.
cp workflows/demos/getting_started/11_filesystem_policy/settings.example.yaml \
  "$(transist home get)/settings.yaml"

transist workflows check --path workflows/demos/getting_started/11_filesystem_policy/workflow.yaml
transist workflows install --path workflows/demos/getting_started/11_filesystem_policy/workflow.yaml
transist workflows run demo_11_filesystem_policy@0.1.0 --run-ref fs-policy-demo
```

### Prove `custom` mode is rejected

```bash
cat > /tmp/custom-mode.yaml <<'YAML'
schema: transist.flow.v1
id: custom_mode_denied
version: 0.1.0
plugins:
  shell: 0.1.0
steps:
  - id: probe
    uses: process:shell/exec
    filesystem_policy:
      workspace_access:
        mode: custom
        read_allow_paths: [src/**]
    with:
      command: sh
      args: ["-c", "true"]
YAML

transist workflows check --path /tmp/custom-mode.yaml
# exits non-zero with: workspace_access.mode 'custom' is not supported (filesystem_custom_mode_unsupported)
```

The fullscreen TUI displays the effective filesystem policy read-only. Update
runtime policy through the CLI-managed configuration and validate the result:

```bash
transist policy validate
transist policy show --effective
```

## Inspect it

```bash
transist runs status fs-policy-demo
transist runs show fs-policy-demo --tui
```
