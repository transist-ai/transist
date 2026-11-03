# Dependency Update Policy

This repository accepts automated dependency update pull requests, but they do not merge automatically.

## Scope

- Cargo dependencies in this workspace
- GitHub Actions workflow dependencies under `.github/workflows/`
- CI and release executables declared in `scripts/ci/tool-versions.env`

## Cadence

- Dependabot checks Cargo and GitHub Actions dependencies weekly.
- Security advisories are handled out of band when maintainers need to move
  faster than the weekly cadence.

## Review Gate

Every dependency update requires human review before merge.

Review focuses on:

- compatibility with the Rust `1.88.0` build pin and MSRV
- license and advisory impact
- kernel-boundary and public/private dependency guards
- trust, secret, and local-only behavior regressions
- Action references must remain pinned to reviewed 40-character commit SHAs.
- Binary downloads must retain exact versions and SHA-256 verification.
- Cargo-installed tools must retain exact versions and `--locked` installs.
- The CI container images, runner labels, and Debian/Alpine system packages
  (`APT_*_PACKAGE`, `ALPINE_*_PACKAGE`) must remain pinned to exact versions in
  `scripts/ci/tool-versions.env`; runner labels and container images must match
  those pins, and `apt-get`/`apk` installs must go through the pinned
  `install_debian_tools.sh` / `install_alpine_tools.sh` installers.
- npm-managed tools (e.g. the Redocly CLI) install from the committed lockfile
  in `scripts/ci/redocly/package-lock.json` via `npm ci`, never from a live
  `npm install -g`; the lockfile is the supply-chain integrity source.

No dependency update should bypass the normal review path, and security-related
updates do not auto-merge.

## Required Validation

Dependency pull requests must pass the normal PR validation workflow plus the
dedicated security regression suite:

```bash
bash scripts/ci/run_security_regression_suite.sh
```

CI runs that suite under the Rust `1.88.0` toolchain. This build pin and MSRV
remains authoritative until an explicit policy change. A dependency bump that
requires a newer compiler is treated as an MSRV violation and must not merge
without that policy update.

Supply-chain changes must also pass:

```bash
bash scripts/ci/check_supply_chain_pins.sh
bash scripts/ci/check_licenses.sh
```

## Security Regression Coverage

The regression suite keeps the highest-risk governed-runtime behaviors under
direct test:

- runtime trust verification and trust-store lifecycle
- plugin package hardening for install/sign/verify surfaces
- local-only and policy enforcement behavior
- secret scoping and redaction behavior
- schema-contract drift for public JSON/YAML contracts
- snapshot export contract and redaction behavior

- trusted workflow and plugin signature verification
- reserved workflow `v0` and plugin `0` / `v0` marker denial in release-style builds;
  ordinary `0.x.y` releases never bypass trust checks
- local-only enforcement for external providers
- secret scoping and persistence guardrails

## When A Dependency Update Needs Extra Care

Escalate review when an update touches:

- crypto, trust, or signature verification dependencies
- adapter execution or sandboxing dependencies
- HTTP, TLS, or serialization dependencies
- build or release workflow actions

In those cases, maintainers should also inspect the affected public docs and
release verification surface.
