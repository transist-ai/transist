#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Synthetic producer solely for test_release_invariants.sh --fixture-build.

Only its isolated candidate dry-run release build is replaced. All tool probes
and other cargo commands delegate to the real executable. Never used by hosted
preparation, the PR gate, or the release-readiness gate.
"""

import os
from pathlib import Path
import shutil
import sys

if (sys.argv[1:6] == ["+1.88.0", "build", "--offline", "--locked", "--release"]
        and Path.cwd().name.startswith("transist-release-src-")):
    output = Path("target/release")
    output.mkdir(parents=True, exist_ok=True)
    for name in ("transist", "transist-server", "transist-admin", "transist-sandbox-launcher"):
        path = output / name
        path.write_text("#!/bin/sh\necho 'fixture-only release binary'\n", encoding="utf-8")
        path.chmod(0o755)
    print("FIXTURE: isolated candidate dry-run uses synthetic binaries")
else:
    search = os.pathsep.join(part for part in os.environ["PATH"].split(os.pathsep)
                             if Path(part).resolve() != Path(__file__).parent.resolve())
    cargo = shutil.which("cargo", path=search)
    if cargo is None:
        sys.exit("real cargo not found for non-build tool probe")
    os.execv(cargo, [cargo, *sys.argv[1:]])
