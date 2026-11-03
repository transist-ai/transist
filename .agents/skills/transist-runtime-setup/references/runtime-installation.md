# Runtime Installation

## Supported Release Targets

- Linux x86-64 with glibc: `x86_64-unknown-linux-gnu`.
- Linux x86-64 with musl, including Alpine: `x86_64-unknown-linux-musl`.
- macOS Apple Silicon: `aarch64-apple-darwin`.
- Windows, Intel macOS, ARM Linux, and other targets have no supported release archive.

## Safe Release Flow

1. Obtain the exact archive, checksum, minisign sidecar, and repository-pinned release public key.
2. Verify the checksum before extraction:

   ```bash
   shasum -a 256 -c transist-<tag>-<target>.tar.gz.sha256
   ```

3. Verify the archive signature before extraction:

   ```bash
   minisign -Vm transist-<tag>-<target>.tar.gz \
     -x transist-<tag>-<target>.tar.gz.minisig \
     -p transist-release.minisign.pub
   ```

4. Extract only after both checks pass.
5. Use the extracted `bin` directory or ask before copying binaries to `/usr/local/bin`.
6. On Linux, keep `transist-sandbox-launcher` beside `transist`.
7. On macOS, remember release archives are not Apple Developer ID signed or notarized. Ask before removing quarantine.
8. The extracted archive includes the complete Git-tracked `.agents/skills/`
   tree as ancillary guidance, not executable runtime content or trust material.

## Source Checkout

- Require the exact checkout root from the user.
- Confirm it contains `Cargo.toml`, `transist-admin/`, `crates/`, and the required package files.
- Use the pinned Rust toolchain from `rust-toolchain.toml`.
- Build after the user selects the source-build path; do not change the global Rust default.
- Use `cargo run -p transist-cli -- ...` and `cargo run -p transist-admin -- ...` from that root.

## Choice Boundaries

- Ask the user to choose release download, an existing archive, a source checkout, or stopping setup.
- After the user selects one path, perform its deterministic non-trust steps without asking before each command.
- Ask again only when a viable alternative appears, such as macOS quarantine removal versus stopping setup, or a persistent versus session-only `PATH` change.
- Trust mutations remain separate: signing Transist artifacts or importing a trust root require immediate explicit consent. Signature verification is read-only and needs no permission prompt.
