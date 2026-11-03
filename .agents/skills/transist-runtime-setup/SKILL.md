---
name: transist-runtime-setup
description: Guide a beginner through installing Transist and configuring a safe runtime home. Use when a user mentions `TRANSIST_HOME`, `home init`, `home set`, `config.yaml`, `settings.yaml`, or first-run onboarding; hand off first-workflow setup to `transist-workflow-setup`.
---

# Transist Runtime Setup

Guide one new user through a safe Transist installation and runtime-home setup.
Use plain language first and introduce Transist terms only when needed.

## Default Workflow

Follow these rules in order:

1. Ask exactly one decision question per turn. Reuse facts already supplied or
   observed; do not ask them again.
2. State what is known, why the next decision matters, recommend one option,
   and ask one decision question only when more than one viable option exists.
3. A selected option authorizes deterministic non-trust actions for that
   option. Explain them before running commands, but do not ask again for each
   command.
4. Run validation and other read-only commands without permission prompts.
   Ask for immediate explicit consent only before a trust mutation: importing a
   trust root or creating, reading, exporting from, or using a private key.
5. Show every command before running it. Mark it `read-only`, `state-changing`,
   `network`, `privileged`, or `uses a private key`.
6. Never search the whole machine for binaries, checkouts, workflows, plugins,
   public roots, or private keys. Use an exact user-supplied path or one
   selected bounded search root.
7. Never run a workflow as a side effect of installation or home setup.

Use `transist ...` commands by default. If the user explicitly identifies a
Transist source checkout and no installed CLI is available, use
`cargo run -p transist-cli -- ...`. Do not search for a checkout.

## Hard Rules

- Default to a governed per-user home for regular use; use standalone only for
  an explicitly temporary unsigned evaluation.
- Keep private keys outside the runtime home. Never search for, print, or
  persist their contents.
- Keep governed defaults until a selected workflow reveals exact requirements.
  Never grant `/`, the whole home, credentials, a workspace parent, or broad
  write access to bypass an error.
- Never delete or overwrite existing runtime, trust, key, signature, plugin,
  workflow, or run data by default.
- Runtime-home setup does not install a workflow. After the home is ready,
  hand off all workflow selection, plugin dependencies, signing, trust,
  promotion, policy, capability, and test work to `transist-workflow-setup`.

### Plain-Language Choices

Start with this question unless the user already gave the answer:

> What are you trying to do: set up Transist for regular use, try a temporary demo, or repair an existing setup?

- **Regular use:** create a per-user home shared across projects. New homes use
  governed mode, which requires signed software and explicit permissions.
- **Temporary demo:** create a separate disposable home. Standalone mode accepts
  unsigned local examples and is not a shortcut for governed setup.
- **Repair:** inspect the existing home and change nothing until the problem and
  proposed change are clear.
- **Workflow:** a YAML file describing work Transist will run.
- **Plugin:** an installed package that provides workflow actions.
- **Public trust root:** a public file used to recognize an approved signer.
- **Governed mode:** the runtime checks signatures and exact permissions before
  execution.

## Installation

Skip this branch when `transist --version` succeeds or the user supplies an
exact path to a known working binary.

1. Ask whether the user has an extracted release, release archive, known source
   checkout, or none. Ask for one exact path; never search for one.
2. If no binary or source is available, recommend the documented release path
   and ask whether to use it or stop.
3. Determine the platform with a read-only check, then use only these release
   targets: Linux x86-64 glibc `x86_64-unknown-linux-gnu`, Linux x86-64 musl
   `x86_64-unknown-linux-musl`, and macOS Apple Silicon
   `aarch64-apple-darwin`.
4. Stop on Windows, Intel macOS, ARM Linux, or another unsupported target. Do
   not select an archive speculatively.
5. Verify a release archive checksum and minisign signature before extraction
   or execution. Stop on any mismatch.
6. For an extracted release, use its known `bin` directory. On Linux keep
   `transist-sandbox-launcher` beside `transist`.
7. For a source checkout, require its exact root, confirm expected workspace
   files, and build with the pinned Rust toolchain only after the user selects
   the source-build option. Do not change the global Rust default.
8. An extracted release also includes the complete Git-tracked
   `.agents/skills/` tree as ancillary authoring guidance.
9. Confirm with `transist --version`, `transist-server --version`, and
   `transist-admin --version` when the user wants all release tools available.

Use the installation reference for exact verification commands and supported
platform details.

## Runtime Home Setup

### Inspect Current State

Run this read-only batch and summarize only fields needed for the next choice:

```bash
transist --version
transist home get
```

If no home resolves, explain that this is normal before first setup. Do not
initialize automatically.

### Choose the Home

Before any `transist home init` command, ask whether the user wants:

1. A **global per-user home**. Recommended for regular use and shared across
   this user's projects.
2. A **specific folder**. Ask for its exact absolute path and explain that it
   is a Transist home, not an adapter workspace.

Do not infer this choice from CWD. A selected home cannot be inside, or contain,
the workspace that Transist will run from. Home resolution is `--home`,
`TRANSIST_HOME`, persisted `transist home set`, then
`${XDG_STATE_HOME:-$HOME/.local/state}/transist`.

Before initialization, show the exact path, mode, files created, whether the
default will persist, and whether existing data will be reused.

```bash
transist home init --mode governed
transist home init --mode governed --home <absolute-home>
transist home set <absolute-home>
transist home init --mode standalone --home <temporary-absolute-home>
```

`home set` persists the per-user default and initializes a missing target.
`home init` is idempotent and does not rewrite a valid existing `config.yaml`.
For a temporary demo, use the same explicit `--home` path on later commands and
ask at the end whether to retain or delete it.

### Configure Runtime-Wide Policy Only When Needed

- `<home>/config.yaml` defines runtime mode and policy. Keep governed defaults
  until a workflow supplies exact requirements.
- `<home>/settings.yaml` defines runtime-wide filesystem and network defaults.
  External path grants must be specific.
- User-scoped settings are separate from the home. On macOS the documented path
  is `~/Library/Application Support/transist/settings.yaml`.

Resolve the effective home, show the exact file and minimal proposed change,
explain its security effect, and ask whether to apply it. Verify an applied
runtime-policy edit with:

```bash
transist policy validate
transist policy show --effective
```

## First Workflow Handoff

After home setup succeeds, ask exactly:

> Would you like to install your first workflow now?

If no, report the runtime setup state and stop. If yes, invoke
`transist-workflow-setup` and pass the effective home, runtime mode, available
`transist` and `transist-admin` paths, and any known release or repository
root. That skill owns workflow selection, bounded plugin discovery, signing and
public-root trust, authored validation, package installation, prospective
readiness checks, exact authorization and rechecks, live preparation, and
optional explicit test runs.

## Verification and Completion

Use the narrowest read-only check matching each mutation:

- Home selection/init: `transist home get` and mode/configuration inspection.
- Runtime policy: `transist policy validate` and `transist policy show --effective`.
- Binary installation: `transist --version` and the requested companion binary
  version checks.

Finish with installed binary/source version, effective home and resolution
source, runtime mode, whether a per-user default was persisted, temporary paths
and retain/delete decision, and the next workflow-setup handoff.

## Troubleshooting

- Unexpected home: inspect `transist home get`, then explain `--home`,
  `TRANSIST_HOME`, persisted default, and computed default in that order.
- Working storage: maintained delivery flows use root managed documents for
  intermediate files/trees outside Git and keep source/OpenSpec deliverables in
  the workspace. Repository `.transist/` is an optional plugin-specific path
  convention, not another runtime home or the maintained handoff default. Other
  workflows may explicitly use mutable workspace files; home selection does not
  relocate or capture them. Preserve files needed by active runs and refer to
  `docs/public/reference/path-and-resource-semantics.md`.
- Missing Linux `bwrap`: explain that sandboxed process/MCP execution cannot
  proceed and ask the user to install it with their package manager.
- Existing home data: inspect and ask whether to reuse, back up, or choose a
  new path. Never delete it during setup.
- Workflow, plugin, signing, trust, capability, or preflight issue: hand off to
  `transist-workflow-setup`; do not duplicate or improvise that lifecycle here.

## Source Priority

Use facts in this order when guidance conflicts:

1. `docs/public/reference/cli.md`
2. `docs/public/reference/storage-layout.md`
3. `docs/public/guides/getting-started/installation-and-local-setup.md`
4. `docs/public/reference/release-artifact-verification.md`
5. `docs/public/reference/supported-platforms.md`
6. `docs/public/guides/inspection/operating-the-transist-home-directory.md`

## References

- [Runtime home and settings](references/runtime-home-and-settings.md)
- [Runtime installation](references/runtime-installation.md)
