# Release Artifact Verification

Transist releases are published through a draft-first atomic publication
process. Assets are built, tested, signed, and uploaded to a **draft** release.
Publication happens only after the draft's exact asset set is verified against
a signed release manifest. See [Supported Platforms](supported-platforms.md)
for the exact release target matrix.

## Committed Verification Key

The release verification key is committed to the repository at
`transist-release.minisign.pub`. Its
key ID is `202B06AC561F2B56`.

**Before trusting a downloaded key, compare it byte-for-byte with the committed
file.** The key ID is useful as a human-readable cross-check, but it does not
replace the exact comparison. Do not trust a key downloaded from the same
untrusted release alone.

```text
untrusted comment: minisign public key 202B06AC561F2B56
RWRWKx9WrAYrIHlPpT1eTyTodKHmYcLdXqanG4RFeYWFA8cBu39siM9h
```

## Expected Assets

For a release `<tag>` (initially `v0.1.0`) with the six-target matrix, expect exactly
**29 release assets**:

- `transist-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `transist-<tag>-x86_64-unknown-linux-gnu.tar.gz.sha256`
- `transist-<tag>-x86_64-unknown-linux-gnu.tar.gz.minisig`
- `transist-<tag>-x86_64-unknown-linux-musl.tar.gz`
- `transist-<tag>-x86_64-unknown-linux-musl.tar.gz.sha256`
- `transist-<tag>-x86_64-unknown-linux-musl.tar.gz.minisig`
- `transist-<tag>-aarch64-apple-darwin.tar.gz`
- `transist-<tag>-aarch64-apple-darwin.tar.gz.sha256`
- `transist-<tag>-aarch64-apple-darwin.tar.gz.minisig`
- `transist-<tag>-x86_64-apple-darwin.tar.gz`
- `transist-<tag>-x86_64-apple-darwin.tar.gz.sha256`
- `transist-<tag>-x86_64-apple-darwin.tar.gz.minisig`
- `transist-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `transist-<tag>-aarch64-unknown-linux-gnu.tar.gz.sha256`
- `transist-<tag>-aarch64-unknown-linux-gnu.tar.gz.minisig`
- `transist-<tag>-aarch64-unknown-linux-musl.tar.gz`
- `transist-<tag>-aarch64-unknown-linux-musl.tar.gz.sha256`
- `transist-<tag>-aarch64-unknown-linux-musl.tar.gz.minisig`
- `build-provenance-x86_64-unknown-linux-gnu.json`
- `build-provenance-x86_64-unknown-linux-musl.json`
- `build-provenance-aarch64-apple-darwin.json`
- `build-provenance-x86_64-apple-darwin.json`
- `build-provenance-aarch64-unknown-linux-gnu.json`
- `build-provenance-aarch64-unknown-linux-musl.json`
- `packaging-provenance.json`
- `transist-<tag>-sbom.tar.gz`
- `transist-release.minisign.pub`
- `release-manifest.txt`
- `release-manifest.txt.minisig`

GitHub artifact attestations, when available, are separate from this 29-file
release asset set.

Each archive is a minimal evaluation bundle containing `bin/transist`,
`bin/transist-server`, `bin/transist-admin`, `plugins/shell/` with
`plugins/shell.sig`, `examples/quickstart/`,
`workflows/demos/getting_started/`, the distribution `README.md` (the only
Markdown README in an archive), the legal notices `LICENSE`, `NOTICE`, and
`THIRD_PARTY_NOTICES.md`, plus `build-provenance.json` and
`packaging-provenance.json`. The former records the
native target runner, source commit/tree, Rust/Cargo versions, OS tools, and
observed Xcode version/build. The latter records the source commit/tree and the
single pinned Linux environment that creates all six final archives. Linux
archives additionally contain `bin/transist-sandbox-launcher`; macOS
archives do not. Both provenance records are required; packaging fails if
either is missing.

Each archive also contains the complete Git-tracked `.agents/skills/` tree.
Archive validation checks every tracked skill member and its source bytes.
Skills are ancillary guidance, not a runtime, API, or schema contract.

**Documentation is not archive payload.** Public and developer documentation
stay in the source repository; the public documentation is published at
[transist.ai/docs](https://transist.ai/docs). Repository documentation,
governance files, global schemas, CI scripts, developer trees, and source-only
README files are not shipped in archives. The complete Git-tracked
`.agents/skills/` tree is the ancillary guidance exception. Archive ancillary
content is not a versioned runtime, API, or schema surface.
No separate documentation archive is published.

## Release Manifest

`release-manifest.txt` has **27 entries**, sorted by filename, with SHA-256
digests for every payload asset: platform archives, checksum files, archive
signatures, per-target build provenance, final-packaging provenance, the SBOM
archive, and the committed verification key. It excludes itself and its own
signature by definition.

The manifest is signed with `minisign` and verified before publication. The
`publish` operation compares the draft's exact asset names and digests
against the manifest and fails on any missing or extra asset.

Unsigned archive validation is an explicit pre-sign step only. The release
workflow passes `--allow-unsigned` while the package and archive-smoke jobs
validate archives before signatures exist. Post-sign, downloaded-draft, attestation, and publish
checks require the archive and manifest signatures; unsigned mode is not used
on those paths.

After staging uploads the draft, the workflow re-downloads the **uploaded**
draft and independently validates it before attestation and publication. The
check runs `scripts/ci/verify_downloaded_draft.sh`, which reruns — for all
six targets and for all shipped binaries — archive checksum-sidecar
verification, archive member inspection (single expected package directory, no
links/special/unsafe paths), and provenance-to-packaged-binary digest
comparison, plus SBOM semantic validation. These checks operate on the asset
set that was actually uploaded, not on the pre-upload staging copy; the
workflow never re-uploads anything during verification. The workflow passes
the resolved tagged commit and tree explicitly; both native and final-packaging
provenance must match those values, and verification fails closed when the
expected values are omitted or differ.

## Verify The Checksum

```bash
tag=v0.1.1 # select the release tag being verified
shasum -a 256 -c "transist-${tag}-x86_64-unknown-linux-gnu.tar.gz.sha256"
```

## Verify The Minisign Signature

First, obtain the committed verification key from the repository (not from
the release page alone):

```bash
# From a clone of the repository:
cp transist-release.minisign.pub ./

# Compare the downloaded release key with the key from the repository clone.
cmp -s transist-release.minisign.pub /path/to/transist/transist-release.minisign.pub
# A zero exit status is required before using the key.

# Optional human-readable cross-check:
head -1 /path/to/transist/transist-release.minisign.pub
# Should show: untrusted comment: minisign public key 202B06AC561F2B56
```

Then verify each archive:

```bash
minisign -Vm "transist-${tag}-x86_64-unknown-linux-gnu.tar.gz" \
  -x "transist-${tag}-x86_64-unknown-linux-gnu.tar.gz.minisig" \
  -p transist-release.minisign.pub
```

`release-manifest.txt` can be verified the same way:

```bash
minisign -Vm release-manifest.txt \
  -x release-manifest.txt.minisig \
  -p transist-release.minisign.pub
```

## Verify The Manifest

After verifying the manifest signature, check that every listed asset exists
and its digest matches:

```bash
# Verify each entry in the manifest
while IFS= read -r line; do
  digest="$(echo "$line" | awk '{print $1}')"
  filename="$(echo "$line" | awk '{print $2}')"
  actual="$(shasum -a 256 "$filename" | cut -d' ' -f1)"
  if [[ "$digest" != "$actual" ]]; then
    echo "MISMATCH: $filename"
    exit 1
  fi
done < release-manifest.txt
echo "All manifest entries verified."
```

## Verify The GitHub Attestation

When the release repository publishes GitHub artifact attestations, verify the
archive with:

```bash
gh attestation verify "transist-${tag}-x86_64-unknown-linux-gnu.tar.gz" \
  --repo transist-ai/transist
```

Use `gh attestation download` when you need the attestation bundle for offline
inspection.

Attestations are generated only after the complete draft, manifest, key, and
source commit/tree provenance have been verified, and before final release
publication.

## Inspect The SBOM

The release SBOM archive contains CycloneDX JSON generated from the
workspace build inputs.

```bash
tar -tzf "transist-${tag}-sbom.tar.gz"
```

Unpack it when you need to inspect package names, versions, and licenses during
review or incident response.

## Draft-First Publication

Successful merges to `main` pass exact-SHA mainline/security checks and automatic
hosted aggregate readiness before six-platform building, packaging, signing,
uploading and downloaded-draft verification. A ready candidate has all 29 assets,
source commit/tree provenance, verified attestations and a durable digest receipt.
No permanent version tag exists yet. Failed preparations remain inspectable and
cannot become ready or supersede a valid draft.

### Download and test a ready draft

Drafts require authenticated repository write access even in a public repository.
Open the draft URL in the preparation summary; verify the candidate ID, SHA/tree,
version, manifest digest and preparation results. The candidate draft's tag is
`candidate-<full-source-SHA>-<run-ID>-<attempt>`; filenames use the final version.
Download by the numeric **release ID** in that summary/receipt, not by final tag:

```bash
# Set the exact reviewed release ID; list all pages before downloading assets.
release_id=12345
gh api --paginate "repos/transist-ai/transist/releases/${release_id}/assets?per_page=100" \
  --jq '.[] | [.id, .name, .size] | @tsv'
# For each expected asset, use its numeric asset ID and exact filename.
gh api -H 'Accept: application/octet-stream' \
  repos/transist-ai/transist/releases/assets/67890 > release-manifest.txt
```

Download the complete expected set into a fresh directory. Apply the signature,
checksum, manifest, committed-key comparison, provenance, SBOM and attestation
checks above. Obtain the verification key from the receipt's exact source SHA,
not from the draft alone. On a supported isolated test host, inspect archive
members before extraction, then test the downloaded binaries and bundled examples.
Record the candidate ID and tested digests. Draft access or missing platform test
coverage is a limitation, not verification evidence.

The normal operator path is **review/test the ready draft, then approve its
`release-publish` deployment once**. Publication revalidates current policy and
exact-SHA checks, atomically reserves `ready → publishing`, rechecks all downloaded
digests, creates/verifies the annotated final tag and makes the same release public.
Publication never rebuilds, re-signs, uploads, deletes or replaces assets.

A newer fully verified descendant can supersede an older ready candidate while
review is pending; its draft is retained but approval cannot publish it. A
publication reservation cannot be superseded. Interrupted publication retains that
reservation: dispatch **Release Recovery** on `main` with the same candidate ID
and provide fresh approval. Recovery verifies existing effects and resumes without
rebuilding. A matching already-public release is idempotent success; changed bytes,
wrong release identity or a lightweight/wrong-target tag fail hard. Never move or
delete a permanent tag or overwrite assets. Source changes require a new version;
an existing reservation requires investigation/reconciliation, never automatic reset.
See the [release environment and rollback procedure](../../developer/release-environment.md).
