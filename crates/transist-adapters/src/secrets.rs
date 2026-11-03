// SPDX-License-Identifier: MPL-2.0
//! Adapter-specific secret materialization helpers.
//!
//! `resolve_secret_value` centralizes the missing-required-secret check
//! shared by every binding path, and each `build_*` / `inject_*` helper
//! emits a consistent "this binding kind is not supported by adapter X"
//! error for scope-mismatch cases. A larger generic abstraction (e.g.,
//! per-binding-kind dispatch trait) was evaluated and rejected: it would
//! add closures or trait objects to save four small `match` arms while
//! making each adapter's accepted binding set harder to read. The
//! cross-adapter behaviors (missing secret, unsupported binding kind)
//! are exercised by `tests` below so future drift is caught at the
//! shared boundary rather than per adapter.

use crate::*;
use tempfile::TempPath;

pub(crate) struct ProcessSecretEnv {
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) temp_files: Vec<TempPath>,
}

#[cfg(test)]
impl std::fmt::Debug for ProcessSecretEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSecretEnv").finish_non_exhaustive()
    }
}

pub(crate) struct McpSecretBindings {
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) args: Vec<String>,
}

#[cfg(test)]
impl std::fmt::Debug for McpSecretBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSecretBindings").finish_non_exhaustive()
    }
}

/// Resolves a single secret value from the secret map.
///
/// Returns `Ok(None)` if the secret is absent and not required; returns an error if
/// it is required but missing.
fn resolve_secret_value<'a>(
    secret_name: &str,
    secret_required: bool,
    action_name: &str,
    secret_values: &'a BTreeMap<String, String>,
) -> Result<Option<&'a str>> {
    match secret_values.get(secret_name) {
        Some(value) => Ok(Some(value.as_str())),
        None if secret_required => bail!(
            "required secret '{}' is missing for action '{}'",
            secret_name,
            action_name
        ),
        None => Ok(None),
    }
}

pub(crate) fn build_process_secret_env(
    staging_dir: &Path,
    action: &ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
) -> Result<ProcessSecretEnv> {
    let mut env = BTreeMap::new();
    let mut temp_files = Vec::new();
    for secret in &action.secret_inputs {
        let Some(value) =
            resolve_secret_value(&secret.name, secret.required, &action.name, secret_values)?
        else {
            continue;
        };
        match &secret.binding {
            SecretBinding::Env { name } => {
                env.insert(name.clone(), value.to_string());
            }
            SecretBinding::TempFile { path_env } => {
                let mut file = NamedTempFile::new_in(staging_dir)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.as_file()
                        .set_permissions(fs::Permissions::from_mode(0o600))?;
                }
                file.write_all(value.as_bytes())?;
                let temp_path = file.into_temp_path();
                let path = temp_path.to_path_buf();
                temp_files.push(temp_path);
                env.insert(path_env.clone(), path.display().to_string());
            }
            _ => bail!("process actions only support env or temp_file secret bindings"),
        }
    }
    Ok(ProcessSecretEnv { env, temp_files })
}

pub(crate) fn build_mcp_secret_bindings(
    action: &ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
) -> Result<McpSecretBindings> {
    let mut env = BTreeMap::new();
    let mut args = Vec::new();
    for secret in &action.secret_inputs {
        let Some(value) =
            resolve_secret_value(&secret.name, secret.required, &action.name, secret_values)?
        else {
            continue;
        };
        match &secret.binding {
            SecretBinding::Env { name } => {
                env.insert(name.clone(), value.to_string());
            }
            SecretBinding::Arg { flag } => {
                args.push(flag.clone());
                args.push(value.to_string());
            }
            _ => bail!("mcp actions only support env or arg secret bindings"),
        }
    }
    Ok(McpSecretBindings { env, args })
}

/// Builds HTTP header key-value pairs from action secret bindings.
pub(crate) fn build_http_header_bindings(
    action: &ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for secret in &action.secret_inputs {
        let Some(value) =
            resolve_secret_value(&secret.name, secret.required, &action.name, secret_values)?
        else {
            continue;
        };
        if let SecretBinding::Header { name } = &secret.binding {
            headers.push((name.clone(), value.to_string()));
        }
    }
    Ok(headers)
}

/// Injects HTTP body-field secrets into the action input, returning a modified copy.
pub(crate) fn inject_http_action_secrets(
    input: &serde_json::Value,
    action: &ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let mut input = input.clone();
    for secret in &action.secret_inputs {
        let Some(value) =
            resolve_secret_value(&secret.name, secret.required, &action.name, secret_values)?
        else {
            continue;
        };
        match &secret.binding {
            SecretBinding::BodyField { name } => {
                let object = input
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("HTTP action input must be an object"))?;
                object.insert(name.clone(), serde_json::Value::String(value.to_string()));
            }
            SecretBinding::Header { .. } => {}
            _ => bail!("HTTP actions only support header or body_field secret bindings"),
        }
    }
    Ok(input)
}

pub(crate) fn cleanup_temp_files(paths: &mut Vec<TempPath>) {
    for path in paths.drain(..) {
        let _ = path.close();
    }
}

#[cfg(test)]
mod tests {
    use crate::secrets::*;
    use std::{fs, path::PathBuf};
    use transist_plugins::{ActionDescriptor, SecretBinding, SecretInput};
    use transist_prerequisites::PrerequisiteSet;

    fn action_with(secret: SecretInput) -> ActionDescriptor {
        ActionDescriptor {
            name: "act".to_string(),
            input_schema: "in.json".to_string(),
            output_schema: "out.json".to_string(),
            secret_inputs: vec![secret],
            bindings: BTreeMap::new(),
            prerequisites: PrerequisiteSet::default(),
            provider_kind: None,
        }
    }

    fn required_secret(name: &str, binding: SecretBinding) -> SecretInput {
        SecretInput {
            name: name.to_string(),
            required: true,
            binding,
        }
    }

    fn empty_values() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn one_value(k: &str, v: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert(k.to_string(), v.to_string());
        m
    }

    #[test]
    fn missing_required_secret_rejected_across_all_adapters() {
        let action = action_with(required_secret(
            "TOK",
            SecretBinding::Env {
                name: "TOK".to_string(),
            },
        ));
        let _plugin_dir = tempfile::tempdir().unwrap();
        let staging_dir = tempfile::tempdir().unwrap();
        let process = build_process_secret_env(staging_dir.path(), &action, &empty_values());
        let mcp = build_mcp_secret_bindings(&action, &empty_values());
        let header_action = action_with(required_secret(
            "TOK",
            SecretBinding::Header {
                name: "X-Tok".to_string(),
            },
        ));
        let header = build_http_header_bindings(&header_action, &empty_values());
        let body_action = action_with(required_secret(
            "TOK",
            SecretBinding::BodyField {
                name: "tok".to_string(),
            },
        ));
        let body =
            inject_http_action_secrets(&serde_json::json!({}), &body_action, &empty_values());
        for (label, err) in [
            ("process", process.err()),
            ("mcp", mcp.err()),
            ("http_header", header.err()),
            ("http_body", body.err()),
        ] {
            let err = err.unwrap_or_else(|| panic!("{label} did not reject missing secret"));
            let msg = format!("{err}");
            assert!(
                msg.contains("required secret 'TOK' is missing"),
                "{label}: unexpected error: {msg}"
            );
        }
    }

    #[test]
    fn unsupported_binding_kind_rejected_per_adapter() {
        let _plugin_dir = tempfile::tempdir().unwrap();
        let staging_dir = tempfile::tempdir().unwrap();
        let header_for_process = action_with(required_secret(
            "TOK",
            SecretBinding::Header {
                name: "X".to_string(),
            },
        ));
        let err = build_process_secret_env(
            staging_dir.path(),
            &header_for_process,
            &one_value("TOK", "v"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("process actions only support"), "{err}");

        let body_for_mcp = action_with(required_secret(
            "TOK",
            SecretBinding::BodyField {
                name: "x".to_string(),
            },
        ));
        let err = build_mcp_secret_bindings(&body_for_mcp, &one_value("TOK", "v"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("mcp actions only support"), "{err}");

        let env_for_http = action_with(required_secret(
            "TOK",
            SecretBinding::Env {
                name: "X".to_string(),
            },
        ));
        let err = inject_http_action_secrets(
            &serde_json::json!({}),
            &env_for_http,
            &one_value("TOK", "v"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP actions only support"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_secret_is_private_and_raii_cleaned() {
        use std::os::unix::fs::PermissionsExt;

        let plugin_dir = tempfile::tempdir().unwrap();
        let staging_dir = tempfile::tempdir().unwrap();
        let action = action_with(required_secret(
            "TOK",
            SecretBinding::TempFile {
                path_env: "TOK_FILE".to_string(),
            },
        ));
        let process =
            build_process_secret_env(staging_dir.path(), &action, &one_value("TOK", "secret"))
                .expect("temp file binding");
        let path = process.env.get("TOK_FILE").expect("path env");
        let path = PathBuf::from(path);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(path.starts_with(staging_dir.path()));
        assert!(!path.starts_with(plugin_dir.path()));
        drop(process);
        assert!(!path.exists());
    }
}
