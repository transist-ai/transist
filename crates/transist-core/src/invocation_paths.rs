// SPDX-License-Identifier: MPL-2.0
//! Shared side-effect-free placement checks for local invocation-private temporary roots.
use crate::dispatch::canonicalize_path_allow_missing;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use transist_protocol::{ExecutionRequest, canonical_external_policy_path};

/// Returns the canonical OS temporary parent used by all local invocation paths.
/// It deliberately ignores caller TMPDIR so workspace aliases cannot select it.
pub fn runtime_temp_parent() -> PathBuf {
    #[cfg(target_os = "macos")]
    let parent = PathBuf::from("/private/tmp");
    #[cfg(not(target_os = "macos"))]
    let parent = PathBuf::from("/tmp");
    parent.canonicalize().unwrap_or(parent)
}

/// Rejects a temporary parent covered by the request's workspace, home or external
/// grants before any staging/owner file is created. Existing ancestors and symlinks
/// are canonicalized; missing write targets are inspected without creating them.
/// Home/workspace are literal paths; only external grants use grant grammar.
pub fn validate_runtime_temp_parent(
    parent: &Path,
    home: &Path,
    request: &ExecutionRequest,
) -> Result<()> {
    let parent = parent.canonicalize()?;
    let mut exposed = vec![
        canonicalize_path_allow_missing(home, "home_dir")?,
        canonicalize_path_allow_missing(Path::new(&request.paths.workspace_dir), "workspace_dir")?,
    ];
    if let Some(filesystem) = request.policy.get("filesystem") {
        for key in ["read_allow_paths", "write_allow_paths"] {
            if let Some(values) = filesystem.get(key) {
                for value in values
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("invalid filesystem allow paths"))?
                {
                    let raw = value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid filesystem allow path"))?;
                    exposed.push(canonical_external_policy_path(raw, true)?);
                }
            }
        }
    }
    for root in exposed {
        if parent.starts_with(root) {
            bail!(
                "runtime_temp_exposed_root_overlap: private temporary parent is covered by a workspace, home or external grant"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(home: &Path, workspace: &Path) -> ExecutionRequest {
        serde_json::from_value(serde_json::json!({
            "type":"tep.invoke.request.v1", "protocol_version":"tep.v1", "invoke_id":"test", "run_id":"test", "workflow_id":"test", "step":{"id":"test","execution":1}, "adapter_kind":"process", "action":"test", "input":{},
            "paths":{"run_root":home.join("runs/run-test"), "workspace_dir":workspace, "artifacts_dir":home.join("artifacts"), "evidence_dir":home.join("evidence")}, "requested_capabilities":[], "policy":{}, "metadata":{}
        })).unwrap()
    }

    #[test]
    fn literal_location_names_are_accepted_and_canonical_overlap_still_fails() {
        let base = tempfile::tempdir().unwrap();
        let parent = base.path().join("runtime-parent");
        std::fs::create_dir(&parent).unwrap();
        for (home_name, workspace_name) in [
            ("home[review]", "workspace"),
            ("home", "workspace[review]"),
            ("home[review]?", "workspace[review]?"),
        ] {
            let home = base.path().join(home_name);
            let workspace = base.path().join(workspace_name);
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&workspace).unwrap();
            let request = request(&home, &workspace);
            validate_runtime_temp_parent(&parent, &home, &request)
                .expect("literal paths are not grant patterns");
            for covered in [&home, &workspace] {
                let nested_parent = covered.join("runtime-parent");
                std::fs::create_dir(&nested_parent).unwrap();
                let error =
                    validate_runtime_temp_parent(&nested_parent, &home, &request).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("runtime_temp_exposed_root_overlap"),
                    "{error:#}"
                );
                std::fs::remove_dir(nested_parent).unwrap();
            }
        }
        #[cfg(unix)]
        {
            let alias = base.path().join("workspace[alias]?");
            std::os::unix::fs::symlink(&parent, &alias).unwrap();
            let home = base.path().join("home");
            let request = request(&home, &alias);
            let error = validate_runtime_temp_parent(&parent, &home, &request).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("runtime_temp_exposed_root_overlap"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn external_grants_keep_their_own_grammar() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let workspace = base.path().join("workspace");
        let parent = base.path().join("runtime-parent");
        for path in [&home, &workspace, &parent] {
            std::fs::create_dir(path).unwrap();
        }
        for name in ["grant[review]", "grant?review"] {
            let grant = base.path().join(name);
            std::fs::create_dir(&grant).unwrap();
            let mut request = request(&home, &workspace);
            request.policy = serde_json::json!({"filesystem":{"write_allow_paths":[grant]}});
            assert!(
                validate_runtime_temp_parent(&parent, &home, &request).is_err(),
                "external grant grammar must remain enforced"
            );
        }
    }
}
