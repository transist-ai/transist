# Draft-first pipeline validation evidence

## Local implementation

Steps 1–6 of the automated draft-first release pipeline are implemented locally.
The production pipeline has **not** been activated or validated on hosted runners. See
[Release Environment Configuration](release-environment.md) for the operator path
and secure provisioning commands.

The final Step 6 focused checks passed: documentation/skills, 106 candidate tests,
release invariants (original invocation, no fixture-build flag), supply-chain
pins, Markdown lint and focused Python lint. Production six-platform compatibility
is not established by the synthetic archive fixtures.

## Step 7: initial checkpoint (historical)

Read-only checks on 2026-11-13 established:

- The authenticated account has admin access to public `transist-ai/transist`
  (repository ID `1444908011`); its default branch is `main`.
- `setup_release_pipeline.py --check --repo transist-ai/transist` exited 1:
  it reported a missing `release-build` signing key. An owner-supplied secure source was
  required. GitHub secret values cannot be exported; none were requested.
- The `RELEASE_PIPELINE_ENABLED` variable endpoint returned HTTP 404. Automatic
  preparation therefore remains disabled by the workflow condition.
- The designated `transist-ai/transist-release-pipeline-test` repository endpoint
  returned HTTP 404. No test repository was created or populated.

At that checkpoint, remote pushes and setup mutations were deferred. Subsequent
authorized setup and hosted harness work supersede those initial blockers as
recorded below. The configured reviewer must perform hosted approvals; automation
must not approve or bypass them.

At that checkpoint, all requested hosted scenario outcomes were **not run**: failed eligibility,
complete private draft, absent pre-approval final tag, supersession, stale approval
rejection, asset tampering, annotated-tag publication, unchanged public digests,
interrupted publication recovery, duplicate events and public attestations. There
were no hosted workflow URLs, candidate IDs, tag targets or before/after digest
sets to report.

## Hosted synthetic harness follow-up: local production fixes

The separate `transist-ai/transist-release-pipeline-test` harness subsequently
exercised synthetic assets using production helpers. Its retained `RESULT.md`,
`SUPERSESSION-RESULT.md`, `B-PUBLICATION-RESULT.md` and `C-READY-RESULT.md` are
historical evidence, not production release qualification. The local evidence
directory is `../tmp/release-pipeline-hosted-test/.local/evidence/`.

- Run `43510016968` prepared a draft but its summary GET returned HTTP 403
  with `contents: read`. Only the summary job now receives `contents: write`;
  it still runs the read-only summary command without persisted checkout
  credentials or signing secrets.
- Run `43509634280` received an empty signing key. The `release-build`
  environment existed at 21:17:21Z and key metadata at 21:17:24Z, before the
  failed signing step at 21:20:10Z on 2026-11-13. Both harness revisions bound
  the signing job to that environment. Commit `f469ff1` changed only the two
  reusable preparation calls to `secrets: inherit`, and signing then succeeded.
  The original reusable interface declared no secrets and callers mapped none.
  This is consistent with a reusable secret-delivery issue, but does not establish
  GitHub's internal resolution cause or a need to expose every secret. Production declares and
  maps only `MINISIGN_SECRET_KEY`; the optional call input allows the caller's
  repository-level value to be absent. The signing job's `release-build`
  environment supplies the key and takes precedence. Keep repository-level
  signing secrets absent for new setup; the existing production secret is retained
  pending separate removal authorization. An empty key fails before creating a
  key file. The exact narrower mapping subsequently passed the hosted regression
  below with only an environment secret; this validates the replacement, not the
  internal cause of the earlier failure.
- Duplicate eligibility runs `43511955995` and `43511957864` failed predecessor
  reconciliation after the label PATCH changed A's `tag_name` from its original
  `candidate-*` identity to `untagged-d845540d1daa59d6daa1`. Label PATCHes now send
  the original `tag_name` and exact `target_commitish`, then GET and verify both
  identity and label. An already-aliased predecessor is accepted only in the
  superseded-label path, by bound numeric release ID, exact source SHA, valid
  persisted receipt and all 29 downloaded asset bytes. Listing and release
  identity are reread before repair. Duplicate reconciliation is a no-op after
  repair; active-candidate and publication identity checks remain strict.

Offline regressions simulate the observed tag rewrite, alias repair, unchanged
state/receipts/assets, duplicate reconciliation, and rejection of wrong identity,
inventories and same-size byte tampering. Exact current helper bytes and the
workflow mapping/permission fixes were exercised in the separate hosted regression
harness below. The production follow-up remains local pending review.

Focused local validation on the uncommitted fix tree passed: 113 candidate tests,
the unreduced release-invariant script, supply-chain pins, documentation/skills,
Actionlint, focused Python correctness lint (`E9,F`) and Markdown lint (183 files,
zero errors). Logs are in the external project temp folder with prefix
`release-local-fixes-`. The first invariant invocation timed out during its native
candidate build; the completed final invocation passed without `--fixture-build`.
The full aggregate must run unchanged on the final clean follow-up commit; its
result is separate from these focused checks.

Final follow-up checks passed: 252 tests via discovery of `test_*candidate*.py`,
documentation/skills validation, supply-chain pins including Actionlint, focused
Python correctness lint (`E9,F`), and Markdown lint (183 files, zero errors;
17 linter regression tests). Logs use `release-pipeline-step-7-followup-` in
`../tmp/`. The subsequent clean-commit aggregate log uses
`release-pipeline-step-8-followup-readiness.log` and a companion `.exit` file.

## Current setup and exact-fix hosted regression

Local `verify_release_signing_key.sh` verification against the committed public
key passed for key ID `202B06AC561F2B56`. Authorized production setup `--apply`
then passed environment, signing-key-presence and data-only-state checks. Evidence:
`../tmp/release-pipeline-production-key-verification.log` and
`../tmp/release-pipeline-production-setup-apply.log`. The existing production
release secret was retained. These results supersede the missing-key/setup
blocker; they do not establish production hosted signing or activate preparation.

The retained report at
`../tmp/release-pipeline-hosted-regression/.local/evidence/RESULT.md` records exact
source hashes in `PRODUCTION-SOURCES.json` and byte-for-byte production helper
copies. In `transist-ai/transist-release-pipeline-regression-test`, only
`release-build` held the signing secret; repository secret count was zero.

| Scenario | Hosted run |
| --- | --- |
| P eligibility: explicit mapping, signing, 29 assets, attestations and summary | `43524662384` |
| Q missing environment key: fails before key-file creation | `43525225916` |
| Q recovery: restored environment-only key, explicit mapping, ready and summary | `43525551452` |
| Repeated completion and reconciliation | `43525976884` |
| Reproduced legacy alias and repaired via exact production helper | `43526181982` |
| Repeated reconciliation is idempotent | `43526395869` |

Run URLs use `https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/`
followed by the run ID. P release ID is `444830300`; Q release ID is `444834497`.
P and Q attestations (`67385059`, `67386761`) each cover seven archives and passed
production signer/source verification. All 29 predecessor asset bytes matched
the receipt before and after repair. State, receipts and asset identities remained
unchanged; browser URL slug rotation and download counter changes are recorded.

All requested **pre-approval regressions passed**. Q remains awaiting the designated
human reviewer in `release-publish`, with admin bypass disabled. Q publication and
post-publication validation are **pending**, not passed; no final tags or public
releases were created in this regression harness. Synthetic assets do not qualify
production six-platform binaries.

## Step 8: pending

The initial full gate at Step 6 commit `64bf1c3` failed at maintainability after
passing format, Clippy, Rustdoc, 2,227 default-feature tests, 2,241 all-feature
tests and 222 security regression tests. The identity checker detected hardcoded
synthetic release versions in a new signed-payload fixture. That fixture now
derives its version and archive names from `Cargo.toml`; the identity checker
(18 tests) and candidate suite (106 tests) pass. The checker was not weakened.

Run the aggregate on the committed follow-up tree and record its outcome alongside
the origin plan. Logs use the `release-pipeline-step-8-` prefix in the specified
external temp folder. A passed local gate does not establish hosted production readiness.

The full local aggregate and first real six-platform candidate are required before
completion. Enabling the variable is deferred until review, merge and successful
Step 7 validation. Stop before approving any production publication.
