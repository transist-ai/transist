# Reproducibility Verification

This document describes how to verify that the published `transist` release
archives match the source at the tagged release commit.

Releases use draft-first atomic publication: assets are staged to a draft
release, verified against a signed release manifest, and then transitioned to
public. The release workflow never creates, moves, or deletes a tag.

## Toolchain

- **Rust:** build pin and MSRV `1.88.0` (edition 2024). The checked-in
  `rust-toolchain.toml` selects this version; it remains authoritative until an
  explicit policy change.
- **Cargo:** ships with the Rust toolchain.
- **Platform:** results are expected to match only the same published target
  triple (`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, or
  `aarch64-apple-darwin`). Build runners and containers are provenance inputs,
  not alternate artifact identities.

## CI Build Environment

- Linux glibc CI jobs use the digest-addressed `rust:1.88.0-bookworm` image
  (`CI_CONTAINER_IMAGE`) recorded in `scripts/ci/tool-versions.env`.
- Alpine/musl CI jobs use the digest-addressed `rust:1.88.0-alpine` image
  (`CI_ALPINE_CONTAINER_IMAGE`) and pin every installed `apk` package (including
  the `bubblewrap` sandbox backend) to an exact version recorded in
  `scripts/ci/tool-versions.env`.
- Debian system tools (`curl`, `unzip`, `nodejs`, `npm`, `bubblewrap`,
  `ripgrep`, `git`, `jq`, `python3`, `pip3`) are installed only inside the
  digest-pinned `CI_CONTAINER_IMAGE` container through
  `install_debian_tools.sh`, which applies the exact `APT_*_PACKAGE` pins from
  `scripts/ci/tool-versions.env`; no workflow calls an unpinned `apt-get
  install`. Every Bookworm container job installs its complete `jq`,
  `python3`, and `pip3` requirements explicitly before use so no job depends on
  tools supplied by the base image or a transitive package.
- Every workflow declares workflow-level `defaults.run.shell: bash` so
  Bash-authored `run` blocks (including `set -euo pipefail`, `[[ ... ]]`,
  arrays, and process substitution) resolve to Bash; GitHub container jobs
  default to `sh`. The only exception is the Alpine bootstrap step, which runs
  before the pinned `bash` package is installed and therefore invokes
  `sh scripts/ci/install_alpine_tools.sh` with `shell: sh` explicitly. The
  supply-chain pin check rejects a missing workflow-level Bash default or a
  missing Alpine bootstrap `shell: sh`.
- The runner labels (`LINUX_RUNNER_IMAGE`, `MACOS_RUNNER_IMAGE`) and the
  container images used by every job and matrix entry must equal the pins in
  `scripts/ci/tool-versions.env`; drift fails the supply-chain pin check.
- Release macOS jobs use the numbered `macos-15` runner label. Each build
  records provenance with the observed runner image/version, OS-provided
  tool versions, and Xcode version/build.
- GitHub-hosted Apple runner images are captured in provenance but are not
  bit-for-bit pinned; image drift can change native linker output.
- GNU compatibility validation records the actual required `GLIBC_*` symbols.
  It does not claim compatibility with every glibc distribution.
- All release archives are packaged on one pinned Linux job (the same
  digest-addressed `rust:1.88.0-bookworm` container) with sorted paths,
  normalized ownership/modes/timestamps, and `gzip -n` for deterministic
  archive metadata. Deterministic archives include the complete Git-tracked
  `.agents/skills/` tree alongside binaries and evaluation assets. Repeated
  packaging of identical inputs produces identical SHA-256 digests.
- `build-provenance-<target>.json` records each native build environment
  (target triple, runner, Rust/Cargo versions, OS tools, and Xcode where
  applicable) and the SHA-256 of every raw binary produced by that build. A
  packaged archive whose binary does not match the recorded digest is rejected.
- `packaging-provenance.json` records the separate pinned Linux container and
  packaging tools that create all final archives.
- `transist-<tag>-sbom.tar.gz` is a deterministic CycloneDX 1.5 SBOM archive:
  documents are normalized (sorted components/services, per-run
  `serialNumber` and `timestamp` stripped) and validated against the pinned
  semantic schema committed at `scripts/ci/schemas/cyclonedx-1.5.schema.json`
  before repackaging. `scripts/ci/test_sbom_reproducibility.sh` proves the
  normalization is byte-for-byte reproducible across independent generations.

## Rebuild from source

```bash
# Clone the tagged release
git clone https://github.com/transist-ai/transist.git
cd transist
git checkout v0.1.0   # use the v-prefixed tag you are verifying

# Confirm the checked-in toolchain pin
rustc --version

# Linux GNU example: build every binary shipped in the Linux archive
cargo +1.88.0 build --locked --release \
  -p transist-cli -p transist-server -p transist-admin \
  -p transist-sandbox-launcher

# The resulting binaries are at:
#   target/release/transist
#   target/release/transist-server
#   target/release/transist-admin
#   target/release/transist-sandbox-launcher
```

## Compare digests

The published release archive is a compressed tarball, not a bare binary.
Compare the locally built binary against the binary extracted from the
published archive:

```bash
# Extract the published archive
tar -xzf transist-v0.1.0-x86_64-unknown-linux-gnu.tar.gz

# SHA-256 of the locally built binary
sha256sum target/release/transist

# SHA-256 of the binary from the published archive
sha256sum transist-v0.1.0-x86_64-unknown-linux-gnu/bin/transist

# Compare the two digests
```

Alternatively, rebuild the complete deterministic archive and compare archive
digests:

```bash
# Record native build provenance (matching the target build environment)
mkdir -p dist
bash scripts/ci/record_build_provenance.sh \
  dist/build-provenance-x86_64-unknown-linux-gnu.json \
  x86_64-unknown-linux-gnu \
  target/release/transist target/release/transist-server target/release/transist-admin \
  target/release/transist-sandbox-launcher

# Record the final packaging environment separately
bash scripts/ci/record_packaging_provenance.sh dist/packaging-provenance.json

# Package the archive
bash scripts/ci/package_release_archive.sh v0.1.0 \
  target/release/transist target/release/transist-server target/release/transist-admin \
  x86_64-unknown-linux-gnu dist/packaging-provenance.json \
  target/release/transist-sandbox-launcher

# Compare the locally packaged archive digest with the published checksum
sha256sum dist/transist-v0.1.0-x86_64-unknown-linux-gnu.tar.gz

# The published checksum file is:
#   transist-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
# from: https://github.com/transist-ai/transist/releases/tag/v0.1.0
```

Repeat the packaging command with the same raw binaries and provenance inputs;
the archive and checksum digests must be identical. Release CI performs this
check in `scripts/ci/test_archive_reproducibility.sh` where the host target is
available.

## Known sources of non-determinism

Rust does not yet guarantee bit-for-bit reproducible builds across all
environments by default. Known variables that can cause digest differences:

| Variable | Effect |
|---|---|
| Rust toolchain minor version | Codegen differences |
| OS / libc version | Linking differences for dynamically linked artifacts |
| `RUSTFLAGS` | Optimization or target changes |
| Build timestamp embedding | Some crates embed `env!("CARGO_PKG_VERSION")` but not timestamps; check with `strings` |

If your digest does not match, verify that your toolchain version, target triple,
and `RUSTFLAGS` exactly match the CI build matrix in `.github/workflows/release.yml`.

## Attestation

GitHub Actions build attestations (SLSA provenance) are attached to each release
once the repository is public. Verify with:

```bash
gh attestation verify transist-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo transist-ai/transist
```

Attestations confirm the artifact was built from the tagged source by the
official CI, not that the binary is bit-for-bit reproducible from a local build.

## Reproducibility Limit

The Rust toolchain and Linux container inputs are pinned, but native build
outputs are not guaranteed to be bit-for-bit reproducible. In particular, the
observed `macos-15` runner image is recorded rather than bit-pinned, and upstream
crate and linker behavior may vary. Any unexplained digest difference between
the published artifact and a rebuild from the recorded environment should be
reported as a potential supply-chain integrity issue via `SECURITY.md`.
