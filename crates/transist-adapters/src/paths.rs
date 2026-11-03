// SPDX-License-Identifier: MPL-2.0
//! File-system path helpers for plugin home resolution and runtime env injection.

use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use transist_protocol::ExecutionRequest;

const TRANSIST_RUNTIME_METADATA_FILE_ENV: &str = "TRANSIST_RUNTIME_METADATA_FILE";
pub(crate) const RUNTIME_TEMP_DIR_METADATA_KEY: &str = "__transist_runtime_temp_dir";

/// Resolved child environment for one process or MCP entrypoint.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeTempEnvironment {
    entries: Vec<(String, String)>,
    pub(crate) tmpdir: PathBuf,
}

/// Resolves the Transist home from a kernel-owned run or invocation-staging path.
pub(crate) fn home_dir_from_run_root(run_root: &Path) -> Result<PathBuf> {
    for ancestor in run_root.ancestors() {
        if matches!(
            ancestor.file_name(),
            Some(name) if name == OsStr::new("runs") || name == OsStr::new(".transist-staging")
        ) {
            return ancestor.parent().map(Path::to_path_buf).ok_or_else(|| {
                anyhow!(
                    "runtime path '{}' is missing its home parent",
                    run_root.display()
                )
            });
        }
    }
    Err(anyhow!(
        "runtime path '{}' is not below a Transist run or staging directory",
        run_root.display()
    ))
}

pub(crate) fn inject_runtime_paths_env(cmd: &mut Command, request: &ExecutionRequest) {
    cmd.env("TRANSIST_WORKSPACE_DIR", &request.paths.workspace_dir);
    cmd.env("TRANSIST_ARTIFACTS_DIR", &request.paths.artifacts_dir);
    cmd.env("TRANSIST_EVIDENCE_DIR", &request.paths.evidence_dir);
    if let Some(metadata_path) = request
        .metadata
        .get("runtime_metadata_file")
        .and_then(serde_json::Value::as_str)
    {
        cmd.env(TRANSIST_RUNTIME_METADATA_FILE_ENV, metadata_path);
    }
}

/// Resolves entrypoint environment declarations beneath the request's private
/// temporary root and creates each declared directory before child startup.
pub(crate) fn prepare_runtime_temp_environment(
    request: &ExecutionRequest,
    environment: &BTreeMap<String, String>,
) -> Result<RuntimeTempEnvironment> {
    let temporary_root = runtime_temp_root(request)?;
    let visible_root = if request
        .metadata
        .get(RUNTIME_TEMP_DIR_METADATA_KEY)
        .is_some()
    {
        runtime_temp_visible_root(&temporary_root)?
    } else {
        temporary_root.clone()
    };
    // Validate the complete set before the first mkdir/chmod. No manifest-owned
    // path may traverse a symlink into kernel-owned document copies.
    for value in environment.values() {
        if let Some(relative) = transist_plugins::runtime_temp_relative_path(value)? {
            let mut path = temporary_root.clone();
            for component in Path::new(relative).components() {
                path.push(component);
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        bail!("runtime temporary declaration traverses a symlink or non-directory")
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    let mut entries = Vec::with_capacity(environment.len());
    let mut declared_tmpdir = None;
    for (name, value) in environment {
        let value = match transist_plugins::runtime_temp_relative_path(value)? {
            Some(relative) => {
                let host_path = temporary_root.join(relative);
                fs::create_dir_all(&host_path).map_err(|error| {
                    anyhow!(
                        "failed creating runtime temporary directory '{}': {error}",
                        host_path.display()
                    )
                })?;
                set_private_directory_mode(&host_path)?;
                let resolved = visible_root.join(relative);
                if name == "TMPDIR" {
                    declared_tmpdir = Some(resolved.clone());
                }
                resolved.display().to_string()
            }
            None => value.clone(),
        };
        entries.push((name.clone(), value));
    }
    Ok(RuntimeTempEnvironment {
        entries,
        tmpdir: declared_tmpdir.unwrap_or(visible_root),
    })
}

/// Applies the already-resolved manifest environment after managed environment
/// policy clearing and before runtime-injected variables and secret bindings.
pub(crate) fn apply_runtime_temp_environment(
    cmd: &mut Command,
    environment: &RuntimeTempEnvironment,
) {
    for (name, value) in &environment.entries {
        cmd.env(name, value);
    }
}

/// Forces child processes to use their declared temporary directory, or the
/// invocation-private root when no declaration selected `TMPDIR`.
pub(crate) fn force_private_tmpdir(cmd: &mut Command, temporary_dir: &Path) {
    cmd.env("TMPDIR", temporary_dir);
}

/// Returns the host-private temporary root injected by invocation staging.
///
/// Direct adapter tests and legacy callers without staging retain their current
/// private run root as a compatibility fallback.
pub(crate) fn runtime_temp_root(request: &ExecutionRequest) -> Result<PathBuf> {
    let path = request
        .metadata
        .get(RUNTIME_TEMP_DIR_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&request.paths.run_root));
    if !path.is_dir() {
        bail!(
            "runtime temporary root '{}' is not an existing directory",
            path.display()
        );
    }
    Ok(path)
}

/// Maps a host-private temporary root to the path visible inside the sandbox.
pub(crate) fn runtime_temp_visible_root(host_root: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let name = host_root.file_name().ok_or_else(|| {
            anyhow!(
                "runtime temporary root '{}' is missing a final path component",
                host_root.display()
            )
        })?;
        Ok(Path::new("/tmp").join(name))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(host_root.to_path_buf())
    }
}

fn set_private_directory_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

pub(crate) fn apply_managed_child_env_policy(cmd: &mut Command, request: &ExecutionRequest) {
    let Some(runtime_auth) = request.policy.get("runtime_auth_context") else {
        return;
    };
    let active = runtime_auth
        .get("active")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !active {
        return;
    }
    cmd.env_clear();
    if let Some(vars) = runtime_auth
        .get("child_env_allowlist")
        .and_then(serde_json::Value::as_array)
    {
        for name in vars.iter().filter_map(serde_json::Value::as_str) {
            if let Ok(value) = std::env::var(name) {
                cmd.env(name, value);
            }
        }
    }
    cmd.env("TRANSIST_MANAGED_RUNTIME_AUTH_ACTIVE", "1");
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{AdapterKind, InvokePaths, InvokeStep};

    #[test]
    fn review_step4_temp_environment_cannot_create_document_inputs_or_outputs() {
        for (area, index) in [("read", 0), ("read", 9), ("write", 120)] {
            for alias in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let target = temp
                    .path()
                    .join(format!("documents/{area}/binding-{index:020}"));
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                if area == "read" {
                    std::fs::create_dir(&target).unwrap();
                    std::fs::write(target.join("member.txt"), "committed bytes").unwrap();
                }
                let relative = if alias {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&target, temp.path().join("cache-alias")).unwrap();
                    "cache-alias/injected".to_string()
                } else {
                    format!("documents/{area}/binding-{index:020}/injected")
                };
                let request: ExecutionRequest = serde_json::from_value(serde_json::json!({
                    "type":"tep.invoke.request.v1", "protocol_version":"tep.v1", "invoke_id":"test", "run_id":"test", "workflow_id":"test", "step":{"id":"test","execution":1}, "adapter_kind":"process", "action":"test", "input":{},
                    "paths":{"run_root":temp.path(), "workspace_dir":temp.path(), "artifacts_dir":temp.path(), "evidence_dir":temp.path()}, "requested_capabilities":[], "policy":{}, "metadata":{"__transist_runtime_temp_dir":temp.path()}
                })).unwrap();
                let env = BTreeMap::from([
                    ("A_VALID".into(), "runtime://temp/otherwise-valid".into()),
                    ("CACHE_DIR".into(), format!("runtime://temp/{relative}")),
                ]);
                assert!(
                    prepare_runtime_temp_environment(&request, &env).is_err(),
                    "document {area} collision accepted, alias={alias}"
                );
                assert!(!temp.path().join("otherwise-valid").exists());
                assert!(!target.join("injected").exists());
                if area == "read" {
                    assert_eq!(
                        std::fs::read_to_string(target.join("member.txt")).unwrap(),
                        "committed bytes"
                    );
                    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
                } else {
                    assert!(!target.exists(), "missing tree output was precreated");
                }
            }
        }
    }

    #[test]
    fn force_private_tmpdir_overrides_existing_value() {
        let mut command = Command::new("true");
        command.env("TMPDIR", "/manifest/tmp");

        force_private_tmpdir(&mut command, Path::new("/private/invoke-root"));

        let value = command
            .get_envs()
            .find_map(|(name, value)| (name == "TMPDIR").then(|| value.unwrap()))
            .expect("TMPDIR override");
        assert_eq!(value, std::ffi::OsStr::new("/private/invoke-root"));
    }

    #[test]
    fn runtime_temp_environment_resolves_declarations_and_preserves_literals() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut request = ExecutionRequest {
            message_type: "tep.invoke.request.v1".to_string(),
            protocol_version: "tep.v1".to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            workflow_id: "workflow-1".to_string(),
            step: InvokeStep {
                id: "step-1".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input: serde_json::json!({}),
            paths: InvokePaths {
                run_root: temp.path().display().to_string(),
                artifacts_dir: temp.path().join("artifacts").display().to_string(),
                evidence_dir: temp.path().join("evidence").display().to_string(),
                workspace_dir: temp.path().display().to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        request.metadata[RUNTIME_TEMP_DIR_METADATA_KEY] =
            serde_json::Value::String(temp.path().display().to_string());
        let environment = BTreeMap::from([
            ("HOME".to_string(), "/literal/home".to_string()),
            (
                "CACHE".to_string(),
                "runtime://temp/cache/provider".to_string(),
            ),
            ("TMPDIR".to_string(), "runtime://temp/work".to_string()),
        ]);

        let resolved = prepare_runtime_temp_environment(&request, &environment).expect("resolve");

        assert!(temp.path().join("cache/provider").is_dir());
        assert!(temp.path().join("work").is_dir());
        assert!(
            resolved
                .entries
                .iter()
                .any(|(name, value)| name == "HOME" && value == "/literal/home")
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            resolved.tmpdir,
            Path::new("/tmp")
                .join(temp.path().file_name().unwrap())
                .join("work")
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(resolved.tmpdir, temp.path().join("work"));
    }
}
