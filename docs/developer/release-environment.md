# Release Environment Configuration

This document describes the GitHub `release` environment and the live rules
that protect release staging and publication. These settings are verified by
`scripts/ci/check_release_candidate.sh` before staging.

## Release Environment

A GitHub deployment environment named `release` protects the `stage-and-sign`,
`attest-public-assets`, and `publish-release` jobs in `.github/workflows/release.yml`.

| Setting | Required value |
|---|---|
| Environment name | `release` |
| Required reviewers | As committed in `.release_reviewers` |
| Deployment branch policy | Selected custom policy from `.branch_policies` |
| Administrator bypass | Disabled |
| Wait timer | Optional |

The environment must exist before the first `stage` operation. The
`check_release_candidate.sh` script delegates read-only live policy checks to
`check_release_policy.sh` before staging proceeds. The checker verifies the
environment, required reviewers, disabled administrator bypass, and the exact
deployment policy returned by the API's `.branch_policies` field.

The committed `.branch_policies` file is the source of truth for the allowed
deployment policy. Its current exact set is `{name: "v*", type: "tag"}` and
`{name: "main", type: "branch"}`; the live API response is compared to that
complete committed set. Release CD still requires dispatch from the exact
release tag ref.

The committed `.release_reviewers` file is the source of truth for the required
reviewer set. It must contain a JSON array of reviewer login strings; the live
API's `required_reviewers` protection-rule logins are compared to that exact
committed set. Use that file's exact approved contents rather than an example
reviewer list. A reviewer-policy change requires maintainer approval and matching
live environment configuration before release operations.

## v* Tag Protection Ruleset

A committed tag ruleset at `.github/rulesets/v-tag-protection.json` defines
the immutable tag policy for all `v*` tags. It blocks:

- **Deletion** — no one can delete a `v*` tag.
- **Non-fast-forward** — no one can force-push or move a `v*` tag.
- **Update** — no one can update a `v*` tag.

The `update` rule carries no permissive parameters: the live ruleset API returns
no `parameters` for tag-targeted `update` rules, so permissive force-push or
fetch-and-merge parameters (`allow_all_users`, `allow_force_pushes`,
`allow_soft_force_pushes`, `update_allows_fetch_and_merge`) are rejected
whenever present, and force-push prevention is enforced by the
`non_fast_forward` rule itself.

Once activated, the `check_release_candidate.sh` script verifies via the
GitHub API that:

- A tag-targeted ruleset exists.
- It blocks `deletion`, `update`, and `non_fast_forward`.
- The `update` rule permits no force-push or fetch-and-merge allowance
  (permissive parameters are rejected whenever present).
- It has no bypass actors.

The candidate script also requires the remote tag to be an annotated tag whose
peeled commit SHA (`refs/tags/${tag}^{}`) matches the release-candidate commit
SHA exactly; a missing or mismatched remote peel is a hard failure.

The check-runs query is paginated and bound to the candidate commit SHA. Only
GitHub Actions check runs (`app.id` `15368`, `app.slug` `github-actions`) are
eligible; duplicate names are resolved by the newest completion/start time,
and a newer failure or incomplete run cannot be hidden by an older success.

Release preflight requires existing successful `Mainline Validation / main-gate`
and `Security Scan / security-gate` checks on that exact SHA. Release CD does
not call a `release-security` job or rerun the security workflow; obtain the
exact-SHA security result before staging. Local aggregate release readiness
remains a separate pre-tag requirement; see
[CI and Contribution Governance](ci-and-contribution-governance.md).

## Draft-First Atomic Publication

The release matrix identifies six platform archives by Rust target triple. Runner
labels and pinned Debian or Alpine containers define native build and archive
validation environments. ARM64 musl builds and smokes in pinned Bookworm with
native `musl-tools`. One central pinned Linux job packages all six archives.
See [Supported Platforms](../public/reference/supported-platforms.md).

Release publication follows a two-phase model:

1. **Stage**: The `stage` operation builds, tests, packages, signs, and
   uploads all assets to a **draft** GitHub release. No asset is ever
   uploaded to a public release. If any build, sign, or check fails, the
   draft remains incomplete but no public release exists.

2. **Publish**: The `publish` operation compares the draft's exact asset
   names and digests against the signed release manifest and verifies source
   provenance. `attest-public-assets` records the verified archive digests and
   attests them when the repository is public; `publish-release` re-verifies
   the draft and requires that same digest set before updating the release:

   ```bash
   gh api -X PATCH "repos/$GITHUB_REPOSITORY/releases/$release_id" \
     -f draft=false -f tag_name="$RELEASE_TAG" --jq .html_url
   ```

   A verification failure leaves the release as a draft.

### Manifest

The release manifest (`release-manifest.txt`) is a sorted list of SHA-256
digests for every payload asset: platform archives, checksum files, archive
signatures, one native build provenance record per target, the final-packaging
provenance record, the SBOM archive, and the committed verification key. Each
record binds its source to the release commit and tree SHA. The manifest
excludes itself and its own signature by definition. The six-target contract
contains 27 manifest entries and 29 release assets including the manifest and
its signature. GitHub attestations are separate from that file asset set.

The manifest is signed with `minisign` and uploaded alongside the assets.
Before publication, the `publish` operation verifies that every draft asset
is listed in the manifest with a matching digest, and that no unexpected
extra assets exist. The only allowed assets outside the manifest are
`release-manifest.txt` and `release-manifest.txt.minisig`.

Staging refuses to upload when the draft already has any asset names. The
upload deliberately does not use `--clobber`; incomplete draft releases must be
discarded and rebuilt without deleting the protected tag or overwriting assets.

## Verification Key

The committed `transist-release.minisign.pub` is the only verification-key
source. A downloaded release key must compare byte-for-byte to this file before
it is trusted; matching only the key ID is insufficient. The key ID is published in `README.md` and
`docs/public/reference/release-artifact-verification.md`.

Before signing, the workflow verifies that the signing secret's key ID
matches the committed public key's key ID. A mismatch prevents all signing.

## Local And Hosted Regression Gates

`test_release_invariants.sh` uses fixture-backed GitHub API responses to test
exact SHA and provider identity, pagination, duplicate check runs, latest
result selection, tag rulesets (including permissive `update`-rule parameters),
reviewer and bypass protection, environment selection, tag policy type, and
packaged-doc links that escape the archive root. `test_public_tree.sh`
exercises the tracked public-tree boundary. Both regression scripts run in the
local aggregate `check_release_readiness.sh` gate.

### Candidate Worktree Allowlist

`check_candidate_worktree.sh` rejects ignored files unless they are under an
explicit generated-output allowlist. The exact generated roots are:

- `target/*`, `target-linux/*`, `dist/*`, `reports/*`, `run/*`
- `.transist/*`, `.transist-*/*`, `.tmp/*`, `.tmp_*/*`
- `scripts/ci/redocly/node_modules/*`

The generated roots are intentionally pruned from ignored-input inspection for
performance. They contain build, release, smoke, or scratch output and are
outside the materialized release candidate, so the guard does not inspect
individual filenames or contents beneath them. This exception must not be
extended to candidate-controlled release-input roots.

The narrower public-output exceptions are inspected individually and allow
only:

- `trust-input/**/*.pub`, `trust-input/**/*.sig`, `trust-output/**/*.pub`,
  `trust-output/**/*.sig`, `demo/trust/**/*.pub`, and `demo/trust/**/*.sig`
- `tests/fixtures/**/*.pub` and `tests/fixtures/**/*.sig`
- Root smoke markers `artifact.txt`, `src/input.txt`, `.retry-ok`, and
  `cleanup-success.txt`, plus `.DS_Store` and Python `__pycache__` entries

The trust and fixture exceptions are public generated outputs only. Ignored
candidate-controlled paths outside the pruned generated roots remain
release-blocking, including source and build-control files such as
`.cargo/config.toml`, private-key filenames (`*.key`, `*.pem`, `*.p12`, `*.pfx`,
`*.jks`, `*.keystore`, `id_rsa*`, and `id_ed25519*`), secret-like filenames,
and files containing private-key or credential markers, even when a broad
`.gitignore` rule hides them.

## Rollback

- For transient CI, signing, or upload failure: discard the incomplete draft
  release, retain the protected tag, and re-run `stage` at the unchanged
  tag/SHA. Do not replace individual assets or delete the tag.
- For a source or generated-artifact defect after the protected tag is
  created: leave the release unpublished and immutable, bump the patch
  version, repeat the release-candidate gate, and publish only the new tag.
- After publication: withdraw the release and issue a new patch; never
  overwrite artifacts under the same tag.
