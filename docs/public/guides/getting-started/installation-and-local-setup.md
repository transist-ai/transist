# Guide: Installation and Local Setup

This guide is for technical evaluators and operators installing the `transist` command-line tool and setting up a first local environment.

## 1. Install the `transist` CLI

First, you need the `transist` binary for your operating system.

1. Download the archive that matches your platform (Linux x86-64 or macOS
    Apple Silicon) from the
    [GitHub releases page](https://github.com/transist-ai/transist/releases).
2. Extract the archive. Every archive has a `bin` directory containing
    `transist`, `transist-server`, and `transist-admin`. Linux archives also
    contain the internal `transist-sandbox-launcher`; macOS archives do not.
    Archives also contain complete Git-tracked `.agents/skills/` guidance.
3. Add the extracted `bin` directory to your `PATH`:

    ```bash
    export PATH="$PWD/bin:$PATH"
    ```

    To copy the executables into `/usr/local/bin`, run this from the extracted
    archive root on Linux:

    ```bash
    sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
      bin/transist-sandbox-launcher /usr/local/bin/
    ```

    On macOS Apple Silicon:

    ```bash
    sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
      /usr/local/bin/
    ```

    On Linux, keep `transist-sandbox-launcher` beside `transist`; the runtime
    discovers it there for loopback sandbox setup.

Windows is not supported. See [Supported Platforms](../../reference/supported-platforms.md) for the exact release target matrix.

To confirm the installation, open a **new terminal** and run:

```bash
transist --version
transist-server --version
transist-admin --version
```

If each prints a version number, the binaries are available on your path.

## 2. Set Up Your Local Environment

Transist stores all of its data—plugins, run histories, and logs—in a "home directory." By default, it resolves to a per-user default path outside your project (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`), shared across every project on this machine. Let's create one.

1. **Create a project folder:**
    It's best to start with a fresh directory for your project.

    ```bash
    mkdir my-transist-project
    cd my-transist-project
    ```

2. **Initialize the home directory:**
    Run the `home init` command. With no `--home` flag, this creates the
    per-user default home and records it as your persisted user home.

    ```bash
    transist home init
    ```

    You should see a message like `initialized /home/user/.local/state/transist`. New homes are governed
    by default. For an explicit non-governed home, use `transist home init --mode
    standalone` or `--mode development`. The fullscreen TUI shows home state but
    does not initialize or change homes.

## 3. Understand the Home Directory

The `transist` home directory is the heart of your local setup. It contains everything:

- `config.yaml`: Default settings and policies.
- `plugins/`: All your installed plugins.
- `runs/`: The complete history and artifacts for every workflow run.

This local-first design means you have full control. You can back it up, inspect it, or even delete it just by managing this one directory.

Typical `config.yaml` policy settings include:

```yaml
version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
  child_env:
    inherit: false
    allowlist: ["PATH", "HOME", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR"]
```

Add version-pinned `policy.allow.plugin_grants` for plugin actions and exact
`policy.allow.capabilities` for secrets, filesystem, network, and approvals
before running governed work. The runtime rejects unsigned or ungranted work
before dispatch. Existing explicit standalone/development homes may retain
`profile: permissive`; capability enforcement is then recorded as bypassed. A
missing `config.yaml` is an error.

### Changing the Home Directory Location

Sometimes, you might want a specific home path instead of the computed per-user default.

- **User-level default at an explicit path:** Use `transist home set` to persist a specific shared home path instead of the per-user default.

    ```bash
    # All commands on this machine will use this path unless overridden.
    transist home set ~/my-transist-data
    ```

- **Environment Variable:** Set the `TRANSIST_HOME` environment variable for more temporary control.
- **Command-Line Flag:** Use the `--home <path>` flag on any `transist` command to override all other settings.

To check which home directory is currently active, run `transist home get`.

## 4. Linux prerequisite — bubblewrap

On Linux, the bundled `shell` plugin uses `bwrap` (bubblewrap) to sandbox
child processes. Install it before running any workflow:

| Distribution          | Install command                            |
| --------------------- | ------------------------------------------ |
| Debian / Ubuntu       | `sudo apt-get install bubblewrap`          |
| Fedora / RHEL         | `sudo dnf install bubblewrap`              |
| Arch Linux            | `sudo pacman -S bubblewrap`                |
| Alpine                | `sudo apk add bubblewrap`                  |

macOS uses `sandbox-exec` from the system; no separate install is required.

## 5. macOS — Gatekeeper and quarantine

macOS archives are signed with minisign and carry SHA-256 checksums, but they
are **not** Apple Developer ID signed or notarized. Gatekeeper may quarantine
the downloaded archive or the extracted binaries.

**Verify before you remove the quarantine.** Confirm the archive is authentic
first:

1. Download the archive and its per-archive `.sha256` checksum and `.minisig`
   signature sidecar files
   from the [GitHub releases page](https://github.com/transist-ai/transist/releases).
2. Verify the SHA-256 checksum and the minisign signature as documented in
   [Release Artifact Verification](../../reference/release-artifact-verification.md).
3. Compare the committed public key (`transist-release.minisign.pub` in this
   repository) byte-for-byte with any downloaded copy before trusting it.

Once you have confirmed the archive is authentic, remove the quarantine
attribute from the extracted binaries so Gatekeeper allows them to run:

```bash
xattr -d com.apple.quarantine bin/transist
xattr -d com.apple.quarantine bin/transist-server
xattr -d com.apple.quarantine bin/transist-admin
```

Alternatively, right-click each binary in Finder and choose **Open** the first
time to accept the Gatekeeper prompt interactively.

## 6. Install Your First Plugin

Plugins provide the actions that workflows invoke. The demo bundle or source checkout includes a bundled `shell` plugin (`default_provider_kind: local`) used by the getting-started examples. A detached `plugins/shell.sig` sidecar ships next to it.

**Standalone or development homes** (`--mode standalone` or `--mode development`)
use permissive policy; unsigned install works directly:

```bash
# From the extracted archive root (or source checkout root).
transist plugins install --path plugins/shell/plugin.yaml
```

**Governed homes** (the default) require trusted execution. Import the
approving public root first, then install with `--trusted-execution`. The
[Trusted Governed Execution Walkthrough](../trust/trusted-execution-walkthrough.md)
ships a demo-only public root and covers the full signed-install → trusted-run
loop. Create production trust on an admin host with `transist-admin`; do not
place private keys in the runtime home.

```bash
# Import the approving public trust root (demo root shown).
transist trust import examples/quickstart/trust/quickstart-demo.pub

# Install the signed shell plugin (the .sig sidecar is verified).
transist plugins install --path plugins/shell/plugin.yaml --trusted-execution
```

Verify the installation by listing all installed plugins:

```bash
transist plugins list
```

## You're All Set

You now have a working local Transist installation ready to run workflows.

**Next Up:** Head to the [**Quickstart Guide**](quickstart.md) to run your
first workflow in standalone mode, or to the
[Trusted Governed Execution Walkthrough](../trust/trusted-execution-walkthrough.md)
for the full offline signed trust loop.

## Troubleshooting

### `bwrap` not found (Linux)

If you see `bwrap: No such file or directory` or
`failed to sandbox process: bwrap is not available` when running a workflow,
`bubblewrap` is not installed. Install it with the command for your
distribution shown in the Linux prerequisite section above.

Sandboxing is unconditional: Linux process and MCP execution requires `bwrap`
in every mode, and no `transist home init` mode disables it. If you are in a
rootless container or a CI environment where `bwrap` cannot be installed or
run, that environment is unsupported for Linux process execution.

### `"transist" cannot be opened because it is from an unidentified developer` (macOS)

macOS Gatekeeper is blocking the binary. Verify the archive is authentic first
(see the macOS Gatekeeper section above), then remove the quarantine attribute:

```bash
xattr -d com.apple.quarantine bin/transist
xattr -d com.apple.quarantine bin/transist-server
xattr -d com.apple.quarantine bin/transist-admin
```

Or right-click the binary in Finder and choose **Open** to accept the one-time
Gatekeeper prompt.
