# Supported Platforms

This page defines the exact platform targets for official Transist release
archives. The supported-platform table matches the release matrix exactly.

## Supported Release Targets

Artifact identity is the Rust target triple, not a runner or Linux distribution.

| Target triple | OS | Architecture | ABI |
|---|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | x86-64 | glibc |
| `x86_64-unknown-linux-musl` | Linux | x86-64 | musl |
| `aarch64-apple-darwin` | macOS | ARM64 | Darwin |
| `x86_64-apple-darwin` | macOS | x86-64 | Darwin |
| `aarch64-unknown-linux-gnu` | Linux | ARM64 | glibc |
| `aarch64-unknown-linux-musl` | Linux | ARM64 | musl |

## Build And Validation Environments

| Target triple | Native build environment | Native archive validation environment |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` hosting pinned Bookworm | same native GNU environment |
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` hosting pinned Alpine | same native musl environment |
| `aarch64-apple-darwin` | `macos-15` | `macos-15` |
| `x86_64-apple-darwin` | `macos-15-intel` | `macos-15-intel` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` hosting pinned Bookworm | same native GNU environment |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` hosting pinned Bookworm with native `musl-tools` | same native ARM64 Bookworm / `musl-tools` environment |

An Ubuntu runner hosting a Debian or Alpine container does not create an
Ubuntu-, Debian-, or Alpine-specific artifact. The container is build evidence;
the archive name and compatibility contract remain the target triple.

ARM64 musl uses a native ARM64 GNU-hosted Rust toolchain with the ARM64 musl
target and `musl-gcc`. Both its build and final-archive smoke run in pinned
Bookworm, as defined by `.github/workflows/release.yml`.

## Installation Selection

| Host | Install this archive |
|---|---|
| Linux x86-64 with glibc | `x86_64-unknown-linux-gnu` |
| Linux x86-64 with musl, including Alpine | `x86_64-unknown-linux-musl` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |
| Linux ARM64 with glibc | `aarch64-unknown-linux-gnu` |
| Linux ARM64 with musl | `aarch64-unknown-linux-musl` |

Each release publishes one archive per target triple:

- `transist-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `transist-<tag>-x86_64-unknown-linux-musl.tar.gz`
- `transist-<tag>-aarch64-apple-darwin.tar.gz`
- `transist-<tag>-x86_64-apple-darwin.tar.gz`
- `transist-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `transist-<tag>-aarch64-unknown-linux-musl.tar.gz`

Musl-only hosts, including Alpine, require the musl archive for their
architecture. Linux container tool versions are pinned in
`scripts/ci/tool-versions.env`. The macOS builds use the numbered `macos-15`
and `macos-15-intel` runner labels; their observed runner image/version and
Xcode version/build are recorded in provenance, but the runner image is not bit-pinned.

GNU compatibility is stated by the required `GLIBC_*` symbol versions observed
in each release binary's provenance and validation records. It is not a claim
to support every distribution that happens to ship glibc.

All six targets are part of the initial `0.1.0` public release.

## Archive Contents

Every archive is a **minimal evaluation bundle** containing exactly:

- `bin/transist` — the runtime CLI
- `bin/transist-server` — the read-only companion inspection server
- `bin/transist-admin` — the admin trust-creation CLI
- `bin/transist-sandbox-launcher` — the internal Linux loopback sandbox launcher (**Linux archives only**; raises the loopback interface inside the `bwrap` network namespace and then drops all capabilities before executing the plugin)
- `plugins/shell/` — the bundled shell plugin (`0.1.0`, `default_provider_kind: local`) with plugin-local schemas
- `plugins/shell.sig` — detached demo-only signature over the bundled shell plugin
- `examples/quickstart/` — the offline trusted local-only quickstart fixture (signed workflow, governed config template, demo-only public trust root, and detached signatures)
- `workflows/demos/getting_started/` — the getting-started demo workflows
- `.agents/skills/` — the complete Git-tracked ancillary authoring guidance
- `README.md` — the distribution-specific README (the only Markdown README in an archive)
- `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md`
- `build-provenance.json` — native target build toolchain, runner, and OS tool versions
- `packaging-provenance.json` — final pinned Linux packaging environment

**Documentation is not archive payload.** Public and developer documentation
live in the source repository; the public documentation is published at
[transist.ai/docs](https://transist.ai/docs). The complete Git-tracked
`.agents/skills/` tree is the deliberate ancillary exception to source-only
documentation. Repository documentation, governance files, global schemas, CI
scripts, developer trees, and retained-tree README files remain excluded.
Skills are ancillary guidance, not a runtime, API, or schema contract.

The exact, machine-checkable file and directory inventory ships in
`scripts/ci/release-archive-inventory.json`; the archive checker, the release
manifest, and this page all derive from that single source, and any drift
fails CI.

No private key material is ever shipped in an archive.

## Deterministic Packaging

All six archives are packaged on one digest-pinned Linux job with sorted
paths, normalized ownership (uid 0, gid 0), normalized file modes, normalized
timestamps (epoch 0), and `gzip -n` to suppress embedded mtime and filename
metadata. Repeated packaging of identical inputs produces identical SHA-256
digests.

## Verification

The release workflow requires each user-facing CLI binary to exit 0 with
`--help` and `--version` on its target OS, and checks `--version` against the
expected release version. It defines native archive-smoke jobs for all six
targets before signing or uploading release assets. Archive checksums are
verified before extraction, and binary architecture is checked against the
target triple. Exact-tag workflow results provide the per-release evidence.

Native build jobs upload only raw binaries and build provenance. The pinned
Linux packaging job creates all six final archives. Native Linux glibc,
x86-64 Alpine musl, ARM64 Bookworm/musl-tools, and both macOS jobs then run the
quickstart and shipped-workflow coverage from those final archives before
signing. The release workflow uses draft-first atomic publication:
assets are built, tested, signed, and uploaded to a draft release, then verified
against a signed release manifest before the draft transitions to public. A
failed build, sign, or check leaves the release unpublished.

Every release stage that can run in a container does so in a digest-pinned
`rust:1.88.0-bookworm` / `rust:1.88.0-alpine` image with exact `apt`/`apk`
package pins from `scripts/ci/tool-versions.env`; the GNU release build,
packaging, and archive smoke all run in the digest-pinned GNU container, never
on a floating or unpinned base.

See [Release Artifact Verification](release-artifact-verification.md) for
checksum, minisign, manifest, SBOM, and attestation verification.

## Unsupported Targets

- **Windows** is not supported. No Windows binary, installer, or signing
  path is published. Windows and other targets not listed above are
  unsupported.
- **Alpine and other musl-only distributions** are supported only through the
  matching `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl` archive.
  Do not install a GNU archive on Alpine.

## macOS Signing Posture

macOS archives have minisign and checksum verification but are not Apple
Developer ID signed or notarized. Apple Developer ID signing and notarization
will ship as a separate capability; until then, macOS users verify archives
with minisign and SHA-256 checksums as documented in
[Release Artifact Verification](release-artifact-verification.md).

### Gatekeeper and quarantine

Because the binaries are not Apple Developer ID signed, macOS Gatekeeper may
quarantine the downloaded archive or the extracted binaries and prevent them
from running.

**Always verify the archive before removing the quarantine.** Confirm the
download is authentic using the minisign signature and SHA-256 checksum (see
[Release Artifact Verification](release-artifact-verification.md)), and compare
the committed public key (`transist-release.minisign.pub` in the repository)
byte-for-byte with any downloaded copy before trusting it.

After verification, remove the quarantine attribute from the extracted binaries:

```bash
xattr -d com.apple.quarantine bin/transist
xattr -d com.apple.quarantine bin/transist-server
xattr -d com.apple.quarantine bin/transist-admin
```

Alternatively, right-click each binary in Finder and choose **Open** the first
time. See also the
[Installation and Local Setup guide](../guides/getting-started/installation-and-local-setup.md)
for step-by-step instructions.

## Related Docs

- [Release Artifact Verification](release-artifact-verification.md)
- [Platform Distribution Signing](platform-distribution-signing.md)
- [Reproducibility Verification](reproducibility.md)
- [Compatibility and Stability](compatibility-and-stability.md)
