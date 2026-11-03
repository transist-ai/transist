# transist-admin

`transist-admin` is the canonical OSS admin CLI for Transist trust creation.

It creates private signing keys, exports public trust roots, packages and signs
workflow and plugin artifacts, performs local verify dry-runs, and inspects
detached signatures. `transist` remains verification-only: it imports trust
roots, verifies signed packages or detached signatures, and enforces trust
policy at runtime.

## Trust Boundary

- `transist-admin` creates trust.
- `transist` imports public trust roots and verifies or enforces trust.
- Runtime machines should not carry private keys.
- Signing, key generation, private-key handling, and signature inspection must
  not move into runtime crates.

## Canonical Commands

Generate trust material:

```bash
cargo run -p transist-admin -- key generate --name acme --private-key-file ./trust/acme.key
cargo run -p transist-admin -- key export-public --private-key-file ./trust/acme.key --trust-root-file ./trust/acme.pub
```

Publish a governed workflow package:

```bash
transist-admin workflow package ./path/to/workflow.yaml --version 0.1.0 \
  --private-key-file ./trust/acme.key --output ./path/to/package-0.1.0
transist-admin workflow verify ./path/to/package-0.1.0/workflow.yaml \
  --signature ./path/to/package-0.1.0/transist.signature.v1 \
  --trust-root-file ./trust/acme.pub
```

The package command captures the workflow's declared subflows, prompts, and
`source://` assets, then signs the immutable package contents. Publish the
verified package to a governed runtime with `transist workflows install`.

Sign workflow and plugin artifacts directly when an immutable package is not
needed:

```bash
transist-admin workflow sign ./examples/quickstart/quickstart.workflow.yaml --private-key-file ./trust/acme.key
transist-admin plugin sign ./plugins/shell --private-key-file ./trust/acme.key
```

Dry-run verification:

```bash
transist-admin workflow verify ./examples/quickstart/quickstart.workflow.yaml --signature ./examples/quickstart/quickstart.workflow.yaml.sig --trust-root-file ./trust/acme.pub
transist-admin plugin verify ./plugins/shell --signature ./plugins/shell.sig --trust-root-file ./trust/acme.pub
```

Inspect detached signatures:

```bash
transist-admin signature inspect ./examples/quickstart/quickstart.workflow.yaml.sig
transist-admin signature inspect ./plugins/shell.sig
```

## Maintenance

`dev regenerate-fixtures` is a repository-maintainer command used to refresh the
committed trust test vectors:

```bash
cargo run -p transist-admin -- dev regenerate-fixtures --fixture-root tests/fixtures/admin_tooling
```

## Command Contract

### `key generate`

```bash
transist-admin key generate --name <name> --private-key-file <private-key-file>
```

- Writes one private key JSON file with schema `transist.admin.key.v1`.
- Output file is operator-private and must use `0600` permissions on Unix.
- `key_id` must equal the SHA-256 hex digest of the raw Ed25519 public key
  bytes.
- Fails if the parent directory cannot be created or the output file cannot be
  written.

### `key export-public`

```bash
transist-admin key export-public --private-key-file <private-key-file> --trust-root-file <trust-root-file>
```

- Reads one `transist.admin.key.v1` private key file.
- Writes one public trust root JSON file with schema `transist.trust_root.v1`.
- The output is suitable for `transist trust import`.
- Fails closed on malformed key JSON, invalid base64, invalid Ed25519 key
  bytes, mismatched public or private key material, or a `key_id` mismatch.

### `workflow sign`

```bash
transist-admin workflow sign <workflow-path> --private-key-file <private-key-file> [--output <signature-file>]
```

- Accepts a top-level workflow YAML file.
- Computes the canonical workflow-definition digest from monorepo digest logic.
- Writes one detached `transist.signature.v1` sidecar. Without `--output`, it
  writes `<workflow-path>.sig`; use `--output` to publish the sidecar elsewhere.
- The signature subject must record `kind`, `id`, `version`, and `digest`.
- Fails closed when the workflow cannot be resolved, parsed, or digested, or
  when the key file is malformed.

### `workflow package`

```bash
transist-admin workflow package <workflow-path> --version <version> --private-key-file <private-key-file> --output <package-directory>
```

- Validates the workflow and its canonical version identifier.
- Captures the workflow definition and declared detached assets into an
  immutable package directory.
- Signs and self-verifies the captured package before publishing it with
  no-clobber semantics.
- Use this command for governed workflow delivery. It is not a replacement
  for the standalone `workflow sign` sidecar command when a sidecar alone is
  the intended artifact.

### `plugin sign`

```bash
transist-admin plugin sign <plugin-dir-or-plugin.yaml> --private-key-file <private-key-file> [--output <signature-file>]
```

- Accepts a plugin directory or `plugin.yaml` path.
- Archive inputs are not supported in v1.
- Computes the deterministic plugin digest from `plugin.yaml` plus the
  manifest-declared `package.files`.
- Rejects symlinks and special files selected by the plugin manifest's
  `package.files` list.
- Writes one detached `transist.signature.v1` sidecar. Without `--output`, it
  writes `<plugin-dir>.sig`, including when the input is `plugin.yaml`; use
  `--output` to publish the sidecar elsewhere.
- Fails closed when the plugin cannot be resolved, parsed, or digested, or when
  the key file is malformed.

### `workflow verify`

```bash
transist-admin workflow verify <workflow-path> --signature <signature-file> --trust-root-file <trust-root-file>
```

- Recomputes the workflow digest and verifies a detached signature against one
  trusted `transist.trust_root.v1` file.
- Prints the verification result.
- Exits `0` only when verification succeeds.
- Fails closed for tampered subjects, malformed signatures, invalid trust roots,
  unsupported algorithms or schema values, and untrusted keys.

### `plugin verify`

```bash
transist-admin plugin verify <plugin-dir-or-plugin.yaml> --signature <signature-file> --trust-root-file <trust-root-file>
```

- Recomputes the manifest-selected plugin payload digest and verifies a detached
  signature against one trusted `transist.trust_root.v1` file.
- Accepts a plugin directory or `plugin.yaml` path only.
- Prints the verification result.
- Exits `0` only when verification succeeds.
- Fails closed for tampered subjects, malformed signatures, invalid trust roots,
  unsupported algorithms or schema values, untrusted keys, and selected plugin
  payload files that are symlinks or special files.

### `signature inspect`

```bash
transist-admin signature inspect <signature-file>
```

- Reads one detached `transist.signature.v1` envelope.
- Prints subject metadata without requiring a trusted key.
- Must report subject, digest, key id, algorithms, timestamp, and `subject_key`.
- Fails closed on malformed JSON or unsupported signature schema.

## JSON Schemas

### `transist.admin.key.v1`

Private signing key file:

```json
{
  "schema": "transist.admin.key.v1",
  "name": "acme",
  "key_id": "<sha256-hex-of-raw-ed25519-public-key>",
  "private_key": "<base64-encoded-32-byte-ed25519-private-key>",
  "public_key": "<base64-encoded-32-byte-ed25519-public-key>"
}
```

### `transist.trust_root.v1`

Public trust root file:

```json
{
  "schema": "transist.trust_root.v1",
  "name": "acme",
  "key_id": "<sha256-hex-of-raw-ed25519-public-key>",
  "public_key": "<base64-encoded-32-byte-ed25519-public-key>"
}
```

### `transist.signature.v1`

Detached signature envelope:

```json
{
  "schema": "transist.signature.v1",
  "sig_alg": "ed25519",
  "digest_alg": "sha256",
  "subject": {
    "kind": "workflow",
    "id": "demo.workflow",
    "version": "0.1.0",
    "digest": "<sha256-hex>"
  },
  "key_id": "<sha256-hex-of-raw-ed25519-public-key>",
  "signed_at_ms": 0,
  "signature": "<base64-encoded-ed25519-signature>"
}
```

## Runtime Flow

```bash
transist trust import ./trust/acme.pub
transist trust list
transist trust inspect <key-id>
transist trust export <key-id> --output ./acme.pub
transist trust remove <key-id> --yes
transist plugins install --path ./plugins/shell/plugin.yaml --trusted-execution
transist workflows install --package ./workflow-package-0.1.0
transist workflows check demo.workflow@0.1.0 --inputs-file ./inputs.json
transist workflows authorize demo.workflow@0.1.0 --input repo_path=/path/to/repo
transist workflows check demo.workflow@0.1.0 --inputs-file ./inputs.json
transist workflows run demo.workflow@0.1.0 --inputs-file ./inputs.json --trusted-execution
```

Installed checks and authorization use probe-free validation, not live readiness.
Passing validation leaves readiness `not_ready`; add `--live` or `--output` to
check live readiness. Preparation still checks live conditions before run birth.
Grant application reevaluates current inputs/resources/trust/policy and aborts
without writes if the exact reviewed fragment or destination changed.

`trust remove` only changes the local runtime trust store for future verification. It does not revoke or rewrite historical trust decisions already persisted by prior runs.
Workflow packaging and signing remain administrative operations. The runtime
installs the signed package without inputs or policy mutation; authorization is
the separate, exact authority-writing operation.

## Verification

```bash
cargo fmt --all
cargo test -p transist-admin
cargo test --test admin_round_trip
cargo test --test admin_tooling_digest_parity
scripts/ci/check_no_runtime_signing.sh
scripts/ci/check_workspace_dependencies.sh
```
