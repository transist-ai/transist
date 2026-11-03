# Transist @TRANSIST_RELEASE_TAG@

This archive is the minimal **Transist evaluation bundle** for @TRANSIST_RELEASE_TAG@. It
contains the runtime binaries, the bundled `shell` plugin, the offline
quickstart fixture, and the getting-started demo workflows. Full documentation
is published online at [transist.ai/docs](https://transist.ai/docs) and is not
bundled; `.agents/skills/` is the complete Git-tracked ancillary guidance
exception.

**Release:** [@TRANSIST_RELEASE_TAG@ on GitHub](https://github.com/transist-ai/transist/releases/tag/@TRANSIST_RELEASE_TAG@)

---

## What's In This Archive

| Path | Purpose |
|---|---|
| `bin/transist` | The runtime CLI: check, install, run, inspect, and recover workflows. |
| `bin/transist-server` | The read-only companion inspection server (HTTP + Prometheus metrics). |
| `bin/transist-admin` | The admin trust-creation CLI: keys, trust roots, and detached signatures. Runtime verification stays separate from trust creation. |
| `bin/transist-sandbox-launcher` | **Linux only.** Internal loopback network sandbox launcher used by the bundled `shell` plugin. |
| `plugins/shell/` | The bundled `shell` plugin (`@SHELL_PLUGIN_VERSION@`, `default_provider_kind: local`) plus plugin-local schemas. |
| `plugins/shell.sig` | Detached demo-only signature over the selected `shell` payload: `plugin.yaml` plus its manifest-declared `package.files`. |
| `examples/quickstart/` | The offline quickstart fixture: signed workflow, governed config template, and demo-only public trust root. |
| `workflows/demos/getting_started/` | Getting-started demo workflows. |
| `.agents/skills/` | Complete Git-tracked ancillary authoring guidance; not executable runtime content or trust material. |
| `README.md` | This file (the distribution-specific README). |
| `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` | Legal notices. |
| `build-provenance.json`, `packaging-provenance.json` | Build and packaging provenance. |

No private key material is ever shipped in an archive.

## Installation

### macOS (Apple Silicon)

```bash
tar -xzf transist-@TRANSIST_RELEASE_TAG@-aarch64-apple-darwin.tar.gz
cd transist-@TRANSIST_RELEASE_TAG@-aarch64-apple-darwin
sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
  /usr/local/bin/
```

### Linux (x86-64, glibc or musl/Alpine)

```bash
tar -xzf transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu.tar.gz
cd transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu
sudo install -m 0755 bin/transist bin/transist-server bin/transist-admin \
  bin/transist-sandbox-launcher /usr/local/bin/
```

**Linux launcher requirement:** the sandbox launcher must remain co-located
beside `transist`. Keep it in the same directory (e.g. `/usr/local/bin/`) after
installing, exactly as shown above.

Verify the install:

```bash
transist --version
transist-server --version
transist-admin --version
```

Supported platforms (six release targets):

- Linux x86-64: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`.
- Linux ARM64: `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`.
- macOS Apple Silicon: `aarch64-apple-darwin`.
- macOS Intel: `x86_64-apple-darwin`.

See
[Supported Platforms](https://transist.ai/docs/reference/supported-platforms)
for the full release matrix.

## Minimal Standalone Quickstart

The quickstart runs fully offline using only the bundled plugin and workflow.
From the extracted archive root:

Installed checks and authorization validate without plugin probes. Passing
validation leaves readiness `not_ready`; use `check <id>@<version> --live` or
`--output readiness.json` for live readiness. Preparation and execution still
check live readiness before creating a run.

```bash
export TRANSIST_HOME="$(mktemp -d -t transist-quickstart-XXXXXX)"
transist home init --mode standalone --home "$TRANSIST_HOME"

transist plugins install --path plugins/shell/plugin.yaml
transist workflows install --path examples/quickstart/quickstart.workflow.yaml
printf '{"subject":"demo"}\n' > quickstart-inputs.json
transist workflows check quickstart.demo@0.1.0 --inputs-file quickstart-inputs.json
transist workflows run quickstart.demo@0.1.0 --run-ref qs --inputs-file quickstart-inputs.json --ui plain
transist runs show qs

rm -f quickstart-inputs.json
rm -rf "$TRANSIST_HOME"
```

For the full signed offline trust loop (trust import, governed home, cancel,
retry, evidence export), follow the
[Trusted Governed Execution Walkthrough](https://transist.ai/docs/guides/trust/trusted-execution-walkthrough).

The getting-started demo workflows under `workflows/demos/getting_started/` are
bundled for you to run and modify.

## Verify Your Download

Always verify an archive before using it. Verify the checksum, the minisign
signature, the release manifest, and (when the release publishes them) the
GitHub attestations.

```bash
# Checksum
shasum -a 256 -c transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu.tar.gz.sha256

# Minisign signature (use the committed key from the release, never one from
# the release page alone)
minisign -Vm transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu.tar.gz \
  -x transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu.tar.gz.minisig \
  -p transist-release.minisign.pub

# Release manifest
minisign -Vm release-manifest.txt \
  -x release-manifest.txt.minisig \
  -p transist-release.minisign.pub

# GitHub attestation (when the release repository publishes them)
gh attestation verify transist-@TRANSIST_RELEASE_TAG@-x86_64-unknown-linux-gnu.tar.gz \
  --repo transist-ai/transist
```

Full verification instructions are in
[Release Artifact Verification](https://transist.ai/docs/reference/release-artifact-verification).

## Documentation

Full documentation is published online and is **not** bundled in this archive.
The complete Git-tracked `.agents/skills/` tree is the ancillary guidance
exception:

- [Documentation home](https://transist.ai/docs)
- [Quickstart](https://transist.ai/docs/guides/getting-started/quickstart)
- [Trusted Governed Execution Walkthrough](https://transist.ai/docs/guides/trust/trusted-execution-walkthrough)
- [Release Artifact Verification](https://transist.ai/docs/reference/release-artifact-verification)
- [Supported Platforms](https://transist.ai/docs/reference/supported-platforms)

## License

MPL-2.0. See `LICENSE`, `NOTICE`, and `THIRD_PARTY_NOTICES.md` in this
archive.
