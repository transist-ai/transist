# Trusted Governed Execution Walkthrough

This walkthrough runs the full offline trusted local loop from an extracted
Transist release archive: import a demo trust root, install the signed `shell`
plugin and a signed workflow, run with trusted execution and local-only,
cancel during a bounded sleep, retry the same run, inspect the local record,
and export evidence. It uses no account and no network.

The complete sequence is verified automatically by
`scripts/ci/run_quickstart_smoke.sh`. Run that script to validate a release
archive or a source build without copy-pasting the steps.

For a new governed workflow version, use `transist-admin workflow package` on
an authorized administrative machine, then import only its public trust root
into the runtime home. Install the resulting package, check the prospective
workflow invocation, authorize only exact missing authority, and check again
before preparing or running. The runtime never opens a private key or signs an
artifact.

For the operator-owned lifecycle, including creating and signing new plugin and
workflow versions, see [Governed Runtime Setup and Delivery](governed-runtime-setup-and-delivery.md).

For a simpler entry point that needs no signing or trust setup, see the
[Quickstart](../getting-started/quickstart.md).

## Prerequisites

- An extracted Transist release archive for your platform
  (`transist-<tag>-<target>.tar.gz`) containing `bin/transist`,
  `bin/transist-admin`, `plugins/shell/`, `plugins/shell.sig`, and
  `examples/quickstart/`, plus ancillary `.agents/skills/` guidance.
- Python 3 for the bundled `shell` plugin.
- `jq` (to extract the effective home path from `transist home get`).

**On Linux**, the `shell` plugin uses `bwrap` (bubblewrap) to sandbox child
processes. Install it before running any workflow. See
[Installation and Local Setup](../getting-started/installation-and-local-setup.md)
for per-distro install commands.

All commands below run from the extracted archive root with that directory on
your `PATH` (or invoke `./bin/transist` directly). The walkthrough never
needs the network.

## 0. Use the archive binaries

```bash
export PATH="$PWD/bin:$PATH"
transist --version
```

## 1. Create a throwaway project and initialize a governed home

```bash
mkdir quickstart-project && cd quickstart-project
transist home init
```

`home init` (with no `--home` flag and no `TRANSIST_HOME`) creates a governed
home at the per-user default location
(`${XDG_STATE_HOME:-$HOME/.local/state}/transist`), outside the current
project directory. Every `transist` command run from anywhere on this machine
resolves to the same home unless you pass `--home`, set `TRANSIST_HOME`, or run
`transist home set <path>`. Governed homes default to `policy.profile:
allowlist` and require trusted workflow/plugin verification.

## 2. Grant the exact capabilities the walkthrough needs

The governed allowlist denies missing or partial capability grants before
dispatch. The shipped `examples/quickstart/quickstart.config.yaml` template
carries the exact default-deny grants for this workflow (adapter, plugin,
action, provider, and filesystem resources). Substitute the absolute workspace
path and write it over the home config:

```bash
home_dir="$(transist home get | jq -r .effective_home)"
sed "s#__WORKSPACE__#$(pwd -P)#g" \
  ../examples/quickstart/quickstart.config.yaml > "$home_dir/config.yaml"
```

The workspace capability grant is path-exact (no wildcards), so it must match
the directory you run from.

## 3. Import the demo-only public trust root

```bash
transist trust import ../examples/quickstart/trust/quickstart-demo.pub
transist trust list
```

`quickstart-demo` is a throwaway public trust root shipped only so this
walkthrough can prove the signed-install → trusted-run loop without asking
you to generate keys. It is not a production trust root. Real deployments
create trust on an admin host with `transist-admin` and retain private keys
outside the runtime home.

## 4. Install the signed shell plugin and signed workflow package

```bash
transist plugins install --path ../plugins/shell/plugin.yaml --trusted-execution
# Obtain this immutable package from its publisher; do not rebuild it on the runtime host.
export PACKAGE_DIR=/path/to/quickstart-package-v1
transist workflows install --package "$PACKAGE_DIR"
```

`--trusted-execution` verifies the shipped `plugins/shell.sig` and
package-contained `transist.signature.v1` material against the imported public
root before any bytes are copied or compiled. Governed homes require this even
when the flag is omitted.

The plugin's verified bytes and the workflow package snapshot are retained in
the runtime home. Workflow installation has no inputs and never grants policy
authority or creates a run.

## 5. Check and, when needed, authorize before the run

```bash
transist workflows check quickstart.demo@0.1.0 --inputs-file ./inputs.json
# Only when the check reports exact missing authority:
transist workflows authorize quickstart.demo@0.1.0 --inputs-file ./inputs.json
transist workflows check quickstart.demo@0.1.0 --inputs-file ./inputs.json
```

`check` and blocked authorization are read-only and return `run_created: false`.
Installed `check` and authorization use probe-free validation. Passing validation
reports `validation_status: passed` while readiness remains `not_ready`.
Use `check --live` or `--output readiness.json` when live readiness is required;
preparation still checks live conditions. Applying grants revalidates the exact
reviewed fragment; changed inputs/resources/policy require another review without writes.
`authorize` accepts the same repeatable `--input key=value` and
`--inputs-file` forms as `check`; use values that describe the same prospective
invocation. The interactive report separates input/action-schema failures from
pending grants; if both appear, correct inputs and rerun `check` before trying
to authorize. For machine-readable filtering, use `--format json` and inspect
`diagnostics.groups` and `diagnostics.authorization.blocking_codes` (see the
[CLI reference](../../reference/cli.md)).
Default and piped reports are concise; add `--format json` before parsing JSON.
`check --output readiness.json` writes full canonical evidence and displays a
summary from the same evaluation. Noninteractive grant application requires
`--approve`; JSON never prompts.

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

After `prepare` creates a run, use `runs show` and `runs preflight` for durable
diagnostics.

## 6. Run offline with trusted execution and local-only

```bash
transist workflows run quickstart.demo@0.1.0 --run-ref quickstart-run \
  --trusted-execution --local-only --input subject=transist-demo --ui plain
```

- `--trusted-execution` re-verifies the signed workflow and plugin before
  dispatch and persists `trust_decision` on the run.
- `--local-only` denies any invoke step whose effective plugin-action
  `provider_kind` is `external` before dispatch. The bundled `shell` plugin
  declares `default_provider_kind: local`, so its `exec` action is allowed
  locally. Under local-only the runtime also caps the shell process at
  `network: loopback`, so the shell action cannot make non-loopback connections.

`run` starts a detached runner and returns. The first workflow step sleeps
for a bounded window so you can cancel it.

The fullscreen TUI provides the same trusted run path without a command-line
flag. After selecting a workflow, governed homes proceed with trusted execution
automatically; standalone and development homes show an execution-mode picker
with standard execution preselected. The selected mode is preserved while
moving back through the run-reference and input steps.

The TUI does not package, compile, install, promote, or uninstall workflows.
It evaluates the same read-only fast validation as `workflows check`, can
review and explicitly apply exact authorization, and reruns validation before live
preparation. For governed delivery, create the workflow package with
`transist-admin workflow package` and install it with `transist workflows
install` before running it from the operator console.

## 6. Cancel during the sleep window

From the same directory (another shell or after `run` returns):

```bash
transist runs cancel quickstart-run
transist runs status quickstart-run
```

`run` detaches the runner, so a single `runs status` may observe it before
it finishes. Poll in a bounded loop until the run reaches `canceled`, failing
closed if it never settles within the window:

```bash
settled=""
for i in $(seq 1 60); do
  st=$(transist runs status quickstart-run 2>/dev/null | head -1 || true)
  case "$st" in
    *canceled*|*success*) settled="$st"; break ;;
    *failed*)
      echo "run ended: $st" >&2
      exit 1
      ;;
  esac
  sleep 0.5
done
if [[ -z "$settled" ]]; then
  echo "run did not settle before the poll window expired" >&2
  exit 1
fi
```

## 7. Retry the same run

```bash
transist runs retry quickstart-run --trusted-execution --local-only
```

`retry` re-enters the canceled run from persisted state, re-verifies trust,
re-runs the canceled step, and continues to the reporting step. It keeps the
same run id and preserves prior execution history.

## 8. Inspect the local record

```bash
transist runs show quickstart-run
transist runs tail quickstart-run --limit 50
```

The run record carries `local_only_mode`, `trusted_execution_mode`,
`policy_profile: allowlist`, and `capability_enforcement: enforced`. The report
step promotes an authored evidence file, under the effective home directory
resolved in step 2 (`transist home get | jq -r .effective_home`):

```text
<home>/runs/run_quickstart-run/artifacts/write_report/execution_001/evidence/000_summary.txt
```

## 9. Export the evidence summary

```bash
transist runs evidence export quickstart-run
```

The export renders a markdown summary from the persisted execution history
and provenance, including the verified workflow digest, the verified
`shell@0.1.0` plugin digest, and the signer key id.

## What this proves

- authored workflow definitions and plugin packages are signed outside the
  runtime, then verified locally before install and dispatch;
- the runtime imports trust roots and verifies executable inputs offline;
- governed, default-deny capability grants are required before dispatch;
- execution can be canceled and recovered without losing the run record;
- artifacts, evidence, and inspection stay on local disk;
- the full loop works offline with local surfaces and no external providers.

## Verify it automatically

From the extracted archive root (the smoke script is included in the archive):

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

Release CI runs this archive form from the final package on all three
supported native targets: Linux x86-64 (glibc and musl) and macOS Apple
Silicon.

The smoke isolates each run inside a temporary project/home, removes it
through a trap, and never generates a secret, private key, or runtime home
inside the checkout.
