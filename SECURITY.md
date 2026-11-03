# Security Policy

We take the security of Transist seriously.

Related public security docs:

- [Threat Model](docs/public/security/threat-model.md)
- [Release Artifact Verification](docs/public/reference/release-artifact-verification.md)
- [Platform Distribution Signing](docs/public/reference/platform-distribution-signing.md)

## How to Report a Vulnerability

Do not disclose vulnerabilities in a normal issue, pull request, or other
non-private channel.

### Supported Versions

Transist is in public beta (pre-1.0). Security support applies to released
versions only:

- The **latest patch of the current minor** receives full security support.
- The **previous minor** receives security-only support for **90 days** after
  a new minor is released, after which it is unsupported.
- Pre-release, development, and source builds outside a published release tag
  are not supported; reproduce and report against the latest patch of the
  current minor.

See [Compatibility and Stability](docs/public/reference/compatibility-and-stability.md)
for the version-support policy.

### Reporting Channels

- **`security@transist.ai`** is the permanent fallback reporting channel and
  is always available. Prefer it when in doubt.
- **GitHub private vulnerability reporting** at
  <https://github.com/transist-ai/transist/security/advisories/new> may be used
  only when it is enabled on the repository. If GitHub private reporting is
  unavailable, disabled, or unsuitable, email `security@transist.ai` instead.

### Response Targets

These are non-binding targets, not an SLA:

- Acknowledge reports within **3 business days**.
- Complete initial triage within **7 business days**.

Fix timing, release timing, and disclosure timing are determined by maintainers.

### What to Include

Include as much detail as possible:

- affected version or commit
- impact assessment
- reproduction steps
- suggested mitigations if available

## Handling

Reports are handled privately. Reports received via `security@transist.ai` are
triaged by maintainers with security authority. Reports received via GitHub
private reporting are triaged through the same process. Fix timing, release
timing, and disclosure timing are determined by maintainers.

## CI Security Scanning

`Security Scan / security-gate` always requires `security-audit` to succeed:
security regressions, dependency/license checks, npm audits and full-history
secret scanning. CodeQL runs only when `github.event.repository.private` is
`false`; public repositories require its success. Private repositories require
CodeQL to be skipped and receive audit-only evidence, not a CodeQL scan.
Unexpected results or malformed visibility fail the gate.

GitHub security features are intentionally disabled while the repository is
private to control costs; maintainers enable them when making it public. This
workflow does not enable features or use CodeQL on unlicensed private code.
Local CI checks the policy contract but cannot verify hosted CodeQL analysis,
uploads or repository settings; see [Local CI Preflight](docs/developer/local-ci-preflight.md).

## Trust Boundary

- Private keys, key generation, and signing flows do not belong in the
  `transist` runtime repository.
- Trust creation lives in the monorepo `transist-admin` CLI.
- The runtime side is verification-only: it imports `transist.trust_root.v1`
  public keys, verifies detached `transist.signature.v1` sidecars, and never
  stores private key material.
- Current workflow signing covers the authored definition set rooted at the
  workflow file: the workflow file itself plus referenced subflow files,
  referenced `prompt_file` targets, and `source://` helper scripts, each
  LF-normalized before hashing. Templated `prompt_file` paths are refused at
  sign time. Current plugin signing uses `plugin_dir_sha256` over exactly
  `plugin.yaml` plus the manifest-declared `package.files`; unselected files
  are outside the signed payload.
- Imported trust roots are stored locally under
  `<home>/trust/keys/<key_id>.pub`. Example trust roots are public demo material
  and do not establish trust for real environments or change the runtime/admin
  trust boundary.
- Governed homes automatically require trusted workflow/plugin verification for
  install, prepare, workflow run, resume, replay, cleanup, and preflight.
  On standalone or development homes, trusted execution is opt-in via
  `--trusted-execution` on workflows install, plugins install, prepare, workflow run,
  run, and `runs preflight`.
- Local-only mode is opt-in via `--local-only` on prepare, workflow run,
  replay/resume/retry/cleanup, and `runs preflight`.
- Local-only mode relies on plugin-manifest `provider_kind` declarations. It
  denies `external` actions before dispatch and records additive local
  provenance/evidence about allowed external calls in non-local-only runs.
- External-transfer evidence is operational audit data, not a DLP-grade
  redaction or legal-compliance guarantee. `redaction_applied = false` is the
  honest fixed value.
- In trusted-execution mode the runtime fails closed before dispatch when a
  workflow or referenced plugin is unsigned, untrusted, modified, or malformed.
- Snapshot-backed fresh run birth may retain resolved prompt and `source://`
  helper bytes in private `workflow_assets/` storage. These captures are not
  exposed through inspection or snapshot exports and are the source for
  snapshot-backed dispatch and re-entry. Workflow-owned files must not contain
  secrets.
- Each trusted-execution run persists a `transist.trust_decision.v1` decision on
  the run record before execution continues or is refused.
- Workflow installation is not an authority-granting operation. Governed and
  trusted installs verify and persist a signed package, but never bind inputs,
  write policy, or create a run. Operators review prospective inputs with
  `workflows check`, apply only reported exact missing grants with `workflows
  authorize`, and recheck before preparing or running.

If you find runtime code that introduces signing helpers, private-key encoding,
or keygen logic, treat it as a boundary violation and report it through the same
private path.

## Scope Notes

- The runtime aims to be inspectable, auditable, and fail-closed under trusted
  execution. It does not claim to be tamper-proof against a local administrator.
- Release artifact signing is a distribution concern documented separately from
  runtime workflow/plugin trust verification.
