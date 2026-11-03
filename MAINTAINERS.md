# Maintainers

This document defines Transist maintainer governance: appointment, removal,
inactivity, succession, release authority, conflict of interest, and dispute
resolution.

## Project Stewardship

Transist is maintained by the transist-ai maintainer team and hosted under the
[transist-ai GitHub organization](https://github.com/transist-ai).
The project website is [transist.ai](https://transist.ai). The organization and
domain names identify the project, not a separate legal entity. Copyright
attribution is recorded separately in `NOTICE` and applicable source notices.

This is a monorepo. It contains the runtime kernel, the companion inspection
server (`transist-server`), and the admin trust-creation CLI (`transist-admin`)
as separate executables built from workspace packages in this repository.
`transist-admin` is **not** a sibling repository.

## Current Maintainer Team

- Public organization members are listed in the
  [organization directory](https://github.com/orgs/transist-ai/people).
  This directory may omit private members and is not an authority roster.
- Maintainer appointments define areas of responsibility; repository permissions,
  `CODEOWNERS`, and release-environment controls implement those assignments.
  Organization membership alone grants no maintainer or release authority.
- Maintainer contact: `hello@transist.ai`

The sections below define how maintainers are appointed and how authority
transfers. Team-based stewardship does not imply a particular team size.

## Roles

- **Trusted contributor:** an invited repository collaborator who may open
  implementation pull requests in approved areas and must satisfy project
  validation and sign-off requirements. This role has no merge, release, or
  governance authority by default.
- **Maintainer:** merge authority on a defined area, review responsibilities,
  and a vote on cross-cutting decisions. Maintainers approve changes to
  licensing claims, compatibility policy, and trust-boundary contracts.
- **Release authority:** the maintainer (or maintainers) authorized to cut,
  sign, and publish a release. Release authority requires admin trust-material
  access through approved controls and a separate explicit appointment.
- **Area owners:** maintainers with merge authority over a specific surface
  (kernel, adapters, schemas, docs, security, release). Area ownership is
  recorded in `CODEOWNERS`.

## Appointment

- A new maintainer is proposed by an existing maintainer and appointed with
  approval from the GitHub organization owners responsible for project governance.
- Appointments record the maintainer handle, areas of authority, and the date.
- A new maintainer must accept the DCO, the code of conduct, and the
  trust-boundary rules in `AGENTS.md` and `SECURITY.md`.
- Release authority is granted separately and only to maintainers with
  admin trust-material access; it is never granted by area ownership alone.

## Removal

- A maintainer may step down voluntarily at any time by notifying the maintainer
  contact and the organization owners responsible for project governance.
- A maintainer may be removed for code-of-conduct violations, repeated
  trust-boundary violations, security-policy violations, or sustained
  inactivity, by decision of those organization owners.
- Removal revokes merge authority, area ownership, release authority, and
  repository access. `CODEOWNERS` is updated in the same change.

## Inactivity

- A maintainer with no commits, reviews, or documented decisions for 90 days
  is considered inactive.
- Inactive maintainers are moved to **emeritus** status: they retain credit
  and recognition but lose merge authority until they resume activity and are
  reappointed.
- Emeritus status is recorded in this file and reflected in `CODEOWNERS`.

## Succession

- If an appointed maintainer becomes unavailable, organization owners responsible
  for project governance appoint a successor using the appointment process.
  If no authorized release maintainer is available, release publication pauses
  until a successor has been explicitly appointed and provisioned.
- Record the successor's handle, scope and appointment date; revoke superseded
  access and update repository and release-environment controls. Organization
  ownership alone does not grant release authority or bypass release protections.
- Succession for `transist-admin` and release authority requires secure provisioning
  of admin trust-material access. Any signing-key rotation and update of the
  published key (`transist-release.minisign.pub`) follows
  [Release Artifact Verification](docs/public/reference/release-artifact-verification.md).

## Release Authority

- Only a maintainer with explicit release authority may cut, sign, and publish
  a release.
- Release publication follows the draft-first atomic process: all
  artifacts and verification gates must pass before the final publication
  transition.
- Release signing keys are generated and held outside the runtime, in
  `transist-admin`. The runtime never creates trust material.
- Every release updates `CHANGELOG.md`, the compatibility reference, and the
  supported-platforms reference in the same release change.

## Conflict Of Interest

- Maintainers disclose any conflict of interest (employer, customer, or
  commercial relationship) that could affect a decision they are leading.
- A maintainer with a disclosed conflict recuses from binding decisions on
  that conflict and may still contribute technical review.
- Trust-boundary, security, and release decisions require a maintainer without
  a disclosed conflict to be accountable.

## Dispute Resolution

- Technical disputes are resolved by technical evidence: contract tests, E2E
  suites, the kernel-contract docs, and `VISION.md`.
- If evidence is inconclusive, escalate to the organization owners responsible
  for project governance for a documented decision, subject to the conflict-of-interest
  rules above.
- Governance or conduct disputes follow the [Code of Conduct](CODE_OF_CONDUCT.md)
  escalation path.
- Decisions affecting the kernel contract or public compatibility are recorded
  in `CHANGELOG.md`.

## Contact Paths

- Trusted contributors and maintainers may use repository issues and pull
  requests for day-to-day implementation work.
- General maintainer contact: `hello@transist.ai`.
- Security reports follow [`SECURITY.md`](SECURITY.md); never use normal issues.
- CI and contribution governance (why upstream PRs are restricted, how
  collaborator access is granted, CI tiers, test categorization):
  [CI and Contribution Governance](docs/developer/ci-and-contribution-governance.md).

## Decision Process

- Kernel and contract changes must align with `AGENTS.md`, `VISION.md`, and the
  kernel contract docs.
- Maintainers may request narrower scope, additional evidence, or split changes
  before merge.
- Stability-impacting changes require matching changelog and documentation
  updates.
- The runtime/admin trust split is non-negotiable: `transist` must not gain
  key generation or signing code. Those responsibilities live in the
  `transist-admin` executable in this monorepo.

## Trust Surface Ownership

- `transist` maintainers own verification and enforcement inside the runtime.
- Appointed `transist-admin` area maintainers own the trust-creation tooling;
  release signing and publication still require separate release authority.
- `transist-admin` owns private-key handling, public-key export, signing,
  verify dry-runs, and signature inspection.
- `transist-admin` lives in this monorepo as the `transist-admin` workspace
  package and the `transist-admin/` source tree; it is not a separate
  repository.

## Security Escalation

- Do not disclose vulnerabilities through normal issues.
- Follow the private reporting path in [`SECURITY.md`](SECURITY.md) or email
  `security@transist.ai`.
