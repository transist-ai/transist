#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise real Cargo 1.88 subcommand discovery with harmless temporary tools.

No Rust compilation, installation or network operations occur. Cargo itself is
never stubbed; only the competing cargo-llvm-cov executables are tiny fixtures.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class CargoToolDiscovery(unittest.TestCase):
    """Verify the CARGO_HOME exception and normal explicit PATH precedence."""

    def test_real_cargo_home_and_path_precedence(self) -> None:
        """An unlisted Cargo bin is preferred; listing it restores PATH ordering."""
        cargo = shutil.which("cargo")
        self.assertIsNotNone(cargo, "real Cargo 1.88.0 must be available")
        with tempfile.TemporaryDirectory(
            prefix="transist-cargo-discovery-", dir="/tmp"
        ) as tmp:
            root = Path(tmp)
            home_bin = root / "cargo-home/bin"
            tool_bin = root / "ci-tools/bin"
            for label, directory in (("cargo-home", home_bin), ("tool-root", tool_bin)):
                directory.mkdir(parents=True)
                fixture = directory / "cargo-llvm-cov"
                fixture.write_text(
                    '#!/bin/sh\n[ "$#" = 2 ] && [ "$1" = llvm-cov ] '
                    '&& [ "$2" = --version ] || exit 1\n'
                    f"printf '%s\\n' '{label}: llvm-cov --version'\n",
                    encoding="utf-8",
                )
                fixture.chmod(0o755)
            env = dict(
                os.environ,
                CARGO_HOME=str(home_bin.parent),
                CARGO_NET_OFFLINE="true",
                RUSTUP_AUTO_INSTALL="0",
                RUSTUP_TOOLCHAIN="1.88.0",
                PATH=os.pathsep.join((str(tool_bin), os.defpath)),
            )
            # Use an isolated Cargo home outside the repository's configuration.
            version = subprocess.check_output(
                [cargo, "--version"], cwd=root, env=env, text=True
            ).strip()
            self.assertTrue(version.startswith("cargo 1.88.0 "), version)
            print(f"Real executable: {cargo}; {version}", flush=True)
            cases = (
                ((tool_bin,), "cargo-home"),
                ((tool_bin, home_bin), "tool-root"),
                ((home_bin, tool_bin), "cargo-home"),
            )
            for directories, expected in cases:
                with self.subTest(path_order=directories):
                    env["PATH"] = os.pathsep.join((*map(str, directories), os.defpath))
                    actual = subprocess.check_output(
                        [cargo, "llvm-cov", "--version"], cwd=root, env=env, text=True
                    ).strip()
                    self.assertEqual(actual, f"{expected}: llvm-cov --version")
                    order = tuple(str(p.relative_to(root)) for p in directories)
                    print(f"PATH order {order} => {actual}", flush=True)
            (home_bin / "cargo-llvm-cov").unlink()
            env["PATH"] = os.pathsep.join((str(tool_bin), os.defpath))
            actual = subprocess.check_output(
                [cargo, "llvm-cov", "--version"], cwd=root, env=env, text=True
            ).strip()
            self.assertEqual(actual, "tool-root: llvm-cov --version")
            print(f"No home executable => {actual}", flush=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
