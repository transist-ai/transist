# Trust: Determinism and Auditability

Transist is built for workflows where teams need to trust the execution record. It focuses on deterministic runtime behavior and durable audit evidence.

## What Transist Guarantees

Transist does not control the whole world, but it does control its own runtime behavior. Transist guarantees that its documented runtime logic is explicit and persisted:

* **Validated Workflows**: Every workflow is checked against strict rules before it ever runs.
* **Locked-in Recipes**: Once a run starts, the instructions (the "recipe") are snapshotted and never change.
* **Predictable Storage**: Every artifact, event, and piece of evidence is stored in a structured, immutable location.
* **Bounded Recovery**: When you **Resume** or **Replay**, Transist uses saved records and explicit semantics. External systems and time-dependent side effects remain outside that guarantee.

## What it *Doesn't* Guarantee

Transist does not (and cannot) make external tools deterministic. If a shell script behaves differently because of a network lag, or if an AI model gives two different answers to the same prompt, Transist won't "fix" that.

Instead, Transist preserves the runtime context needed to inspect what happened. It is not making the world predictable; it is making workflow execution around external systems explicit and reviewable.

## The Audit Trail

Auditability is part of the core product value. By looking at a run folder, an operator, workflow author, or auditor can reconstruct the persisted runtime record:

1. **What was the plan?** (`recipe.yaml`)
2. **What were the inputs?** (`run_inputs.json`)
3. **What trusted bytes and policy decision did the runtime evaluate?** (`provenance.json`)
4. **What happened, step-by-step?** (`events.jsonl`)
5. **What was produced?** (`artifacts/`)
6. **What is the proof?** (`evidence/`)
7. **Who approved the move?** (`gates/`)

This level of detail turns automation from a black box into a recorded process that authorized reviewers can inspect.

## Detached Signature Contract

Transist shares a detached-signature contract with `transist-admin`, but the
trust boundary stays strict: `transist-admin` creates trust and `transist`
verifies or enforces it.

* Workflow-definition digest: SHA-256 over the full authored definition set rooted at the workflow file, including referenced subflows, referenced `prompt_file` targets, and `source://` helper scripts. Each file is LF-normalized first. Templated `prompt_file` paths are refused at sign time.
* Plugin-package digest: `plugin_dir_sha256` over exactly `plugin.yaml` plus
  the manifest-declared `package.files`. Unselected files in the source
  directory are not part of the digest or installed package.
* `transist-admin key generate` writes private signing keys and `transist-admin key export-public` emits importable `transist.trust_root.v1` public records.
* Runtime machines should import public trust roots, not carry private signing keys.
* `<subject>.sig` is the default signature-sidecar location. Admin signing can
  use `--output <path>`, and trusted installs can select that detached file with
  `--signature <path>`.
* Runtime trust roots are imported with `transist trust import <trust-root-file>` and listed with `transist trust list`.
* The runtime does not generate keys, export public trust roots, sign content, or inspect private-key material. It only imports public keys and verifies detached signatures.
* Governed homes automatically require trusted execution for workflow/plugin
  install, prepare, workflow run, resume, replay, cleanup, and preflight. The
  legacy `--trusted-execution` flag remains accepted but is redundant there.
* When trusted execution is enabled, the runtime records `transist.trust_decision.v1` on the run before execution continues or is refused.
* The runtime also writes additive `transist.run_provenance.v1` into `provenance.json` so trust outcomes, workflow/plugin digests, run-start policy metadata, and denied-run details survive interruption and later inspection.

---

### Related

* [Determinism, Auditability, and Governance Guide](../../guides/trust/determinism-auditability-and-governance-by-example.md)
* [TEP (Transist Execution Protocol) Reference](../../reference/tep.md)
* [Error Taxonomy Reference](../../reference/error-taxonomy.md)

**Navigation:**

* Previous: [Troubleshooting](../03-inspection-and-recovery/05-troubleshooting.md)
* Next: [Secrets and Policy](02-secrets-and-policy.md)
