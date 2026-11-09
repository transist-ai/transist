#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Hermetic runner tests with stubs/local Git fixtures; no Docker, build, or network."""

from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import ci_local as ci


def workflows() -> list[dict]:
    """Return parsed workflow fixtures with digest pins and real security options."""
    debian = "rust:1.88.0-bookworm@sha256:" + "a" * 64
    alpine = "rust:1.88.0-alpine@sha256:" + "b" * 64
    options = " ".join(ci.SECURITY_OPTIONS)
    return [
        {
            "file": "main-ci.yml",
            "document": {
                "jobs": {
                    "platform-smoke": {
                        "container": {
                            "image": "${{ matrix.container_image }}",
                            "options": "${{ matrix.container_options }}",
                        },
                        "strategy": {
                            "matrix": {
                                "include": [
                                    {
                                        "platform": "Linux",
                                        "container_image": debian,
                                        "container_options": options,
                                    },
                                    {
                                        "platform": "macOS",
                                        "container_image": "",
                                        "container_options": "",
                                    },
                                ]
                            }
                        },
                    },
                    "musl-smoke": {"container": {"image": alpine, "options": options}},
                    "coverage": {"container": {"image": debian, "options": options}},
                }
            },
        },
        {
            "file": "pr-ci.yml",
            "document": {
                "jobs": {
                    "validate-full": {
                        "container": {"image": debian, "options": options}
                    },
                }
            },
        },
    ]


class Runner(unittest.TestCase):
    """Test argument/ref selection, stage barriers, and truthful summaries."""

    def test_linux_probes_match_runtime_namespace_contract(self) -> None:
        """Require user/network isolation without unrelated nested PID proc mounts."""
        with patch.object(ci.sys, "platform", "linux"), patch.object(
            ci, "require_tools"
        ), patch.object(ci, "execute") as run:
            ci.isolation_probe(ci.ROOT)
        argv = run.call_args.args[0]
        self.assertIn("--unshare-user", argv)
        self.assertIn("--unshare-net", argv)
        self.assertNotIn("--unshare-all", argv)
        for source in (
            ci.CONTAINER_ENTRY,
            (ci.ROOT / "scripts/ci/run_platform_smoke.sh").read_text(),
        ):
            self.assertIn(
                "bwrap --die-with-parent --unshare-user --unshare-net", source
            )
            self.assertNotIn("bwrap --die-with-parent --unshare-all", source)

    def test_wrapper_exposes_tool_root_without_rustup_auto_install(self) -> None:
        """The public shell wrapper exports isolated tool paths and the pinned toolchain."""
        with tempfile.TemporaryDirectory() as directory:
            tool = Path(directory)
            (tool / "bin").mkdir()
            python = tool / "bin/python3"
            python.write_text(
                f"#!{sys.executable}\n"
                + "import json, os\nprint(json.dumps({key: os.environ[key] for key in "
                "('CI_TOOL_ROOT', 'PATH', 'RUSTUP_AUTO_INSTALL', 'RUSTUP_TOOLCHAIN')}))\n",
                encoding="utf-8",
            )
            python.chmod(0o755)
            result = subprocess.run(
                ["bash", str(ci.ROOT / "scripts/ci/check_ci_local.sh"), "--help"],
                env=dict(os.environ, CI_TOOL_ROOT=str(tool)),
                text=True,
                capture_output=True,
                timeout=10,
                check=True,
            )
            exported = json.loads(result.stdout)
            self.assertEqual(exported["CI_TOOL_ROOT"], str(tool))
            self.assertTrue(exported["PATH"].startswith(str(tool / "bin") + os.pathsep))
            self.assertEqual(exported["RUSTUP_AUTO_INSTALL"], "0")
            self.assertEqual(exported["RUSTUP_TOOLCHAIN"], "1.88.0")

    def test_profile_selection(self) -> None:
        """Only the selected profile runs, and base is mandatory only for PR/Debian."""
        for args in (
            ["--profile", "pr"],
            ["--profile", "debian"],
            ["--profile", "alpine", "--base", "main"],
            ["--profile", "unknown"],
            ["--profile", "pr", "--base", "main", "--probe-only"],
        ):
            with self.subTest(args=args), self.assertRaises(SystemExit):
                ci.main(args)
        with patch.object(ci, "run_native") as native, patch.object(
            ci, "run_docker"
        ) as docker:
            with redirect_stdout(io.StringIO()):
                self.assertEqual(ci.main(["--profile", "pr", "--base", "main"]), 0)
                native.assert_called_once()
                docker.assert_not_called()
                self.assertEqual(ci.main(["--profile", "alpine"]), 0)
                self.assertEqual(ci.main(["--profile", "debian", "--base", "main"]), 0)
            self.assertEqual(
                [call.args[1] for call in docker.call_args_list], ["alpine", "debian"]
            )

    def test_probe_only_selects_docker_without_requiring_unused_base(self) -> None:
        """Docker probes accept a missing base and never dispatch the native suite."""
        with patch.object(ci, "run_native") as native, patch.object(
            ci, "run_docker"
        ) as docker, redirect_stdout(io.StringIO()):
            for profile in ("debian", "alpine"):
                self.assertEqual(
                    ci.main(["--profile", profile, "--probe-only", "--bootstrap"]), 0
                )
                self.assertTrue(docker.call_args.kwargs["probe_only"])
                self.assertTrue(docker.call_args.args[3])
            native.assert_not_called()

    def test_node_prerequisite_rejects_old_or_ambiguous_versions(self) -> None:
        """Locked npm tooling requires Node 22+ without an implicit install fallback."""
        for version in (
            "v18.20.4",
            "v20.19.0",
            "v21.7.0",
            "garbled",
            "v22.1.0\nv18.0.0",
        ):
            with self.subTest(version=version), patch.object(
                ci, "require_tools"
            ), patch.object(ci, "execute", return_value=version) as run:
                with self.assertRaisesRegex(ValueError, "Node >=22"):
                    ci.node_prerequisite(ci.ROOT)
                self.assertEqual(run.call_args.args[0], ["node", "--version"])
        for version in ("v22.15.0", "v24.0.0"):
            with patch.object(ci, "require_tools"), patch.object(
                ci, "execute", return_value=version
            ):
                ci.node_prerequisite(ci.ROOT)

    def test_native_prerequisites_require_pinned_toolchain_components_without_installs(
        self,
    ) -> None:
        """Missing clippy/LLVM components fail before any install, Cargo check, or gate execution."""
        for missing in ("clippy", "llvm-tools"):
            installed = "\n".join(
                f"{component}-x86_64-unknown-linux-gnu"
                for component in ("rustfmt", "clippy", "llvm-tools")
                if component != missing
            )
            with self.subTest(missing=missing), patch.object(
                ci, "require_tools"
            ), patch.object(ci, "node_prerequisite"), patch.object(
                ci, "execute", side_effect=["rustc 1.88.0", installed]
            ) as run:
                with self.assertRaisesRegex(
                    ValueError, f"Missing Rust component {missing}"
                ):
                    ci.native_prerequisites(ci.ROOT)
                self.assertEqual(
                    run.call_args_list[0].args[0],
                    ["rustup", "run", "1.88.0", "rustc", "--version"],
                )
                self.assertEqual(
                    run.call_args_list[1].args[0],
                    [
                        "rustup",
                        "component",
                        "list",
                        "--installed",
                        "--toolchain",
                        "1.88.0",
                    ],
                )

    def test_order_and_no_implicit_install(self) -> None:
        """Run the complete native plan serially; coverage follows the unchanged full gate."""
        calls: list[list[str]] = []
        with patch.object(ci, "resolve_refs"), patch.object(
            ci, "native_prerequisites"
        ), patch.object(ci, "isolation_probe"), patch.object(
            ci, "bootstrap"
        ) as install, patch.object(
            ci, "coverage"
        ) as coverage, patch.object(
            ci, "execute", side_effect=lambda args, **kw: calls.append(args)
        ):
            stages = ci.Stages()
            with redirect_stdout(io.StringIO()):
                ci.run_native(ci.ROOT, "main", stages)
            install.assert_not_called()
            coverage.assert_called_once()
            self.assertEqual(
                [row[0] for row in stages.results],
                [
                    "base refs",
                    "prerequisites",
                    "static",
                    "DCO",
                    "markdown",
                    "cargo deny",
                    "isolation",
                    "schema",
                    "full PR readiness",
                    "coverage",
                ],
            )
            self.assertEqual(calls[-1], ["bash", "scripts/ci/check_pr_ready.sh"])
            self.assertEqual(calls[1][-2:], ["main", "HEAD"])

    def test_cheap_failure_prevents_compile_and_summary_records_failure(self) -> None:
        """Every cheap check is a barrier: failure prevents all subsequent expensive work."""
        for failed in (
            "check_ci_static.sh",
            "check_commit_signoffs.sh",
            "check_markdown.sh",
            "deny",
        ):
            calls: list[list[str]] = []

            def command(args: list[str], **kwargs: object) -> str:
                """Record argv and simulate a selected preflight failure."""
                calls.append(args)
                if any(failed in arg for arg in args):
                    raise subprocess.CalledProcessError(7, args)
                return ""

            with self.subTest(failed=failed), patch.object(
                ci, "resolve_refs"
            ), patch.object(ci, "native_prerequisites"), patch.object(
                ci, "execute", side_effect=command
            ), patch.object(
                ci, "coverage"
            ) as coverage:
                stages = ci.Stages()
                output = io.StringIO()
                with redirect_stdout(output):
                    with self.assertRaises(subprocess.CalledProcessError):
                        ci.run_native(ci.ROOT, "main", stages)
                    stages.summary()
                self.assertIn("FAIL", output.getvalue())
                self.assertIn("TOTAL", output.getvalue())
                self.assertFalse(any("schema" in " ".join(args) for args in calls))
                self.assertFalse(
                    any("check_pr_ready" in " ".join(args) for args in calls)
                )
                coverage.assert_not_called()

    def test_ref_failure_prevents_bootstrap(self) -> None:
        """An unavailable base does not trigger downloads even with explicit bootstrap."""
        with patch.object(
            ci, "resolve_refs", side_effect=ValueError("missing ref")
        ), patch.object(ci, "bootstrap") as install, redirect_stdout(io.StringIO()):
            with self.assertRaises(ValueError):
                ci.run_native(ci.ROOT, "missing", ci.Stages(), install=True)
            install.assert_not_called()

    def test_native_isolation_failure_prevents_compile(self) -> None:
        """A backend error after cheap checks prevents schema/full/coverage compilation."""
        with patch.object(ci, "resolve_refs"), patch.object(
            ci, "native_prerequisites"
        ), patch.object(
            ci, "isolation_probe", side_effect=ValueError("no isolation")
        ), patch.object(
            ci, "execute"
        ) as run, patch.object(
            ci, "coverage"
        ) as coverage, redirect_stdout(
            io.StringIO()
        ):
            stages = ci.Stages()
            with self.assertRaises(ValueError):
                ci.run_native(ci.ROOT, "main", stages)
            self.assertEqual(stages.results[-1][:2], ("isolation", "FAIL"))
            self.assertFalse(
                any("schema" in " ".join(call.args[0]) for call in run.call_args_list)
            )
            coverage.assert_not_called()

    def test_refs_fail_closed_without_fetch(self) -> None:
        """Ref-resolution and history-query errors propagate and never invoke git fetch."""
        for failing in (
            "HEAD^{commit}",
            "main^{commit}",
            "--is-shallow-repository",
            "merge-base",
        ):

            def command(args: list[str], **kwargs: object) -> str:
                """Return commit IDs until the selected Git query fails."""
                if failing in args:
                    raise subprocess.CalledProcessError(1, args)
                return "false" if "--is-shallow-repository" in args else "a" * 40

            with self.subTest(query=failing), patch.object(
                ci, "execute", side_effect=command
            ) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    ci.resolve_refs(ci.ROOT, "main")
                self.assertFalse(
                    any("fetch" in call.args[0] for call in run.call_args_list)
                )
        with self.assertRaises(ValueError):
            ci.resolve_refs(ci.ROOT, None)

    def test_coverage_isolates_reports_and_instrumented_target(self) -> None:
        """Reports are unique, LLVM compilation is reusable, and the launcher is explicit."""
        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "execute", return_value="rustc 1.88.0\nhost: x86_64-unknown-linux-gnu"
        ) as run, patch.dict(os.environ, {"CARGO_TARGET_DIR": "custom-target"}):
            root = Path(directory)
            old = root / "coverage/summary.txt"
            old.parent.mkdir()
            old.write_text("previous report", encoding="utf-8")
            with redirect_stdout(io.StringIO()):
                ci.coverage(root)
                ci.coverage(root)
            first, second = [
                call
                for call in run.call_args_list
                if "run_coverage.sh" in " ".join(call.args[0])
            ]
            self.assertNotEqual(first.args[0][-1], second.args[0][-1])
            self.assertEqual(
                first.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"],
                second.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"],
            )
            self.assertIn(
                "pr-default-x86_64-unknown-linux-gnu",
                first.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"],
            )
            self.assertEqual(
                first.kwargs["env"]["TRANSIST_SANDBOX_LAUNCHER"],
                str(root / "custom-target/debug/transist-sandbox-launcher"),
            )
            self.assertEqual(old.read_text(), "previous report")

    def test_coverage_separates_abi_and_compiler_flags(self) -> None:
        """An ABI or instrumentation setting change invalidates only its LLVM cache."""
        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "execute", return_value="host: x86_64-unknown-linux-gnu"
        ) as run, redirect_stdout(io.StringIO()):
            root = Path(directory)
            with patch.dict(
                os.environ, {"CARGO_TARGET_DIR": "target", "CARGO_BUILD_TARGET": ""}
            ):
                ci.coverage(root)
                first = run.call_args.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"]
                with patch.dict(os.environ, {"RUSTFLAGS": "--cfg coverage_test"}):
                    ci.coverage(root)
                    self.assertNotEqual(
                        first, run.call_args.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"]
                    )
                with patch.dict(
                    os.environ, {"CARGO_BUILD_TARGET": "x86_64-unknown-linux-musl"}
                ):
                    ci.coverage(root)
                    self.assertNotEqual(
                        first, run.call_args.kwargs["env"]["CARGO_LLVM_COV_TARGET_DIR"]
                    )
                    self.assertEqual(
                        run.call_args.kwargs["env"]["TRANSIST_SANDBOX_LAUNCHER"],
                        str(
                            root
                            / "target/x86_64-unknown-linux-musl/debug/transist-sandbox-launcher"
                        ),
                    )


class DockerPlan(unittest.TestCase):
    """Validate digest selection, Docker authority, and dirty snapshot safety without Docker."""

    def test_snapshot_preserves_cargo_build_config_not_credentials_or_caches(
        self,
    ) -> None:
        """Cargo configuration must match hosted builds without exposing Cargo secrets."""
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            destination = Path(directory) / "copy"
            cargo = source / ".cargo"
            cargo.mkdir(parents=True)
            destination.mkdir()
            config = "[build]\nincremental = true\n"
            (cargo / "config.toml").write_text(config)
            (cargo / "credentials.toml").write_text("sensitive fixture")
            (cargo / "bin").mkdir()
            (cargo / "bin/cargo").write_text("not a compiler")
            ci.copy_checkout(source, destination)
            self.assertEqual((destination / ".cargo/config.toml").read_text(), config)
            self.assertEqual(
                {path.name for path in (destination / ".cargo").iterdir()},
                {"config.toml"},
            )

    def test_exact_workflow_digests_and_disagreements(self) -> None:
        """Both profiles use workflow bytes, never fallback pins or partial/first matches."""
        docs = workflows()
        self.assertTrue(ci.select_image(docs, "debian").endswith("a" * 64))
        self.assertTrue(ci.select_image(docs, "alpine").endswith("b" * 64))
        docs[1]["document"]["jobs"]["validate-full"]["container"]["image"] = (
            "rust:1.88.0-bookworm@sha256:" + "c" * 64
        )
        for profile in ("debian", "alpine"):
            with self.subTest(profile=profile), self.assertRaises(ValueError):
                ci.select_image(docs, profile)
        docs = workflows()
        docs[0]["document"]["jobs"]["musl-smoke"]["container"][
            "image"
        ] = "rust:1.88.0-alpine"
        with self.assertRaises(ValueError):
            ci.select_image(docs, "alpine")

    def test_yaml_parser_must_be_locked(self) -> None:
        """No regex/ambient parser fallback is possible when the pinned parser is absent."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "scripts/ci/redocly"
            tool.mkdir(parents=True)
            (tool / "package-lock.json").write_text(
                '{"packages": {}}', encoding="utf-8"
            )
            with patch.object(ci, "execute") as run, self.assertRaises(ValueError):
                ci.load_workflows(root)
            run.assert_not_called()

    def test_yaml_parser_pin_matches_parent_tool_lock(self) -> None:
        """The local runner tracks the parent's exact locked parser version without resolving tools."""
        lock = json.loads(
            (ci.ROOT / "scripts/ci/redocly/package-lock.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(ci.YAML_VERSION, "5.5.0")
        self.assertEqual(
            lock["packages"]["node_modules/js-yaml"]["version"], ci.YAML_VERSION
        )
        self.assertTrue(lock["packages"]["node_modules/js-yaml"]["integrity"])

    def test_exact_docker_args_mounts_and_cache_separation(self) -> None:
        """Use amd64 and workflow caps, RO host inputs, named ABI-specific caches, no secrets."""
        commands = []
        for profile in ("debian", "alpine"):
            image = ci.select_image(workflows(), profile)
            snapshot = Path("/filtered snapshot")
            scope = ci.cache_scope(
                Path("/repository"),
                Path("/nonexistent-inputs"),
                profile,
                image,
                identity="fixture",
            )
            command = ci.docker_command(
                snapshot,
                Path("/history"),
                profile,
                image,
                "base",
                "head",
                False,
                scope=scope,
            )
            commands.append(command)
            self.assertEqual(command[command.index("--platform") + 1], "linux/amd64")
            self.assertIn("--pull=never", command)
            self.assertIn(image, command)
            self.assertIn(
                "type=bind,source=/filtered snapshot,target=/checkout,readonly", command
            )
            self.assertIn("type=bind,source=/history,target=/history,readonly", command)
            self.assertNotIn("--privileged", command)
            self.assertNotIn("/var/run/docker.sock", " ".join(command))
            self.assertEqual(command[-4:], ["0", "base", "head", "0"])
            self.assertFalse(
                any(arg.endswith("target=/usr/local/cargo") for arg in command)
            )
            self.assertTrue(
                any(arg.endswith("target=/usr/local/cargo/registry") for arg in command)
            )
            self.assertTrue(
                any(arg.endswith("target=/usr/local/cargo/git") for arg in command)
            )
            self.assertFalse(
                any(arg.endswith("target=/usr/local/cargo/bin") for arg in command)
            )
            for option in ci.SECURITY_OPTIONS:
                self.assertIn(option, command)
        caches = [
            {arg for arg in command if arg.startswith("type=volume")}
            for command in commands
        ]
        self.assertFalse(caches[0] & caches[1])
        self.assertEqual(len(caches[0]), 4)

    def test_all_cache_names_bind_repository_image_and_installer_bytes(self) -> None:
        """Registry/git/tools/target caches all rotate for a different trust/tooling scope."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, snapshot = root / "repository", root / "snapshot"
            snapshot.mkdir()
            tools = snapshot / "scripts/ci"
            tools.mkdir(parents=True)
            installer = tools / "install_ci_tools.sh"
            installer.write_text("version one", encoding="utf-8")
            version = tools / "tool-versions.env"
            version.write_text("NODE_VERSION=22.15.0", encoding="utf-8")
            image = ci.select_image(workflows(), "debian")
            baseline = ci.cache_scope(
                source, snapshot, "debian", image, identity="repository"
            )
            self.assertEqual(
                baseline,
                ci.cache_scope(
                    source, snapshot, "debian", image, identity="repository"
                ),
            )
            second_snapshot = root / "second-snapshot"
            shutil.copytree(snapshot, second_snapshot)
            self.assertEqual(
                baseline,
                ci.cache_scope(
                    source, second_snapshot, "debian", image, identity="repository"
                ),
            )
            scopes = [
                ci.cache_scope(
                    root / "other-repository",
                    snapshot,
                    "debian",
                    image,
                    identity="repository",
                ),
                ci.cache_scope(
                    source, snapshot, "debian", image + "changed", identity="repository"
                ),
                ci.cache_scope(
                    source, snapshot, "debian", image, identity="different-lineage"
                ),
            ]
            installer.write_text("version two", encoding="utf-8")
            scopes.append(
                ci.cache_scope(source, snapshot, "debian", image, identity="repository")
            )
            version.write_text("NODE_VERSION=22.16.0", encoding="utf-8")
            scopes.append(
                ci.cache_scope(source, snapshot, "debian", image, identity="repository")
            )
            (tools / "markdown").mkdir()
            (tools / "markdown/package-lock.json").write_text(
                '{"lockfileVersion": 3}', encoding="utf-8"
            )
            scopes.append(
                ci.cache_scope(source, snapshot, "debian", image, identity="repository")
            )
            baseline_command = ci.docker_command(
                snapshot,
                root / "history",
                "debian",
                image,
                "base",
                "head",
                False,
                scope=baseline,
            )
            baseline_volumes = {
                arg for arg in baseline_command if arg.startswith("type=volume")
            }
            for scope in scopes:
                command = ci.docker_command(
                    snapshot,
                    root / "history",
                    "debian",
                    image,
                    "base",
                    "head",
                    False,
                    scope=scope,
                )
                self.assertFalse(
                    baseline_volumes
                    & {arg for arg in command if arg.startswith("type=volume")}
                )

    def test_repository_identity_binds_lineage_and_origin_without_exposing_credentials(
        self,
    ) -> None:
        """Replacing a repository at the same checkout path cannot reuse another origin's caches."""
        identities = []
        for lineage, origin in (
            ("root-a", "https://credential@host/repo-a"),
            ("root-a", "https://credential@host/repo-b"),
            ("root-b", "https://credential@host/repo-a"),
        ):
            with patch.object(ci, "execute", side_effect=[lineage, origin]):
                identity = ci.repository_identity(Path("/same-checkout"))
                self.assertNotIn("credential", identity)
                identities.append(identity)
        self.assertEqual(len(set(identities)), 3)

    def test_bootstrap_yaml_tools_are_outside_every_mount(self) -> None:
        """Prepare parser tools on the host only after filtering; mount neither old nor new modules."""

        def command(args: list[str], **kwargs: object) -> str:
            """Simulate npm/Docker while inspecting their live temporary directories."""
            if args[:2] == ["npm", "ci"]:
                prefix = Path(args[-1])
                (prefix / "node_modules").mkdir()
                (prefix / "node_modules/host-tool").write_text("tool", encoding="utf-8")
                self.assertNotEqual(prefix, source / "scripts/ci/redocly")
            if args[:2] == ["docker", "run"]:
                self.assertEqual(args[-1], "1")
                mounts = [arg for arg in args if arg.startswith("type=bind,")]
                self.assertEqual(len(mounts), 2)
                snapshot = Path(
                    mounts[0].split("source=", 1)[1].split(",target=", 1)[0]
                )
                self.assertFalse(list(snapshot.rglob("node_modules")))
                self.assertNotIn("host-yaml-tools", " ".join(mounts))
            return ""

        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "require_tools"
        ), patch.object(
            ci, "resolve_refs", return_value=("base", "head")
        ), patch.object(
            ci, "node_prerequisite"
        ), patch.object(
            ci, "load_workflows", return_value=workflows()
        ), patch.object(
            ci, "execute", side_effect=command
        ), redirect_stdout(
            io.StringIO()
        ):
            source = Path(directory)
            tools = source / "scripts/ci/redocly"
            tools.mkdir(parents=True)
            for name in ("package.json", "package-lock.json"):
                (tools / name).write_text("{}", encoding="utf-8")
            ci.run_docker(source, "debian", "main", True, ci.Stages(), probe_only=True)

    def test_unknown_images_expressions_and_options_fail_closed(self) -> None:
        """Unresolved YAML values and weakened main container authority cannot be ignored."""
        for key, value in (
            ("image", "rust:other@sha256:" + "a" * 64),
            ("image", "${{ inputs.image }}"),
            ("options", "--privileged"),
        ):
            docs = workflows()
            docs[0]["document"]["jobs"]["musl-smoke"]["container"][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                ci.select_image(docs, "debian")

    def test_docker_operation_requires_local_refs_and_has_no_implicit_installs(
        self,
    ) -> None:
        """The real orchestration emits a full-history bundle and pinned Docker command only."""
        calls: list[list[str]] = []

        def command(args: list[str], **kwargs: object) -> str:
            """Record operations, returning fake resolved IDs without invoking any program."""
            calls.append(args)
            if args[:2] == ["docker", "run"]:
                mounts = [arg for arg in args if arg.startswith("type=bind,")]
                self.assertEqual(len(mounts), 2)
                snapshot = Path(
                    mounts[0].split("source=", 1)[1].split(",target=", 1)[0]
                )
                self.assertNotEqual(snapshot, source)
                self.assertTrue(
                    snapshot.is_relative_to(source.resolve() / ".tmp/ci-inputs")
                )
                self.assertEqual((snapshot / "dirty.rs").read_text(), "dirty source")
                self.assertEqual(
                    {path.name for path in snapshot.iterdir()}, {"dirty.rs"}
                )
                self.assertNotIn(f"source={source},", " ".join(mounts))
            return ""

        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "require_tools"
        ), patch.object(
            ci, "resolve_refs", return_value=("base-sha", "head-sha")
        ), patch.object(
            ci, "load_workflows", return_value=workflows()
        ), patch.object(
            ci, "execute", side_effect=command
        ), redirect_stdout(
            io.StringIO()
        ):
            source = Path(directory)
            (source / "dirty.rs").write_text("dirty source", encoding="utf-8")
            for name in (".env", ".env.local", ".npmrc", ".gitconfig", "local.key"):
                (source / name).write_text("private", encoding="utf-8")
            for name in (".git", ".transist", "secrets", "node_modules", "target"):
                (source / name).mkdir()
                (source / name / "private").write_text("private", encoding="utf-8")
            ci.run_docker(source, "debian", "main", False, ci.Stages())
        self.assertTrue(
            any(
                args[:3] == ["git", "bundle", "create"]
                and args[-2:] == ["--all", "HEAD"]
                for args in calls
            )
        )
        docker = next(args for args in calls if args[:2] == ["docker", "run"])
        self.assertEqual(docker[-4:], ["0", "base-sha", "head-sha", "0"])
        self.assertFalse(any(args[0] in ("npm", "rustup", "cargo") for args in calls))
        self.assertFalse(any(args[:2] == ["docker", "pull"] for args in calls))

    def test_docker_inputs_stay_in_checkout_regardless_of_tmpdir_and_are_cleaned(
        self,
    ) -> None:
        """All temporary bind sources stay in the shared checkout, not caller/system temp paths."""
        sources: list[Path] = []

        def command(args: list[str], **kwargs: object) -> str:
            """Inspect both live input mounts without invoking Docker or other programs."""
            if args[:2] == ["docker", "run"]:
                for mount in args:
                    if mount.startswith("type=bind,"):
                        path = Path(
                            mount.split("source=", 1)[1].split(",target=", 1)[0]
                        )
                        self.assertTrue(
                            path.is_relative_to(root.resolve() / ".tmp/ci-inputs")
                        )
                        self.assertTrue(path.is_dir())
                        sources.append(path)
            return ""

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unshared = root / "unshared-temp"
            with patch.dict(os.environ, {"TMPDIR": str(unshared)}), patch.object(
                ci, "require_tools"
            ), patch.object(
                ci, "resolve_refs", return_value=("base", "head")
            ), patch.object(
                ci, "load_workflows", return_value=workflows()
            ), patch.object(
                ci, "execute", side_effect=command
            ), redirect_stdout(
                io.StringIO()
            ):
                ci.run_docker(root, "debian", "main", False, ci.Stages())
            self.assertEqual(len(sources), 2)
            self.assertFalse(any(path.exists() for path in sources))
            self.assertEqual(list((root / ".tmp/ci-inputs").iterdir()), [])
            self.assertFalse(unshared.exists())

    def test_docker_input_parent_rejects_symlinks_before_outside_writes(self) -> None:
        """Both .tmp and ci-inputs must be actual directories inside the fixture checkout."""
        for name in (".tmp", ".tmp/ci-inputs"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                parent = Path(directory)
                root, outside = parent / "checkout", parent / "outside"
                root.mkdir()
                outside.mkdir()
                link = root / name
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to(outside)
                with self.assertRaisesRegex(ValueError, "must not be a symlink"):
                    ci.docker_inputs_parent(root)
                self.assertEqual(list(outside.iterdir()), [])

    def test_docker_bind_failure_has_shared_checkout_hint_and_no_repair(self) -> None:
        """Report startup failure without retrying, reconfiguring Docker, or deleting images."""
        args = ["docker", "run", "fixture"]
        error = subprocess.CalledProcessError(125, args)
        diagnostic = io.StringIO()
        with patch.object(ci, "execute", side_effect=error) as run, redirect_stderr(
            diagnostic
        ):
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                ci.run_container(
                    args, Path("/checkout"), Path("/checkout/.tmp/ci-inputs")
                )
            self.assertIs(raised.exception, error)
            run.assert_called_once_with(args, root=Path("/checkout"))
        self.assertIn("Docker/Colima", diagnostic.getvalue())
        self.assertIn("/checkout/.tmp/ci-inputs", diagnostic.getvalue())

    def test_escaping_symlink_fails_before_any_bootstrap(self) -> None:
        """Reject escaping source links on the host before npm/install/image operations."""
        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "require_tools"
        ), patch.object(
            ci, "resolve_refs", return_value=("base", "head")
        ), patch.object(
            ci, "execute"
        ) as run, redirect_stdout(
            io.StringIO()
        ):
            source = Path(directory) / "source"
            source.mkdir()
            (source / "escape").symlink_to(Path(directory))
            with self.assertRaisesRegex(ValueError, "escapes checkout"):
                ci.run_docker(source, "debian", "main", True, ci.Stages())
            run.assert_not_called()

    def test_staged_sensitive_paths_fail_before_bootstrap_or_history_bundle(
        self,
    ) -> None:
        """Do not leak filtered staged secrets through the otherwise-sanitized index patch."""
        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "require_tools"
        ), patch.object(
            ci, "resolve_refs", return_value=("base", "head")
        ), patch.object(
            ci, "execute", return_value=".env\0"
        ) as run, redirect_stdout(
            io.StringIO()
        ):
            with self.assertRaisesRegex(ValueError, "staged sensitive/generated"):
                ci.run_docker(Path(directory), "debian", "main", True, ci.Stages())
            self.assertEqual(len(run.call_args_list), 1)
            self.assertEqual(run.call_args.args[0][:2], ["git", "diff"])

    def test_absolute_internal_link_is_rejected_before_bootstrap(self) -> None:
        """An absolute host-internal link would escape after relocating to /checkout."""
        with tempfile.TemporaryDirectory() as directory, patch.object(
            ci, "require_tools"
        ), patch.object(
            ci, "resolve_refs", return_value=("base", "head")
        ), patch.object(
            ci, "execute"
        ) as run, redirect_stdout(
            io.StringIO()
        ):
            source = Path(directory)
            (source / "file").write_text("source", encoding="utf-8")
            (source / "absolute").symlink_to(source / "file")
            with self.assertRaisesRegex(ValueError, "absolute symlink"):
                ci.run_docker(source, "debian", "main", True, ci.Stages())
            run.assert_not_called()

    def test_dirty_snapshot_excludes_runtime_and_never_follows_links(self) -> None:
        """Preserve dirty/untracked bytes and internal links, omit caches and external links."""
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            source, destination = parent / "source", parent / "destination"
            source.mkdir()
            destination.mkdir()
            (source / "dirty.rs").write_text("uncommitted", encoding="utf-8")
            (source / "untracked").write_text("new", encoding="utf-8")
            (source / "internal").symlink_to("dirty.rs")
            for name in (
                "target",
                ".git",
                ".transist",
                "dist",
                ".tmp-sandbox-tests",
                "node_modules",
            ):
                (source / name).mkdir()
                (source / name / "excluded").write_text("discard", encoding="utf-8")
            ci.copy_checkout(source, destination)
            self.assertEqual((destination / "dirty.rs").read_text(), "uncommitted")
            self.assertEqual((destination / "untracked").read_text(), "new")
            self.assertTrue((destination / "internal").is_symlink())
            self.assertEqual(
                {p.name for p in destination.iterdir()},
                {"dirty.rs", "untracked", "internal"},
            )
            (source / "escape").symlink_to(parent)
            with self.assertRaisesRegex(ValueError, "escapes checkout"):
                ci.copy_checkout(source, destination, copy=False)

    def test_snapshot_never_traverses_directory_links_or_excluded_cache_links(
        self,
    ) -> None:
        """Internal directory links remain links; ignored cache symlinks never reach host binaries."""
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            source, destination = parent / "source", parent / "copy"
            source.mkdir()
            destination.mkdir()
            (source / "directory").mkdir()
            (source / "directory/file").write_text("source", encoding="utf-8")
            (source / "directory-link").symlink_to("directory")
            (source / "self").symlink_to(".")
            (source / "node_modules").symlink_to(parent)
            (source / ".transist").mkdir()
            (source / "runtime-link").symlink_to(".transist")
            ci.copy_checkout(source, destination)
            self.assertTrue((destination / "directory-link").is_symlink())
            self.assertTrue((destination / "self").is_symlink())
            self.assertFalse((destination / "node_modules").exists())
            self.assertFalse((destination / "runtime-link").is_symlink())
            self.assertEqual(
                {path.name for path in destination.iterdir()},
                {"directory", "directory-link", "self"},
            )

    def test_history_snapshot_retains_index_dirty_bytes_and_staged_additions(
        self,
    ) -> None:
        """Reconstruct real history plus distinct staged/working bytes, including non-UTF8 data."""
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            source, destination, history = (
                parent / "source",
                parent / "copy",
                parent / "history",
            )
            source.mkdir()
            history.mkdir()

            def git(root: Path, *args: str) -> bytes:
                """Run only fixture-local Git with hooks and commit signing disabled."""
                return subprocess.check_output(
                    [
                        "git",
                        "-c",
                        "core.hooksPath=/dev/null",
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                        *args,
                    ],
                    cwd=root,
                    stderr=subprocess.PIPE,
                )

            git(source, "init", "-q")
            (source / "file").write_bytes(b"base\n")
            git(source, "add", "file")
            git(source, "commit", "-q", "-m", "fixture")
            (source / "file").write_bytes(b"staged\xff\n")
            (source / "new.md").write_bytes(b"staged addition\n")
            git(source, "add", "file", "new.md")
            (source / "file").write_bytes(b"working bytes\n")
            (source / "untracked").write_bytes(b"untracked bytes\n")
            ci.write_history(source, history)
            git(
                parent,
                "clone",
                "-q",
                "--no-checkout",
                str(history / "history.bundle"),
                str(destination),
            )
            ci.copy_checkout(source, destination)
            git(destination, "reset", "--mixed", "HEAD")
            git(destination, "apply", "--cached", str(history / "index.patch"))
            self.assertEqual(git(destination, "show", ":file"), b"staged\xff\n")
            self.assertEqual(git(destination, "show", ":new.md"), b"staged addition\n")
            self.assertEqual((destination / "file").read_bytes(), b"working bytes\n")
            self.assertEqual(
                (destination / "untracked").read_bytes(), b"untracked bytes\n"
            )
            self.assertEqual(
                git(destination, "rev-parse", "HEAD"), git(source, "rev-parse", "HEAD")
            )


class PlatformSmoke(unittest.TestCase):
    """Run the real shared Bash script with stub Rust/Cargo/sandbox/smoke binaries."""

    def setUp(self) -> None:
        """Prepare a disposable source tree and stubs that record all expensive operations."""
        self.directory = tempfile.TemporaryDirectory(prefix="transist-platform-stub-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        scripts = self.root / "scripts/ci"
        scripts.mkdir(parents=True)
        self.script = scripts / "run_platform_smoke.sh"
        shutil.copy2(ci.ROOT / "scripts/ci/run_platform_smoke.sh", self.script)
        (scripts / "run_quickstart_smoke.sh").write_text(
            '#!/bin/sh\nprintf "smoke %s\\n" "$*" >> "$TRACE"\n', encoding="utf-8"
        )
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        program = f"#!{sys.executable}\n" + """import os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "rustc":
    if args == ["-vV"]:
        print("host: " + os.environ["TEST_HOST"])
    else:
        pathlib.Path(args[args.index("-o") + 1]).write_bytes(os.environ.get("TEST_BINARY", "musl").encode())
elif name == "bwrap":
    sys.exit(int(os.environ.get("TEST_ISOLATION_FAILURE", "0")))
else:
    with open(os.environ["TRACE"], "a") as trace:
        trace.write(name + " " + " ".join(args) + " launcher=" + os.environ.get("TRANSIST_SANDBOX_LAUNCHER", "") + "\\n")
"""
        for name in ("cargo", "rustc", "bwrap"):
            stub = bin_dir / name
            stub.write_text(program, encoding="utf-8")
            stub.chmod(0o755)
        self.trace = self.root / "trace"
        self.env = dict(
            os.environ,
            PATH=str(bin_dir) + os.pathsep + os.defpath,
            TRACE=str(self.trace),
        )

    def smoke(
        self, profile: str, **overrides: str
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        """Run the shell contract with a selected native host and return its command trace."""
        hosts = {
            "linux": "x86_64-unknown-linux-gnu",
            "macos": "aarch64-apple-darwin",
            "alpine": "x86_64-unknown-linux-musl",
        }
        env = dict(self.env, TEST_HOST=hosts[profile], **overrides)
        self.trace.unlink(missing_ok=True)
        result = subprocess.run(
            ["bash", str(self.script), profile],
            env=env,
            text=True,
            capture_output=True,
            timeout=10,
        )
        return result, self.trace.read_text() if self.trace.exists() else ""

    def test_linux_and_macos_build_and_required_smoke(self) -> None:
        """Linux builds its launcher; macOS does not; both require isolated smoke."""
        for profile in ("linux", "macos"):
            with self.subTest(profile=profile):
                result, trace = self.smoke(profile)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(
                    "cargo build --locked --release -p transist-cli -p transist-server -p transist-admin",
                    trace,
                )
                self.assertEqual(
                    "-p transist-sandbox-launcher" in trace, profile == "linux"
                )
                self.assertIn(
                    "smoke --require-isolation --transist-bin target/release/transist",
                    trace,
                )

    def test_alpine_runtime_temp_launcher_and_smoke(self) -> None:
        """Alpine uses the musl target for build/tests and binds the release launcher."""
        result, trace = self.smoke("alpine")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "cargo build --locked --release --target x86_64-unknown-linux-musl", trace
        )
        self.assertIn(
            "--test e2e_plugin_runtime runtime_temp_ launcher=target/x86_64-unknown-linux-musl/release/transist-sandbox-launcher",
            trace,
        )
        self.assertIn(
            "smoke --require-isolation --transist-bin target/x86_64-unknown-linux-musl/release/transist",
            trace,
        )

    def test_unavailable_isolation_prevents_build(self) -> None:
        """Sandbox failure cannot turn into a skip or reach Cargo."""
        for profile in ("linux", "alpine"):
            with self.subTest(profile=profile):
                result, trace = self.smoke(profile, TEST_ISOLATION_FAILURE="1")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(trace, "")
                self.assertIn("isolation is unavailable", result.stderr)

    def test_glibc_output_prevents_build(self) -> None:
        """Both forbidden glibc byte patterns fail before release compilation."""
        for marker in ("ld-linux", "libc.so"):
            with self.subTest(marker=marker):
                result, trace = self.smoke("alpine", TEST_BINARY=marker)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(trace, "")
                self.assertIn("links against glibc", result.stderr)

    def test_wrong_host_prevents_build(self) -> None:
        """The main macOS/Alpine host assertions remain part of the shared script."""
        for profile in ("macos", "alpine"):
            with self.subTest(profile=profile):
                # Mutate the expected host through a separate environment binding.
                with patch.dict(self.env, TEST_HOST="incorrect"):
                    env = dict(self.env)
                    result = subprocess.run(
                        ["bash", str(self.script), profile],
                        env=env,
                        text=True,
                        capture_output=True,
                        timeout=10,
                    )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("expected native Rust host", result.stderr)
                self.assertFalse(self.trace.exists())


class DockerProbe(unittest.TestCase):
    """Exercise the real container entry/probe with temporary paths and tool stubs only."""

    def probe(
        self,
        profile: str,
        install: bool,
        *,
        isolation_failure: bool = False,
        wrong_host: bool = False,
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        """Substitute fixture mount paths and run the actual shell entry without Docker."""
        with tempfile.TemporaryDirectory(
            prefix="transist-container-probe-"
        ) as directory:
            parent = Path(directory)
            snapshot, history, work, clone = [
                parent / name for name in ("checkout", "history", "work", "clone")
            ]
            scripts = snapshot / "scripts/ci"
            scripts.mkdir(parents=True)
            history.mkdir()
            for name in ("ci_local.py", "run_platform_smoke.sh"):
                shutil.copy2(ci.ROOT / "scripts/ci" / name, scripts / name)
            for name in ("install_debian_tools.sh", "install_alpine_tools.sh"):
                (scripts / name).write_text(
                    'printf "bootstrap\\n" >> "$TRACE"\n', encoding="utf-8"
                )
            bin_dir = parent / "bin"
            bin_dir.mkdir()
            (bin_dir / "python3").symlink_to(sys.executable)
            program = f"#!{sys.executable}\n" + """import os, pathlib, sys
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with open(os.environ["TRACE"], "a") as trace:
    trace.write(name + " " + " ".join(args) + "\\n")
if name == "git" and args[0] == "clone":
    pathlib.Path(args[-1], ".git").mkdir(parents=True)
elif name == "bwrap":
    sys.exit(int(os.environ["TEST_ISOLATION_FAILURE"]))
elif name == "rustc":
    if args != ["-vV"]:
        sys.exit("Unexpected Rust compilation during probe")
    print("host: " + os.environ["TEST_HOST"])
elif name == "cargo":
    sys.exit("Unexpected Cargo during probe")
"""
            for name in ("git", "bwrap", "rustc", "rustup", "cargo"):
                path = bin_dir / name
                path.write_text(program, encoding="utf-8")
                path.chmod(0o755)
            entry = (
                ci.CONTAINER_ENTRY.replace("/tmp/history", str(clone))
                .replace("/checkout", str(snapshot))
                .replace("/history", str(history))
                .replace("/work", str(work))
            )
            host = (
                "x86_64-unknown-linux-musl"
                if profile == "alpine"
                else "x86_64-unknown-linux-gnu"
            )
            trace = parent / "trace"
            env = dict(
                os.environ,
                PATH=str(bin_dir) + os.pathsep + os.defpath,
                TRACE=str(trace),
                TEST_ISOLATION_FAILURE=str(int(isolation_failure)),
                TEST_HOST="wrong-host" if wrong_host else host,
            )
            result = subprocess.run(
                [
                    "sh",
                    "-c",
                    entry,
                    "ci-local",
                    profile,
                    str(int(install)),
                    "",
                    "head",
                    "1",
                ],
                env=env,
                text=True,
                capture_output=True,
                timeout=10,
            )
            return result, trace.read_text() if trace.exists() else ""

    def test_probe_only_runs_isolation_host_assertion_and_explicit_bootstrap(
        self,
    ) -> None:
        """Both ABI probes run the shared script with no Cargo or full gate dispatch."""
        for profile in ("debian", "alpine"):
            for install in (False, True):
                with self.subTest(profile=profile, install=install):
                    result, trace = self.probe(profile, install)
                    self.assertEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
                    self.assertIn("rustc -vV", trace)
                    self.assertIn("bwrap --die-with-parent", trace)
                    self.assertNotIn("cargo ", trace)
                    self.assertEqual("bootstrap\n" in trace, install)
                    self.assertEqual("rustup toolchain install" in trace, install)

    def test_probe_only_fails_for_missing_isolation_or_wrong_native_host(self) -> None:
        """The probe cannot pass when the real shared host/isolation assertions fail."""
        for profile in ("debian", "alpine"):
            with self.subTest(profile=profile, failure="isolation"):
                result, trace = self.probe(profile, False, isolation_failure=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("isolation is unavailable", result.stderr)
                self.assertNotIn("rustc ", trace)
            with self.subTest(profile=profile, failure="host"):
                result, trace = self.probe(profile, False, wrong_host=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("expected native Rust host", result.stderr)
                self.assertNotIn("cargo ", trace)


if __name__ == "__main__":
    unittest.main(verbosity=2)
