# Draft-first pipeline validation evidence

## Current status

Steps 1–6 are implemented, and Step 7 hosted synthetic lifecycle validation and
the exact-fix regression are complete. The production rollout in
[PR #42](https://github.com/transist-ai/transist/pull/42) merged on 2026-11-14 at
`4482e66cf0632b1fc2f47f5d666232c190e01716`. The first real six-platform
production draft and its hosted qualification remain **pending**.

The authorized production signing-key verification matched committed public key
ID `202B06AC561F2B56`. Setup `--apply` passed release-environment,
signing-key-presence and data-only-state checks. The existing production signing
secret was preserved. Neither result establishes production hosted signing.
See [Release Environment Configuration](release-environment.md) for setup and
review procedures.

Production `RELEASE_PIPELINE_ENABLED=true` was confirmed after the merge. However,
the merge API returned HTTP 502 during the rebase operation; a subsequent read
confirmed the merge, while no Actions run was recorded for its main SHA. At this
follow-up checkpoint, the Actions workflow registry still lacked the new release
workflows. This documentation follow-up requires ordinary checked squash merge
and verification of actual `push` runs on fresh main. A manual dispatch is not
substitute evidence for that event. No production publication approval is granted
by this follow-up.

## Step 7: completed hosted synthetic scenarios

The original [hosted harness](https://github.com/transist-ai/transist-release-pipeline-test)
used synthetic payloads with production validators, real GitHub lifecycle effects,
signatures, OIDC attestations and designated human deployment approvals.

| Scenario | Outcome and hosted evidence |
| --- | --- |
| Failed eligibility | Intentional [main failure, attempt 2](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43509319663/attempts/2) and [security failure, attempt 2](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43509319445/attempts/2) left no candidate, release or tag. |
| Complete private draft, no pre-approval final tag | [A preparation](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43510623815) produced 29 assets, seven archive attestations and a ready receipt before human review. |
| Supersession and stale approval | [B preparation](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43511405574) superseded A without changing its assets; A's subsequent human approval correctly failed before publication. |
| Normal publication | [B publication](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43511405574) created an annotated final tag at the exact source SHA and retained all 29 original asset digests. Seven public attestation subjects verified. |
| Public idempotent recovery | [B recovery](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43514262713) required fresh human approval and preserved the receipt, tag object, release ID, asset bytes, IDs and timestamps. |
| Interrupted publication | [C interruption](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43516107824) stopped deterministically after annotated tagging, retaining the reservation and private draft. [Fresh-approved recovery](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43521743202) published the original bytes and preserved the original tag object. |
| Same-name, same-size byte tampering | [D publication attempt](https://github.com/transist-ai/transist-release-pipeline-test/actions/runs/43522556483) rejected a one-byte archive change by digest before final tagging or publication. Its intentionally corrupted private draft and reservation were retained as evidence. |

Original manifest SHA-256 identities:

| Candidate ID | Manifest SHA-256 |
| --- | --- |
| `7abfb3cb30b6e632cff1f33d2017501bc7974b2a-43510623815-1` (A) | `96b6f603fc4ccb6b9c2cafb9bd4f37975e24a5e573d1208954591c97bc0b5775` |
| `01652bfe40d01d1eed7069d513ac4ea1371ce0e8-43511405574-1` (B) | `ab0491a1018b7aa8b79eaa7bedbb90262eb46fe317314fabf38aa41cb14f23a0` |
| `b7076d257f64fa6606189fae2d527b9d00b5ea6b-43516107824-1` (C) | `1fe9d82b325a2b84a33b3e6098910e13341c84e5734fcbf7d3e9647c59d2bd7b` |
| `a6fb8819ccb6c7c5217bba563df2c5a4d67925c8-43522556483-1` (D) | `4ad275c3952d0c2e6fabea3f2c663b6f8eb3467731706413d0d4c87f4556506b` |

The original duplicate completion preserved candidate count, state and assets but
exposed a predecessor-label reconciliation failure. That finding, signing-secret
delivery and draft-summary permissions were corrected and validated by the
separate exact-fix regression below.

### Hosted findings and implemented fixes

- **Signing delivery:** the first reusable preparation received an empty key.
  Production now declares/maps only `MINISIGN_SECRET_KEY`; the signing job binds
  `release-build`, whose environment secret takes precedence. An empty value
  fails before key-file creation. The regression passed with zero repository
  secrets and only the environment key. This validates the narrower mapping,
  without claiming GitHub's internal cause of the original failure.
- **Draft summary:** draft GET returned HTTP 403 with `contents: read`.
  Only the summary job receives `contents: write`; it runs a read-only summary
  command without signing secrets or persisted checkout credentials.
- **Superseded identity:** a label-only PATCH caused GitHub to rewrite an
  untagged draft's tag. Label updates now preserve the original tag and exact
  source explicitly, then reread both. Legacy alias repair is restricted to
  superseded predecessors and authenticates numeric release ID, exact source,
  persisted receipt and all 29 downloaded asset bytes before rereading and repair.
  Active/publication identity checks remain strict.

## Exact-fix regression: Q publication succeeded

The separate [regression harness](https://github.com/transist-ai/transist-release-pipeline-regression-test)
copied the production helpers byte-for-byte. Final comparison verified all 236
copied production input hashes and four workflow-source hashes against integration
commit `7c0d2cb055a1232a30cc6c76a1229a7b0cd290e0`. The capture manifest's earlier
base label reflects then-uncommitted fixes; byte hashes establish equivalence.

| Scenario | Hosted evidence |
| --- | --- |
| P explicit mapping, signing, 29 assets, attestations and summary | [43524662384](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43524662384); canceled only after successful preparation and supersession to free the publication slot. |
| Q missing environment key | [43525225916](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43525225916); expected failure before key-file creation, no release. |
| Q restored environment-only key, ready summary and approved publication | [43525551452](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43525551452); completed **successfully**. |
| Repeated completion and reconciliation | [43525976884](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43525976884); passed. |
| Reproduced legacy alias and exact-helper repair | [43526181982](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43526181982); passed. |
| Idempotent repeated reconciliation | [43526395869](https://github.com/transist-ai/transist-release-pipeline-regression-test/actions/runs/43526395869); passed. |

P release ID is `444830300`. Q candidate is
`0b495396f41384da364c3c38fb9d78d2458d03b2-43525551452-1`, public release ID
`444834497` ([v0.0.3](https://github.com/transist-ai/transist-release-pipeline-regression-test/releases/tag/v0.0.3)).
Q's manifest SHA-256 is
`563056243f8231c48d9e499d39e00e0d0d0ebe70b0087ea3703b4feba2cf0798`.

Fresh post-publication downloads matched all 29 receipt-bound sizes and hashes;
Q's receipt and identity remained identical to the ready snapshot. Annotated tag
object `2730a9de5a2baa36ee58a8f182cb971ab16a3437` targets exact source
`0b495396f41384da364c3c38fb9d78d2458d03b2`. All seven public archive subjects passed
`gh attestation verify` with exact signer workflow, source ref/digest, signer
digest and hosted-runner restriction
([Q attestation](https://github.com/transist-ai/transist-release-pipeline-regression-test/attestations/67386761)).

All 29 predecessor assets matched before and after alias repair. State, receipts
and asset identities remained unchanged; draft browser slug rotation and download
counter increments were recorded. Human approvals used `release-publish` with
admin bypass disabled. Earlier reports saying Q publication was pending are
superseded by this final verification.

## Step 8: local integration evidence and production boundary

The initial Step 6 aggregate exposed hardcoded synthetic versions in a signed
fixture. The fixture now derives version/archive names from `Cargo.toml`; the
identity checker was not weakened. Final focused checks passed: 252
candidate-pattern tests, documentation/skills, supply-chain/Actionlint, Python
correctness lint and Markdown lint (183 files, zero errors; 17 linter regressions).

The unchanged full release-readiness aggregate passed all 28 host-local checks
on committed integration SHA `7c0d2cb055a1232a30cc6c76a1229a7b0cd290e0`, exit 0:

```bash
PYTHONDONTWRITEBYTECODE=1 TMPDIR=<project-tmp> bash scripts/ci/check_release_readiness.sh
```

The first final-integration attempt timed out in the cleanup E2E wait after
stages 1–26 passed. The isolated cleanup rerun and subsequent entire aggregate
passed without source changes or timeout weakening. Retained log basenames are
`release-pipeline-final-cutover-readiness.log`,
`release-pipeline-final-cutover-cleanup-diagnostic.log` and
`release-pipeline-final-cutover-readiness-retry.log`, with aggregate exit files.
This docs follow-up requires its own unchanged aggregate on the final committed
tree; record that SHA and outcome in its PR before merge.

Both harness snapshots, receipt/digest comparisons, source manifest and local
aggregate logs are retained in `release-pipeline-both-harnesses-evidence.tar.gz`.
Its verified SHA-256 is
`25fa6ada816f3db831b066f0e4a352755231360742a63483130714abec6798b1`.
The export excludes the harness private-key directory. Private filesystem paths
and key locations are intentionally omitted here.

Hosted lifecycle completion uses **synthetic archives**, not native six-platform
builds or production compatibility qualification. C covers a deterministic
after-tag fail-stop, not every outage boundary; D covers byte corruption, not
every inventory-tamper alternative. The first real production draft still needs
exact-SHA mainline/security success, aggregate readiness and native archive
checks. Stop before production publication approval. To disable automatic
preparation, set `RELEASE_PIPELINE_ENABLED=false`; preserve durable state,
receipts and immutable tags/assets. Reserved recovery requires fresh human
approval rather than rebuilding.
