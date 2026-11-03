# Platform Distribution Signing

This document records the end-user distribution signing posture for the
public release line.

## Current State

See [Supported Platforms](supported-platforms.md) for the exact release matrix.
The current release targets are `x86_64-unknown-linux-gnu`,
`x86_64-unknown-linux-musl`, and
`aarch64-apple-darwin`. All archives are
packaged deterministically on one pinned Linux job and verified with minisign
signatures and SHA-256 checksums.

Releases are published through draft-first atomic publication: assets are
staged to a draft release, verified against a signed release manifest, and
then transitioned to public in one final step. See
[Release Artifact Verification](release-artifact-verification.md)
for details.

## Goals

- let users verify that downloaded artifacts are authentic
- publish enough metadata to audit what was built and shipped
- keep key custody separate from normal runtime operation

## Cross-Platform Baseline

Every release publishes:

- checksums for all artifacts
- detached minisign signatures for release archives
- a signed, sorted release manifest listing every payload asset's SHA-256
  digest (archives, checksum files, archive signatures, per-target build
  provenance, SBOM, and the committed verification key)
- an SBOM for each release
- the committed `transist-release.minisign.pub` verification key
- build provenance or attestations bound to the published digests
- separate final-packaging provenance identifying the pinned Linux packager

## macOS Plan

macOS archives currently have minisign and checksum verification but are not
Apple Developer ID signed or notarized. The plan:

- sign release binaries with Developer ID
- enable hardened runtime where required by notarization
- notarize distributable archives before publication
- document `codesign` and notarization verification steps in release notes

Apple Developer ID signing and notarization will ship as a separate capability;
until then, macOS users verify archives with minisign and SHA-256 checksums.

## Linux Plan

- publish detached signatures and checksums for tarballs
- sign package-repository metadata when native packages are offered
- document `sha256sum` and signature verification steps in release notes

## Unsupported Targets

Windows and other targets not listed in
[Supported Platforms](supported-platforms.md) are unsupported. No binary,
installer, signing path, or path example is published for unsupported targets.

## Key Custody and Rotation

- release-signing keys stay outside the runtime workspace and runtime binaries
- key custody is limited to release maintainers
- CI minisign signing uses a dedicated release key stored only in repository or
  organization secrets so signing remains non-interactive
- the committed `transist-release.minisign.pub` (key ID
  `9B91C4C6F8E61392`) is the only verification-key source; verification docs
  require a byte-for-byte comparison before trusting a downloaded key
- key rotation and compromise response are documented alongside the release playbook
- compromised keys trigger signature revocation guidance and a fresh key rollout

Archive signing is intentionally separate from packaging. The packaging job
creates unsigned archives, validates their contents, and records packaging
provenance. The signing job then signs those exact bytes. No post-sign or
downloaded-draft validation accepts unsigned archives.

## Rollback and Rebuild Guidance

- release notes identify superseding builds when an artifact is withdrawn
- verification docs explain how to confirm replacement artifact digests
- rebuild guidance points users to source tags, checksums, signatures, SBOMs,
  and published attestations

## Non-Goals

- this plan does not move signing into the runtime
- this plan does not imply reproducible builds are already complete for every target
- this plan does not replace the runtime trust model for workflows and plugins
