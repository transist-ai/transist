# Release Environment Configuration

## Public release text

Ready drafts already contain the final public title and release notes. Preparation
reads the versioned `docs/public/releases/<version>.md` and that version's
`CHANGELOG.md` heading from the verified candidate checkout. Relative documentation
links become final-tag-pinned GitHub links. Missing or mismatched notes block
readiness; preparation placeholders are not public release notes.

The preapproval job checks out the exact candidate and displays this final text
alongside its verification evidence. Publication rejects draft text that differs
from those source files, explicitly sets the final title/body, and verifies both
after publication. A lost-response recovery authenticates the same text before
recording a newly published effect. Recovery of an already-recorded published
release preserves later maintainer edits to its title and notes.

Maintainers may correct public title/description errors without changing release
tags, assets, signatures or receipts. Verify all asset IDs, digests, sizes and
timestamps remain unchanged after such a metadata-only correction.

This document describes the draft-first release lifecycle and the live rules
that protect preparation and publication. Automatic eligibility remains disabled
unless `RELEASE_PIPELINE_ENABLED=true`; provisioning and hosted lifecycle
validation are separate activation requirements.

## Release Environment

Preparation runs through `.github/workflows/release.yml`. Publication runs through
`.github/workflows/release-publication.yml`, called automatically after successful
preparation. There is one publication approval, after the complete draft is ready.

| Setting | Required value |
|---|---|
| Environment names | `release-build` and `release-publish` |
| Build reviewers | None |
| Publication reviewer | `implicitly-awesome`; self-review allowed |
| Deployment branch policy | Only `{name: "main", type: "branch"}` for both environments |
| Administrator bypass | Disabled |
| Signing secret | `release-build` only |

Both environments and the `release-state` branch must exist before activation.
`check_release_policy.sh` validates both environments against `.release_reviewers`
and `.branch_policies`, plus exact-SHA checks and immutable tag rules. Both checker
invocations (with or without `--candidate`) enforce this same contract. Publication
has no signing key or OIDC write permission.

## Reproducible Setup (Step 7 Only)

Step 6 implements and tests setup offline with fake `gh`. Applying live GitHub
configuration belongs to Step 7. Use Python 3.11+ and authenticated `gh` with
repository administration, environment/secrets, Actions-variable read and contents
access. Use an owner/operator session; workflow jobs retain their job-scoped
`GITHUB_TOKEN` permissions and do not need a PAT.

Read-only inspection is the default; missing resources or policy drift exit nonzero:

```bash
python3 scripts/ci/setup_release_pipeline.py --repo transist-ai/transist --check
```

In Step 7, with automatic preparation disabled, an owner may explicitly provision:

```bash
python3 scripts/ci/setup_release_pipeline.py --repo transist-ai/transist --apply \
  --signing-key-file /secure/owner-supplied/transist-release.key
```

The file must already hold the existing production key, be owned by the current
user, nonempty and owner-only (for example mode `0600`), with no links. Do not
place it in the checkout, logs or shell variables. Setup checks file metadata and
streams its open descriptor directly to `gh secret set MINISIGN_SECRET_KEY --env
release-build`; it never loads key values into Python, command arguments, output,
environment variables or temporary copies. GitHub secrets cannot be exported.
Missing secure-source access blocks provisioning when the environment key is
absent; do not generate a replacement key to work around missing custody.

Setup creates or reconciles the two environments, resolves the committed reviewer
login to a GitHub user ID, and selects exactly the committed `main` branch policy.
It creates `release-state` using a parentless commit containing only regular
`state.json` with the repository ID and an empty candidate map. It never copies
main's tree, checks out state, or executes branch content. Existing state is
validated and preserved, never reset. Unreadable or unsafe state blocks apply.
Concurrent creation fails safely without forcing a ref; rerun to inspect it.

Only after key presence, state and both environments' desired policy are validated
does setup remove individual obsolete deployment selections from those two
environments. It does not delete any environment, tag, release, state, ruleset or
secret. Partial API failures exit nonzero; rerun `--check`, then `--apply` to
reconcile. Reapplying valid configuration without a key-file option performs no
writes; supplying a key file explicitly provisions that file again.

Secret listings prove presence only, not key identity. Step 7 must verify signing
against `transist-release.minisign.pub` before the owner explicitly removes any
repository-wide or organization-wide signing-key access. Setup never removes old
secrets or changes `RELEASE_PIPELINE_ENABLED`; it refuses to run with that variable
set to `true`. A signing secret in `release-publish` blocks setup and requires
explicit owner remediation. Existing immutable `v*` rules are verified separately:

```bash
GITHUB_REPOSITORY=transist-ai/transist \
  bash scripts/ci/check_release_policy.sh --candidate <full-source-sha>
```

Missing immutable rules, successful exact-SHA gates, permissions or signing
evidence remain blockers. Setup does not dispatch workflows or publish releases.
Lifecycle validation in the public test repository uses only synthetic payloads
and test-only keys. Production activation remains a separate Step 8 operation.

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

Preparation requires no permanent tag. After approval and reservation, publication
creates an annotated `v<version>` tag at the receipt's exact commit. An existing
tag is accepted only when it is an annotated tag with the correct name pointing
directly to that commit. Lightweight, nested and wrong-target tags fail closed;
no tag is moved or deleted.

Preflight verifies the latest started workflow attempt for each required gate
on the exact candidate SHA, including reruns of older workflow runs. It binds
the gate job and check run to the expected workflow and GitHub Actions identity
(`app.id` `15368`, `app.slug` `github-actions`). A newer failure or incomplete
run cannot be hidden by an older success.

The candidate preflight also supports an untagged preparation mode and a
publication mode that binds a receipt to durable candidate state and verifies
the annotated tag in the target repository. The publication workflow uses the
production helper `publish_candidate.py` to reserve before tag creation and to
recover interruptions without rebuilding or replacing assets.

`Security Scan` also runs on pushes to `main`. The separate `Release Eligibility`
workflow is disabled unless repository variable `RELEASE_PIPELINE_ENABLED` is
`true`. It checks both latest successful push gates on the completion event's
exact SHA, runs the aggregate readiness check on that checkout without signing
secrets, and rechecks the gates before atomically claiming the candidate in the
`release-state` branch. Both the latest run and the latest push run must pass;
a newer failed or unfinished scheduled or manual scan blocks eligibility. A
successful rerun clears that failure, but does not trigger preparation: a later
successful push completion must re-evaluate it.
Duplicate events can run duplicate checks but reuse the same active claim. The
state branch must be provisioned before activation; missing state fails closed.
Eligibility claims the source and calls the six-platform preparation workflow.

`scripts/ci/candidate_assets.py` provides offline building blocks for checking
the 29 expected candidate asset names and downloaded bytes against independently
recorded digests and API sizes. A download helper retrieves all listing pages
and draft assets by release ID, then rechecks draft identity and the listing.
`prepare_candidate.py` and `publish_candidate.py` use these byte checks alongside
signature, source provenance and cryptographic attestation verification.

Release preflight requires existing successful `Mainline Validation / main-gate`
and `Security Scan / security-gate` checks on that exact SHA. Release CD does
not call a `release-security` job or rerun the security workflow. Hosted aggregate
readiness runs automatically before preparation. Local aggregate release readiness
remains available for development and diagnosis; see
[CI and Contribution Governance](ci-and-contribution-governance.md).

## Draft-First Atomic Publication

The release matrix identifies six platform archives by Rust target triple. Runner
labels and pinned Debian or Alpine containers define native build and archive
validation environments. ARM64 musl builds and smokes in pinned Bookworm with
native `musl-tools`. One central pinned Linux job packages all six archives.
See [Supported Platforms](../public/reference/supported-platforms.md).

1. **Prepare:** build, smoke, centrally package and sign the six platforms;
   create a private `candidate-<candidate_id>` draft targeting the exact SHA;
   upload the 29 assets once by release ID; download and independently verify
   signatures, manifest, SBOM, archive members and commit/tree provenance.
   Attest those verified downloaded archive bytes. Readiness records the immutable
   receipt in `release-state/state.json` through bounded compare-and-swap.
2. **Review:** the summary job displays candidate SHA/tree, version, the draft's
   actual download URL, manifest digest, preparation results and run link before
   the `release-publish` job requests approval. Download and test the final bytes,
   then approve that environment deployment.
3. **Publish:** after approval, revalidate the receipt, source, current eligibility,
   live policies and latest exact-SHA checks. Reserve `ready → publishing` by CAS
   before tag or release effects. Download and authenticate every asset against
   the receipt, then create/verify the annotated final tag. Patch the draft's tag
   and verify identity and unchanged asset IDs/digests again. Revalidate policy
   immediately before publishing with JSON booleans `draft: false`,
   `prerelease: false` and API enum `make_latest: "true"`. Verify public identity,
   assets and final tag, then record `published`.

Publication never rebuilds, repackages, signs, uploads, deletes or replaces assets.
The normal operator path is draft review/testing, then one publication approval.
There is no manual security dispatch or tag creation prerequisite. The version is
read from `[workspace.package].version`; archive names use the final `v<version>`
even while the draft has a candidate tag. Receipts remain outside the 29 public
assets in the data-only state branch.
The signature-verification key is taken from the exact source checkout. Attestation
verification binds both source and signer digest to the preparation workflow
revision on main, requires its workflow directory to match the candidate's, and
retains the candidate SHA/tree provenance binding inside the verified bytes.

### Interrupted preparation or publication

A newer fully verified descendant supersedes an older unpublished ready candidate;
failed preparation cannot supersede it. Superseded drafts remain inspectable and
are unpublishable. Ordering uses Git ancestry, not completion time; incomparable
sources are rejected. Once `publishing` is reserved, no candidate can supersede it.

Dispatch **Release Recovery** (`release-recovery.yml`) from **main**, with only
the exact `candidate_id` from the original summary or durable state. No arbitrary
source ref, version or operation may be supplied. Every publication recovery
passes a new summary and fresh `release-publish` approval.

- **Failed preparation:** re-run aggregate readiness and latest source checks,
  then claim a new candidate ID using this recovery run ID/attempt. Build and
  upload a new private draft. The failed attempt and its incomplete draft remain
  inspectable; no assets are overwritten. Concurrent recovery claims permit only
  the owning attempt to prepare.
- **Ready:** use the unchanged receipt and assets; approval can fail if a newer
  fully verified descendant superseded it while review was pending. Duplicate
  automatic completions reconcile metadata but do not queue another approval.
- **Publishing:** reservation is retained after any failure. Recover the same
  candidate after missing tag creation, final-tag patch, publication, or state
  recording. Other candidates cannot supersede a reserved publication.
- **Matching public release:** verify its exact release ID, SHA, final annotated
  tag, all 29 byte digests, signatures, provenance and attestations. Treat it as
  idempotent success and reconcile `published` without release mutations. Later
  gate failures or mutable eligibility changes do not block recording this
  already-completed effect. Fresh recovery environment approval still applies;
  current eligibility and policy checks gate new publication effects only.
- **Mismatching public release or final tag:** fail hard. Do not move the tag,
  redraft the public release, overwrite assets or release the reservation.

Failures leave the release private unless the publish API operation already
succeeded. A lost API response is recovered by re-reading authoritative state.
Publication effects are serialized across automatic and recovery jobs; the CAS
reservation also rejects a concurrent newer-candidate promotion.

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

Preparation refuses to upload when the draft already has any asset names. The
upload deliberately does not use `--clobber`; failed preparation recovery creates
a new attempt and a new draft without deleting tags or overwriting assets.

## Verification Key

The committed `transist-release.minisign.pub` is the only verification-key
source. A downloaded release key must compare byte-for-byte to this file before
it is trusted; matching only the key ID is insufficient. The key ID is published in `README.md` and
`docs/public/reference/release-artifact-verification.md`.

Before signing, the workflow verifies that the signing secret's key ID
matches the committed public key's key ID. A mismatch prevents all signing.

## Local And Hosted Regression Gates

For focused pipeline development (plan Steps 1–7), run
`bash scripts/ci/test_release_invariants.sh --fixture-build`. This explicit
test-only mode replaces the isolated candidate dry-run's native build with
synthetic binaries using `tests/fixtures/release/cargo_fixture.py`; all other
checks and real Cargo tool probes remain enabled. It is accepted for the Step 4
focused invariant check, alongside the candidate tests, release inventory and
supply-chain checks. It does not establish native binary/platform compatibility
or replace the full final release-readiness gate. Default invocation and hosted
release preparation do not enable this mode.

The reusable preparation workflow's failure finalizer covers upstream builds,
signing, attestation, and post-promotion errors. It verifies repository, run,
attempt and main-ref ownership before failing a preparing claim; ready and
publication-reserved candidates remain intact. Receipt evidence uploads run
even after a later step fails. A `ready` helper retry revalidates the exact
persisted identity, assets and attestation evidence without replacing its
receipt. Only the predecessor recorded by the promotion CAS is relabelled;
historical superseded drafts are not scanned or modified.

If all three in-job label attempts fail after promotion, rerun the original
`Release Eligibility` workflow or let a duplicate eligible completion event
re-evaluate the same source. When the active claim is already `ready`, its
separate `reconcile` job checks out trusted `main` code and runs
`prepare_candidate.py reconcile --candidate-id <candidate-id>`. It validates the
authoritative ready receipt and current draft identity before reconciling only
the recorded predecessor's label. It requires no original-run ownership, local
assets, signing key, or publication approval; it cannot prepare, promote, modify
assets, create tags, or publish. Errors leave the ready receipt and state intact.

Label updates explicitly preserve the predecessor's original candidate tag and
source SHA, then verify the saved identity and label. GitHub can rewrite an
untagged draft's `tag_name` to its `untagged-*` URL slug when a PATCH omits the
tag. For an already-aliased superseded predecessor, reconciliation downloads all
29 assets by numeric asset ID and compares their full bytes with its persisted
receipt before restoring the original tag field. It also checks the exact release
ID, source SHA, listing stability and current promotion state. A slug or title
alone never identifies a candidate. Active drafts still require their exact
original identity; this exception cannot authorize publication.

The reusable signing interface declares and maps only `MINISIGN_SECRET_KEY`,
without blanket inheritance. Its optional caller mapping may be empty: the
`stage-and-sign` job binds `release-build`, whose environment secret takes
precedence. Provision the key there, not at repository scope. Missing delivery
fails before writing a key file. The review summary needs job-scoped
`contents: write` to read private draft metadata on GitHub, although its command
performs only reads and has no signing secret or deployment environment.

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

- Disable `RELEASE_PIPELINE_ENABLED` to stop automatic candidate creation.
- For transient CI, signing, or upload failure: recover the failed candidate ID
  from main to create a new preparation attempt. Retain incomplete drafts.
- For interrupted publication: recover only the reserved candidate ID through
  fresh approval. Never clear its reservation or replace individual assets.
- For a source or generated-artifact defect after the protected tag is created:
  leave that tag and candidate intact. Do not clear a publication reservation;
  investigate and reconcile its exact candidate before any further publication.
  Corrected source requires a new patch version and a new fully verified candidate.
- After publication: withdraw the release and issue a new patch; never
  overwrite artifacts under the same tag.
