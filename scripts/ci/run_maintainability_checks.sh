#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

echo "Running maintainability checks..."

echo "- testing public release identity fixtures"
bash scripts/ci/test_public_release_identity.sh
echo "- checking public release identity"
bash scripts/ci/check_public_release_identity.sh

# Block accidental conflict markers in tracked source/doc files.
echo "- scanning for merge conflict markers"
if rg -n --hidden -g '!.git/**' -S '^(<{7}([^<].*)?|={7}|>{7}([^>].*)?|\|{7}([^|].*)?)$' .; then
  echo "Merge conflict markers detected in repository files." >&2
  exit 1
fi

echo "- checking repository self-containment"
python3 scripts/ci/check_repository_self_contained.py
echo "- testing repository self-containment"
python3 scripts/ci/test_repository_self_contained.py
echo "- testing conversational authoring guidance"
python3 scripts/ci/test_conversational_authoring_skill.py
echo "- checking kernel boundary"
bash scripts/ci/check_kernel_boundary.sh
echo "- testing kernel boundary fixtures"
bash scripts/ci/test_kernel_boundary.sh
echo "- checking runtime signing boundary"
bash scripts/ci/check_no_runtime_signing.sh
echo "- checking companion server boundary"
bash scripts/ci/check_companion_server_boundary.sh
echo "- testing companion server boundary fixtures"
bash scripts/ci/test_companion_server_boundary.sh
echo "- checking root facade boundary"
bash scripts/ci/check_root_facade.sh
echo "- testing root facade boundary fixtures"
bash scripts/ci/test_root_facade.sh
echo "- checking workspace dependencies"
bash scripts/ci/check_workspace_dependencies.sh
echo "- checking workspace publish policy"
bash scripts/ci/check_workspace_publish_policy.sh
echo "- checking workspace dependency centralisation policy"
python3 scripts/ci/check_workspace_dependency_policy.py
echo "- testing workspace dependency centralisation policy"
python3 scripts/ci/test_workspace_dependency_policy.py
echo "- checking pre-1.0 claims"
bash scripts/ci/check_pre_1_0_claims.sh
echo "- checking extension feature flag boundary"
bash scripts/ci/check_extension_cfg_boundary.sh
echo "- checking missing docs policy"
bash scripts/ci/check_missing_docs.sh
echo "- checking authored workflow fields"
python3 scripts/ci/check_authored_workflow_fields.py
echo "- checking maintained catalog release versions"
python3 scripts/ci/check_catalog_versions.py
echo "- testing maintained catalog release policy"
python3 scripts/ci/test_catalog_versions.py
echo "- checking unsafe-code policy"
python3 scripts/ci/check_unsafe_policy.py
echo "- testing unsafe-code policy"
python3 scripts/ci/test_unsafe_policy.py
echo "- validating docs and skills"
bash scripts/ci/validate_docs_and_skills.sh

echo "Maintainability checks passed."
