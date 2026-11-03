# Quickstart Fixture

This directory is the self-contained, offline quickstart fixture shipped
inside every release archive. Its contents are used by two documented paths:

- **Standalone quickstart** — the
  [Quickstart guide](../../docs/public/guides/getting-started/quickstart.md)
  installs `quickstart.workflow.yaml` unsigned in a standalone home and runs it.
  The standalone leg of `scripts/ci/run_quickstart_smoke.sh` verifies this path.
- **Trusted governed walkthrough** — the
  [Trusted Governed Execution Walkthrough](../../docs/public/guides/trust/trusted-execution-walkthrough.md)
  imports `trust/quickstart-demo.pub`, installs the signed plugin and workflow
  with `--trusted-execution`, and exercises the full cancel → retry → inspect →
  evidence-export loop. The governed leg of `scripts/ci/run_quickstart_smoke.sh`
  verifies this path.

## Contents

- `quickstart.workflow.yaml` — the authored `transist.flow.v1` workflow. It
  uses only the bundled `shell` plugin (now `default_provider_kind: local`) and
  writes run-scoped evidence through `$TRANSIST_EVIDENCE_DIR`. It carries
  a top-level `shell: 0.1.0` dependency pin and no network access.
- `quickstart.workflow.yaml.sig` — a detached `transist.signature.v1` sidecar
  over the workflow definition (required for the governed trusted path only).
- `quickstart.config.yaml` — the governed home `config.yaml` template with
  default-deny `policy.allow.capabilities` grants. The `__WORKSPACE__` token is
  replaced with the absolute workspace path by
  `scripts/ci/run_quickstart_smoke.sh` so the dynamic workspace capability is
  granted exactly. Not used by the standalone path.
- `trust/quickstart-demo.pub` — a `transist.trust_root.v1` public key required
  for the governed trusted path only. It is the only trust material shipped in
  the archive. The matching private key is never committed or shipped; the
  current demo-only signing key was generated in an isolated temporary admin
  directory and destroyed after signing.

The bundled `plugins/shell.sig` (next to `plugins/shell/`) is the matching
detached signature over the shipped `shell` plugin payload: `plugin.yaml` plus
the files declared in its `package.files` list.

## Demo-only trust

The bundled trust root exists only so the governed walkthrough can prove the
full signed-install → trusted-run → cancel → retry → inspect → export loop
without asking the operator to generate keys. Real deployments create trust on
an admin host with `transist-admin` and import only the public root on the
runtime host.

## Running the smoke script

Both the standalone and governed legs run when you invoke the smoke:

```bash
bash scripts/ci/run_quickstart_smoke.sh
```

From a source checkout (debug build):

```bash
cargo build -p transist-cli -p transist-admin
bash scripts/ci/run_quickstart_smoke.sh
```

To verify a tarball without extracting it first:

```bash
bash scripts/ci/run_quickstart_smoke.sh --archive ./transist-<tag>-<target>.tar.gz
```

## Re-signing after plugin/workflow changes

`plugins/shell.sig` and `quickstart.workflow.yaml.sig` cover exact bytes. If
the selected shell payload (`plugin.yaml` or any file in `package.files`) or
`quickstart.workflow.yaml` changes, generate a fresh explicitly demo-only key
on an admin host, regenerate both sidecars, export the matching public root,
and destroy the private key. Never use a production key for these fixtures:

```bash
transist-admin key generate --name quickstart-demo-test-only \
  --private-key-file <isolated-temp-directory>/test-only.key
transist-admin plugin sign plugins/shell \
  --private-key-file <isolated-temp-directory>/test-only.key
transist-admin workflow sign examples/quickstart/quickstart.workflow.yaml \
  --private-key-file <isolated-temp-directory>/test-only.key
transist-admin key export-public \
  --private-key-file <isolated-temp-directory>/test-only.key \
  --trust-root-file examples/quickstart/trust/quickstart-demo.pub
```

Never commit the private key. Never ship it in a release archive.

When the signer changes, update both sidecars and the public root together.
Import the refreshed `trust/quickstart-demo.pub` into an existing demo home
before installing the newly signed artifacts. Prefer a fresh demo home when
the selected plugin payload changed: installed package versions are immutable.
