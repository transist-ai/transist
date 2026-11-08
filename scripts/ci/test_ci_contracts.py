#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Hermetic CI wiring/mutation tests and subprocess tests of the real gate.

Uses only the standard library. Workflow checks deliberately inspect the
repository's block-job/step formatting, not general YAML; actionlint remains
the YAML/Actions syntax validator. No builds, package installs or network calls.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
EVALUATOR = ROOT / "scripts/ci/check_gate_results.sh"
EARLY = {
    "ci-static": "CI_STATIC_RESULT",
    "commit-signoff": "COMMIT_SIGNOFF_RESULT",
    "markdownlint": "MARKDOWNLINT_RESULT",
    "cargo-deny": "CARGO_DENY_RESULT",
    "dependency-review": "DEPENDENCY_REVIEW_RESULT",
}
EXPENSIVE = {
    "validate-full": "VALIDATE_RESULT",
    "schema-api-contracts": "SCHEMA_RESULT",
    "coverage": "COVERAGE_RESULT",
}
PR = {**EARLY, **EXPENSIVE, "preflight-gate": "PREFLIGHT_RESULT"}
MAIN = {
    "platform-smoke": "PLATFORM_RESULT",
    "musl-smoke": "MUSL_RESULT",
    "coverage": "COVERAGE_RESULT",
}
SECURITY = {"security-audit": "AUDIT_RESULT", "codeql": "CODEQL_RESULT"}
PUBLIC_ONLY = "${{ github.event.repository.private == false }}"
JOB = re.compile(r"^  ([\w-]+):[^\S\n]*\n(.*?)(?=^  [\w-]+:|\Z)", re.M | re.S)
READY = "${{ github.event.pull_request.draft == false }}"
ALWAYS = "${{ always() }}"
PREFLIGHT_READY = (
    "${{ !cancelled() && github.event.pull_request.draft == false "
    "&& needs.preflight-gate.result == 'success' }}"
)
PR_FLAGS = {
    "IS_DRAFT": "${{ github.event.pull_request.draft }}",
    "IS_DEPENDABOT": "${{ github.actor == 'dependabot[bot]' }}",
    "DEPENDENCY_REVIEW_ENABLED": "${{ vars.ENABLE_DEPENDENCY_REVIEW }}",
}


def job_field(body: str, key: str) -> str:
    """Return a job-level scalar from a block; never read a nested step field."""
    match = re.search(rf"^    {re.escape(key)}: ([^\n]+)$", body, re.M)
    return match.group(1).strip() if match else ""


def job_needs(body: str) -> set[str]:
    """Read literal inline or block needs lists in the checked-in job format."""
    inline = job_field(body, "needs")
    if inline:
        return {item.strip() for item in inline.strip("[]").split(",")}
    match = re.search(r"^    needs:\n((?:      - [\w-]+\n)+)", body, re.M)
    return set(re.findall(r"- ([\w-]+)", match.group(1))) if match else set()


def workflow_errors(text: str, kind: str) -> list[str]:
    """Return CI contract violations for PR/main/security text without executing it."""
    errors: list[str] = []
    matches = list(JOB.finditer(text.partition("\njobs:\n")[2]))
    jobs = {match.group(1): match.group(2) for match in matches}
    if len(jobs) != len(matches):
        errors.append("duplicate job identity")
    graph = {name: job_needs(body) for name, body in jobs.items()}

    def visit(name: str, ancestors: set[str]) -> None:
        """Report unknown dependencies and cycles in the literal job graph."""
        if name not in jobs:
            errors.append(f"unknown dependency: {name}")
        elif name in ancestors:
            errors.append(f"dependency cycle: {name}")
        else:
            for dependency in graph[name]:
                visit(dependency, ancestors | {name})

    for name in jobs:
        visit(name, set())
    if not re.search(r"^defaults:\n  run:\n    shell: bash$", text, re.M):
        errors.append("missing workflow Bash default")

    gates = {
        "main": {"main-gate": MAIN},
        "pr": {"preflight-gate": EARLY, "pr-gate": PR},
        "security": {"security-gate": SECURITY},
    }[kind]
    required = set({"main": MAIN, "pr": PR, "security": SECURITY}[kind]) | set(gates)
    for name in required:
        if name not in jobs:
            errors.append(f"missing required job: {name}")
        body = jobs.get(name, "")
        if job_field(body, "continue-on-error"):
            errors.append(f"required job may not continue-on-error: {name}")

    for gate, bindings in gates.items():
        body = jobs.get(gate, "")
        if job_field(body, "name") != gate:
            errors.append(f"required gate identity changed: {gate}")
        if job_field(body, "if") != ALWAYS:
            errors.append(f"gate must always evaluate: {gate}")
        if job_needs(body) != set(bindings):
            errors.append(f"gate needs mismatch: {gate}")
        steps = re.split(r"^      - ", body, flags=re.M)
        mode = {
            "preflight-gate": "preflight",
            "pr-gate": "pr",
            "main-gate": "main",
            "security-gate": "security",
        }[gate]
        # Main wiring is owned by the parent. Preserve its existing env/needs
        # contract while its extracted evaluator is independently tested below.
        marker = {
            "main": "name: Require every mainline validation job",
            "security": "name: Require security checks",
            "pr": f"run: bash scripts/ci/check_gate_results.sh {mode}",
        }[kind]
        evaluators = [step for step in steps if marker in step]
        if len(evaluators) != 1:
            errors.append(f"missing single evaluator step: {gate}")
            continue
        step = evaluators[0]
        if re.search(r"^        (if|continue-on-error):", step, re.M):
            errors.append(f"gate evaluator may not skip/ignore failure: {gate}")
        env = {
            variable: "${{ needs." + job + ".result }}"
            for job, variable in bindings.items()
        }
        if kind == "security":
            env["REPOSITORY_PRIVATE"] = "${{ github.event.repository.private }}"
        if kind == "pr":
            env.update(PR_FLAGS)
            checkout = "ref: ${{ github.event.pull_request.head.sha }}"
            if checkout not in body or body.find("actions/checkout@") > body.find(
                marker
            ):
                errors.append(f"gate must checkout PR head before evaluator: {gate}")
        for variable, value in env.items():
            if f"          {variable}: {value}\n" not in step:
                errors.append(f"missing result/flag binding: {gate}/{variable}")

    if kind == "security":
        if job_field(jobs.get("codeql", ""), "if") != PUBLIC_ONLY:
            errors.append("CodeQL must run only for public repositories")
        if job_field(jobs.get("security-audit", ""), "if"):
            errors.append("security audit must run regardless of visibility")
        return errors
    if kind == "main":
        return errors
    if not text.startswith("name: PR Gate\n"):
        errors.append("PR workflow identity changed")
    if "validate-fast" in jobs or "check_pr_ready.sh --fast" in text:
        errors.append("duplicate nonrequired fast validation")
    for name in EXPENSIVE:
        body = jobs.get(name, "")
        # Schema/API document-review fixtures execute real plugins too. Every
        # Linux execution job needs the same sandbox backend and launcher.
        required_sandbox = (
            "--cap-add=SYS_ADMIN --cap-add=NET_ADMIN "
            "--security-opt apparmor=unconfined --security-opt seccomp=unconfined"
        )
        if f"      options: {required_sandbox}\n" not in body:
            errors.append(f"execution job missing sandbox options: {name}")
        if not re.search(r"install_debian_tools\.sh[^\n]*\bbubblewrap\b", body):
            errors.append(f"execution job missing bubblewrap: {name}")
        launcher = "install -Dm755 target/debug/transist-sandbox-launcher /usr/libexec/transist/transist-sandbox-launcher"
        if (
            "cargo build --locked -p transist-sandbox-launcher" not in body
            or launcher not in body
        ):
            errors.append(f"execution job missing sandbox launcher: {name}")
        if job_needs(body) != {"preflight-gate"}:
            errors.append(f"expensive job must need preflight: {name}")
        # An explicit status function prevents Actions from injecting success(),
        # which would suppress these jobs after accepted skipped ancestors.
        if "!cancelled()" not in job_field(body, "if"):
            errors.append(f"expensive job must explicitly check cancellation: {name}")
        if job_field(body, "if") != PREFLIGHT_READY:
            errors.append(
                f"expensive job must require ready successful preflight: {name}"
            )
        if job_field(body, "name") != name:
            errors.append(f"required job identity changed: {name}")
    for name in ("ci-static", "markdownlint", "cargo-deny"):
        if job_field(jobs.get(name, ""), "if") != READY:
            errors.append(f"early job must skip drafts: {name}")
        if job_needs(jobs.get(name, "")):
            errors.append(f"early job must run independently: {name}")
    for name, condition in {
        "commit-signoff": (
            "${{ github.event.pull_request.draft == false "
            "&& github.actor != 'dependabot[bot]' }}"
        ),
        "dependency-review": (
            "${{ vars.ENABLE_DEPENDENCY_REVIEW == 'true' "
            "&& github.event.pull_request.draft == false }}"
        ),
    }.items():
        if job_field(jobs.get(name, ""), "if") != condition:
            errors.append(f"PR skip exception changed: {name}")
        if job_needs(jobs.get(name, "")):
            errors.append(f"early job must run independently: {name}")
    commands = {
        "ci-static": "bash scripts/ci/check_ci_static.sh",
        "commit-signoff": (
            'bash scripts/ci/check_commit_signoffs.sh "$BASE_SHA" "$HEAD_SHA"'
        ),
        "validate-full": "bash scripts/ci/check_pr_ready.sh",
        "schema-api-contracts": "bash scripts/ci/check_schema_api_contracts.sh",
        "coverage": "bash scripts/ci/run_coverage.sh coverage",
        "cargo-deny": "cargo deny check",
        "markdownlint": "bash scripts/ci/check_markdown.sh",
    }
    for name, command in commands.items():
        if f"        run: {command}\n" not in jobs.get(name, ""):
            errors.append(f"required check command changed: {name}")
    static = jobs.get("ci-static", "")
    if "cargo " in static or "rustup " in static or "    container:" in static:
        errors.append("ci-static must remain cheap")
    markdown = jobs.get("markdownlint", "")
    if not re.search(r"uses: actions/setup-node@[0-9a-f]{40}\b", markdown):
        errors.append("Markdown requires pinned Node setup")
    npm_install = (
        "run: npm ci --ignore-scripts --no-audit --prefix scripts/ci/markdown\n"
    )
    if npm_install not in markdown:
        errors.append("Markdown requires locked script-free npm install")
    return errors


def mutate_job(text: str, name: str, old: str, new: str) -> str:
    """Replace one fragment within one job; reject stale mutation fixtures."""
    match = next(match for match in JOB.finditer(text) if match.group(1) == name)
    body = match.group(2)
    if old not in body:
        raise AssertionError(f"stale mutation: {name}/{old}")
    return text[: match.start(2)] + body.replace(old, new, 1) + text[match.end(2) :]


class ContributorGuard(unittest.TestCase):
    """Execute the actual trusted-base guard script with hermetic API responses."""

    def run_guard(self, **inputs: object) -> list[dict]:
        """Execute the workflow's actual script with supplied event/API fixtures."""
        workflow = (ROOT / ".github/workflows/pr-guard.yml").read_text()
        script = workflow.split("          script: |\n", 1)[1]
        script = "\n".join(line[12:] for line in script.splitlines())
        harness = r"""
const fs = require('node:fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const calls = [];
const context = {
  actor: input.actor || 'different-admin',
  repo: {owner: 'fixture', repo: 'repo'},
  payload: {pull_request: {
    number: 33, html_url: 'https://example.test/pr/33',
    author_association: input.association,
    user: input.user || {login: 'pr-author'},
    head: {repo: input.headRepo === undefined ? {id: 123} : input.headRepo},
    base: {repo: {id: 123}},
  }},
};
const github = {rest: {
  repos: {getCollaboratorPermissionLevel: async (args) => {
    calls.push({kind: 'lookup', ...args});
    if (input.error) throw {status: input.error};
    return {data: {permission: input.permission}};
  }},
  issues: {createComment: async () => calls.push({kind: 'comment'})},
  pulls: {update: async (args) => calls.push({kind: 'update', ...args})},
}};
const core = {setFailed: () => calls.push({kind: 'failed'})};
const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
new AsyncFunction('github', 'context', 'core', 'console', input.script)(
  github, context, core, {log: () => {}}
).then(() => process.stdout.write(JSON.stringify(calls)))
 .catch(() => process.exit(1));
"""
        result = subprocess.run(
            ["node", "-e", harness],
            input=json.dumps(dict(script=script, **inputs)),
            text=True,
            capture_output=True,
            check=True,
            timeout=15,
        )
        return json.loads(result.stdout)

    def test_dependabot_identity(self) -> None:
        """Only the authenticated same-repository bot gets the closure exception."""
        bot = {"login": "dependabot[bot]", "id": 49699333, "type": "Bot"}
        cases = [
            ({}, True),
            ({"user": {**bot, "id": 999}}, False),
            ({"user": {**bot, "id": "49699333"}}, False),
            ({"user": {**bot, "login": "impostor"}}, False),
            ({"user": {**bot, "type": "User"}}, False),
            ({"user": {"login": "impostor", "name": "dependabot[bot]"}}, False),
            ({"user": {"login": "impostor"}, "actor": "dependabot[bot]"}, False),
            ({"headRepo": {"id": 456}}, False),
            ({"headRepo": None}, False),
            ({"headRepo": {}}, False),
        ]
        for overrides, accepted in cases:
            with self.subTest(overrides=overrides):
                inputs = dict(user=bot, association="NONE", permission="none")
                inputs.update(overrides)
                calls = self.run_guard(**inputs)
                self.assertEqual(
                    [call["kind"] for call in calls],
                    [] if accepted else ["lookup", "comment", "update"],
                )
                if not accepted:
                    self.assertEqual(calls[-1]["state"], "closed")

    def test_author_permission_fallback(self) -> None:
        """Preserve trusted associations and deny unverified fallback identities."""
        cases = [
            (association, "none", None, True, False)
            for association in ("OWNER", "MEMBER", "COLLABORATOR")
        ] + [
            ("CONTRIBUTOR", role, None, True, True)
            for role in ("admin", "maintain", "write")
        ] + [
            (association, role, None, False, True)
            for association in ("CONTRIBUTOR", "NONE", "FIRST_TIME_CONTRIBUTOR")
            for role in ("read", "triage", "none", "unknown", None)
        ] + [
            ("CONTRIBUTOR", None, status, False, True)
            for status in (403, 404, 429, 500)
        ]
        for association, permission, error, trusted, lookup in cases:
            with self.subTest(association=association, role=permission, error=error):
                calls = self.run_guard(
                    association=association, permission=permission, error=error
                )
                lookups = [call for call in calls if call["kind"] == "lookup"]
                self.assertEqual(len(lookups), int(lookup))
                if lookups:
                    self.assertEqual(lookups[0]["username"], "pr-author")
                    self.assertEqual(lookups[0]["owner"], "fixture")
                    self.assertEqual(lookups[0]["repo"], "repo")
                updates = [call for call in calls if call["kind"] == "update"]
                self.assertEqual(len(updates), int(not trusted))
                if updates:
                    self.assertEqual(updates[0]["state"], "closed")
                    self.assertEqual(updates[0]["pull_number"], 33)
                failures = [call for call in calls if call["kind"] == "failed"]
                self.assertEqual(len(failures), int(error not in (None, 404)))


class WorkflowContracts(unittest.TestCase):
    """Check current workflows and prove weakened DAG/result wiring is caught."""

    def setUp(self) -> None:
        """Read current files, including any concurrent cache-only changes."""
        workflows = ROOT / ".github/workflows"
        self.pr = (workflows / "pr-ci.yml").read_text(encoding="utf-8")
        self.main = (workflows / "main-ci.yml").read_text(encoding="utf-8")
        self.security = (workflows / "security.yml").read_text(encoding="utf-8")

    def test_current_workflows(self) -> None:
        """Current required job identities, semantics and wiring must pass."""
        self.assertEqual(workflow_errors(self.pr, "pr"), [])
        self.assertEqual(workflow_errors(self.main, "main"), [])
        self.assertEqual(workflow_errors(self.security, "security"), [])

    def test_execution_jobs_require_real_sandbox_dependencies(self) -> None:
        """Reject missing namespaces, backend, or launcher in isolated job bootstrap."""
        for job in EXPENSIVE:
            for old, new in (
                ("--cap-add=NET_ADMIN ", ""),
                ("bubblewrap ", ""),
                ("cargo build --locked -p transist-sandbox-launcher", "true"),
                ("install -Dm755 target/debug/transist-sandbox-launcher", "true"),
            ):
                with self.subTest(job=job, removed=old):
                    self.assertTrue(
                        any(
                            "sandbox" in error or "bubblewrap" in error
                            for error in workflow_errors(
                                mutate_job(self.pr, job, old, new), "pr"
                            )
                        )
                    )

    def test_gate_dependency_and_binding_mutations(self) -> None:
        """Every required early, expensive and main result is indispensable."""
        for kind, text, gates in (
            ("pr", self.pr, {"preflight-gate": EARLY, "pr-gate": PR}),
            ("main", self.main, {"main-gate": MAIN}),
            ("security", self.security, {"security-gate": SECURITY}),
        ):
            for gate, bindings in gates.items():
                for job, variable in bindings.items():
                    for old, new, error in (
                        (f"      - {job}\n", "", "gate needs mismatch"),
                        (
                            f"          {variable}: ${{{{ needs.{job}.result }}}}\n",
                            "",
                            "missing result/flag binding",
                        ),
                    ):
                        with self.subTest(
                            kind=kind, gate=gate, job=job, mutation=error
                        ):
                            mutated = mutate_job(text, gate, old, new)
                            self.assertTrue(
                                any(
                                    error in item
                                    for item in workflow_errors(mutated, kind)
                                )
                            )
                with self.subTest(gate=gate, mutation="always"):
                    mutated = mutate_job(
                        text, gate, f"    if: {ALWAYS}", "    if: success()"
                    )
                    self.assertIn(
                        f"gate must always evaluate: {gate}",
                        workflow_errors(mutated, kind),
                    )

    def test_security_policy_mutations(self) -> None:
        """Reject visibility bypasses, missing flags and ignored gate failures."""
        for job, old, new in (
            ("codeql", f"    if: {PUBLIC_ONLY}\n", ""),
            ("codeql", PUBLIC_ONLY, ALWAYS),
            ("codeql", PUBLIC_ONLY, "${{ github.event.repository.private == true }}"),
            ("security-audit", "    steps:", f"    if: {PUBLIC_ONLY}\n    steps:"),
            (
                "security-gate",
                "          REPOSITORY_PRIVATE: ${{ github.event.repository.private }}\n",
                "",
            ),
            ("security-gate", "        run: |", "        if: success()\n        run: |"),
            (
                "security-gate", "        run: |",
                "        continue-on-error: true\n        run: |",
            ),
            *(
                (job, "    runs-on:", "    continue-on-error: true\n    runs-on:")
                for job in (*SECURITY, "security-gate")
            ),
        ):
            with self.subTest(job=job, old=old, new=new):
                self.assertTrue(
                    workflow_errors(mutate_job(self.security, job, old, new), "security")
                )

    def test_expensive_job_mutations(self) -> None:
        """Require explicit status handling, preflight success and non-draft."""
        for job in EXPENSIVE:
            for old, new in (
                ("    needs: [preflight-gate]\n", ""),
                ("!cancelled() && ", ""),
                (PREFLIGHT_READY, READY),
                (PREFLIGHT_READY, "${{ needs.preflight-gate.result == 'success' }}"),
                (PREFLIGHT_READY, "${{ always() }}"),
                ("    needs: [preflight-gate]", "    needs: [validate-full]"),
            ):
                with self.subTest(job=job, old=old, new=new):
                    self.assertTrue(
                        workflow_errors(mutate_job(self.pr, job, old, new), "pr")
                    )

    def test_duplicate_fast_job_and_cycle_mutations(self) -> None:
        """Reject resurrected fast validation, duplicate keys and DAG cycles."""
        duplicate_fast = self.pr + ("\n  validate-fast:\n    continue-on-error: true\n")
        self.assertIn(
            "duplicate nonrequired fast validation",
            workflow_errors(duplicate_fast, "pr"),
        )
        duplicate = self.pr + "\n  ci-static:\n    runs-on: ubuntu-24.04\n"
        self.assertIn("duplicate job identity", workflow_errors(duplicate, "pr"))
        cycle = mutate_job(
            self.pr,
            "ci-static",
            "    steps:",
            "    needs: [pr-gate]\n    steps:",
        )
        self.assertTrue(
            any("dependency cycle" in error for error in workflow_errors(cycle, "pr"))
        )

    def test_required_jobs_cannot_ignore_failure(self) -> None:
        """A green aggregate cannot mask a continue-on-error required check."""
        for job in set(PR) | {"pr-gate"}:
            with self.subTest(job=job):
                mutated = mutate_job(
                    self.pr,
                    job,
                    "    runs-on:",
                    "    continue-on-error: true\n    runs-on:",
                )
                self.assertIn(
                    f"required job may not continue-on-error: {job}",
                    workflow_errors(mutated, "pr"),
                )


class SecurityGateResults(unittest.TestCase):
    """Execute the actual inline security gate, without YAML or Actions emulation."""

    def test_visibility_and_results_matrix(self) -> None:
        """Only successful audit plus the exact visibility-specific outcome passes."""
        workflow = (ROOT / ".github/workflows/security.yml").read_text(encoding="utf-8")
        body = next(
            m.group(2) for m in JOB.finditer(workflow)
            if m.group(1) == "security-gate"
        )
        scripts = re.findall(
            r"^        run: \|\n((?:          [^\n]*\n|\n)+)", body, re.M
        )
        self.assertEqual(len(scripts), 1, "require exactly one inline gate script")
        script = "\n".join(line[10:] for line in scripts[0].splitlines())
        results = ("success", "skipped", "failure", "cancelled", "unknown", "", None)
        with tempfile.TemporaryDirectory(prefix="transist-security-contract-") as cwd:
            for private in ("true", "false", "TRUE", "unknown", "", None):
                for audit in results:
                    for codeql in results:
                        with self.subTest(private=private, audit=audit, codeql=codeql):
                            env = {"PATH": os.defpath}
                            env.update(
                                {
                                    key: value
                                    for key, value in {
                                        "AUDIT_RESULT": audit,
                                        "CODEQL_RESULT": codeql,
                                        "REPOSITORY_PRIVATE": private,
                                    }.items()
                                    if value is not None
                                }
                            )
                            outcome = subprocess.run(
                                ["bash", "-c", script], env=env, cwd=cwd,
                                text=True, capture_output=True, timeout=5, check=False,
                            )
                            passes = audit == "success" and (
                                (private == "true" and codeql == "skipped")
                                or (private == "false" and codeql == "success")
                            )
                            self.assertEqual(
                                outcome.returncode, 0 if passes else 1,
                                outcome.stdout + outcome.stderr,
                            )
                            if passes:
                                self.assertIn(f"CodeQL={codeql}", outcome.stdout)


class GateResults(unittest.TestCase):
    """Execute the real Bash evaluator with isolated environment/result cases."""

    def evaluate(
        self, mode: str, overrides: dict[str, str | None] | None = None
    ) -> subprocess.CompletedProcess[str]:
        """Run a gate in a temporary CWD; None removes an input environment key."""
        bindings = {"preflight": EARLY, "pr": PR, "main": MAIN}.get(mode, {})
        env = {
            "PATH": os.defpath,
            "IS_DRAFT": "false",
            "IS_DEPENDABOT": "false",
            "DEPENDENCY_REVIEW_ENABLED": "true",
        }
        env.update({variable: "success" for variable in bindings.values()})
        for key, value in (overrides or {}).items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        with tempfile.TemporaryDirectory(prefix="transist-ci-contract-") as cwd:
            return subprocess.run(
                ["bash", str(EVALUATOR), mode],
                env=env,
                cwd=cwd,
                text=True,
                capture_output=True,
                timeout=5,
                check=False,
            )

    def test_success_and_required_results(self) -> None:
        """Each result must succeed; missing/unknown outcomes also fail closed."""
        for mode, bindings in (("preflight", EARLY), ("pr", PR), ("main", MAIN)):
            self.assertEqual(self.evaluate(mode).returncode, 0)
            for variable in bindings.values():
                for result in ("failure", "skipped", "cancelled", "", None, "unknown"):
                    with self.subTest(mode=mode, variable=variable, result=result):
                        outcome = self.evaluate(mode, {variable: result})
                        self.assertEqual(
                            outcome.returncode, 1, outcome.stdout + outcome.stderr
                        )
                        self.assertIn("did not succeed", outcome.stderr)

    def test_pr_skip_exceptions(self) -> None:
        """Only the two exact skipped exceptions pass for ready PRs."""
        for mode in ("preflight", "pr"):
            for result in ("success", "skipped", "failure", "cancelled", "", None):
                for flag, variable in (
                    ("IS_DEPENDABOT", "COMMIT_SIGNOFF_RESULT"),
                    ("DEPENDENCY_REVIEW_ENABLED", "DEPENDENCY_REVIEW_RESULT"),
                ):
                    with self.subTest(mode=mode, flag=flag, result=result):
                        value = "true" if flag == "IS_DEPENDABOT" else "false"
                        outcome = self.evaluate(mode, {flag: value, variable: result})
                        self.assertEqual(
                            outcome.returncode == 0,
                            result in ("success", "skipped"),
                            outcome.stderr,
                        )
            outcome = self.evaluate(
                mode,
                {
                    "IS_DEPENDABOT": "true",
                    "COMMIT_SIGNOFF_RESULT": "skipped",
                    "DEPENDENCY_REVIEW_ENABLED": "",
                    "DEPENDENCY_REVIEW_RESULT": "skipped",
                },
            )
            self.assertEqual(outcome.returncode, 0, outcome.stderr)

    def test_accepted_ancestor_skips_keep_expensive_jobs_eligible(self) -> None:
        """Accepted skips pass preflight without an implicit success() blocker."""
        workflow = (ROOT / ".github/workflows/pr-ci.yml").read_text(encoding="utf-8")
        jobs = {match.group(1): match.group(2) for match in JOB.finditer(workflow)}
        for skipped_inputs in (
            {"IS_DEPENDABOT": "true", "COMMIT_SIGNOFF_RESULT": "skipped"},
            {
                "DEPENDENCY_REVIEW_ENABLED": "false",
                "DEPENDENCY_REVIEW_RESULT": "skipped",
            },
            {
                "IS_DEPENDABOT": "true",
                "COMMIT_SIGNOFF_RESULT": "skipped",
                "DEPENDENCY_REVIEW_ENABLED": "false",
                "DEPENDENCY_REVIEW_RESULT": "skipped",
            },
        ):
            with self.subTest(skipped_inputs=skipped_inputs):
                outcome = self.evaluate("preflight", skipped_inputs)
                self.assertEqual(outcome.returncode, 0, outcome.stderr)
                for job in EXPENSIVE:
                    with self.subTest(job=job):
                        self.assertEqual(job_field(jobs[job], "if"), PREFLIGHT_READY)
                        mutated = mutate_job(workflow, job, "!cancelled() && ", "")
                        self.assertIn(
                            f"expensive job must explicitly check cancellation: {job}",
                            workflow_errors(mutated, "pr"),
                        )

    def test_drafts_skip_pr_validation_only(self) -> None:
        """Draft PRs pass with all skipped/missing checks; main never bypasses."""
        for mode, bindings in (("preflight", EARLY), ("pr", PR)):
            for result in ("skipped", None):
                inputs = {variable: result for variable in bindings.values()}
                inputs["IS_DRAFT"] = "true"
                self.assertEqual(self.evaluate(mode, inputs).returncode, 0)
            self.assertEqual(
                self.evaluate(
                    mode,
                    {
                        "IS_DRAFT": None,
                        "CI_STATIC_RESULT": "failure",
                    },
                ).returncode,
                1,
            )
        self.assertEqual(
            self.evaluate(
                "main",
                {
                    "IS_DRAFT": "true",
                    "COVERAGE_RESULT": "skipped",
                },
            ).returncode,
            1,
        )

    def test_preflight_does_not_require_expensive_results(self) -> None:
        """Preflight runs before expensive jobs and ignores their unset results."""
        outcome = self.evaluate(
            "preflight", {variable: "failure" for variable in EXPENSIVE.values()}
        )
        self.assertEqual(outcome.returncode, 0, outcome.stderr)

    def test_invalid_mode_fails_before_draft_exception(self) -> None:
        """An invalid invocation must not silently turn into a passing PR gate."""
        self.assertEqual(self.evaluate("invalid", {"IS_DRAFT": "true"}).returncode, 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
