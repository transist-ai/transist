// SPDX-License-Identifier: MPL-2.0
use crate::manifest::{
    AdapterKind, PLUGIN_MANIFEST_SCHEMA_V1, PluginEntrypoint, PluginManifest, PluginPackage,
    runtime_temp_relative_path,
};
use crate::schema::action_schema_paths;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use transist_prerequisites::validate_prerequisite_set;

/// Validates a parsed plugin manifest against Transist's plugin rules.
pub fn validate_manifest(plugin_dir: &Path, manifest: &PluginManifest) -> Result<()> {
    validate_manifest_with_schemas(
        plugin_dir,
        manifest,
        &mut crate::schema::SchemaCache::default(),
    )
}

/// Validates the complete contract while retaining compiled roots for subsequent input validation.
pub(crate) fn validate_manifest_with_schemas(
    plugin_dir: &Path,
    manifest: &PluginManifest,
    schemas: &mut crate::schema::SchemaCache,
) -> Result<()> {
    crate::record_validation_work(crate::ValidationWork::ContractValidation);
    if manifest.schema != PLUGIN_MANIFEST_SCHEMA_V1 {
        bail!(
            "unsupported plugin schema '{}'; expected '{}'",
            manifest.schema,
            PLUGIN_MANIFEST_SCHEMA_V1
        );
    }
    if manifest.id.trim().is_empty() {
        bail!("plugin id cannot be empty");
    }
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::PluginId,
        &manifest.id,
    )
    .map_err(|e| anyhow::anyhow!("plugin id: {e}"))?;
    if manifest.version.trim().is_empty() {
        bail!("plugin version cannot be empty");
    }
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::PluginVersion,
        &manifest.version,
    )
    .map_err(|e| anyhow::anyhow!("plugin version: {e}"))?;
    if manifest.actions.is_empty() {
        bail!("plugin actions cannot be empty");
    }
    let package_files = validate_package_files(plugin_dir, &manifest.package)?;
    validate_prerequisite_set(&manifest.prerequisites).context("invalid plugin prerequisites")?;
    let mut seen_action_names = BTreeSet::new();
    for action in &manifest.actions {
        if action.name.trim().is_empty() {
            bail!("plugin action name cannot be empty");
        }
        if !seen_action_names.insert(action.name.clone()) {
            bail!("duplicate plugin action name '{}'", action.name);
        }
        action.action_data_projection_enabled()?;
        ensure_relative_file(plugin_dir, &action.input_schema)?;
        ensure_relative_file(plugin_dir, &action.output_schema)?;
        ensure_selected_file(&package_files, &action.input_schema, "action input schema")?;
        ensure_selected_file(
            &package_files,
            &action.output_schema,
            "action output schema",
        )?;
        for schema_path in action_schema_paths(plugin_dir, action)
            .with_context(|| format!("failed discovering schemas for action '{}'", action.name))?
        {
            ensure_selected_file(&package_files, &schema_path, "schema reference")?;
        }
        (|| -> Result<()> {
            schemas.compile(plugin_dir, &action.input_schema, "input")?;
            schemas.compile(plugin_dir, &action.output_schema, "output")?;
            Ok(())
        })()
        .with_context(|| format!("invalid input/output schemas for action '{}'", action.name))?;
        validate_prerequisite_set(&action.prerequisites)
            .with_context(|| format!("invalid prerequisites for action '{}'", action.name))?;
        let prompt_binding = action.prompt_binding()?;
        let skills_binding = action.skills_binding()?;
        let gate_decision_binding = action.gate_decision_binding()?;
        if let (Some(prompt_binding), Some(skills_binding)) = (&prompt_binding, &skills_binding) {
            if prompt_binding.inline_field == skills_binding.bundle_field
                || prompt_binding.inline_field == skills_binding.rendered_field
                || prompt_binding.file_field.as_deref()
                    == Some(skills_binding.bundle_field.as_str())
                || prompt_binding.file_field.as_deref()
                    == Some(skills_binding.rendered_field.as_str())
            {
                bail!(
                    "action '{}' prompt and skills bindings must use distinct field names",
                    action.name
                );
            }
        }
        let mut injected_fields = BTreeSet::new();
        if let Some(prompt_binding) = prompt_binding {
            injected_fields.insert(prompt_binding.inline_field);
            if let Some(file_field) = prompt_binding.file_field {
                injected_fields.insert(file_field);
            }
        }
        if let Some(skills_binding) = skills_binding {
            for field in [skills_binding.bundle_field, skills_binding.rendered_field] {
                if !injected_fields.insert(field.clone()) {
                    bail!(
                        "action '{}' runtime-injected bindings must use distinct field names; '{}' is duplicated",
                        action.name,
                        field
                    );
                }
            }
        }
        if let Some(gate_decision_binding) = gate_decision_binding
            && !injected_fields.insert(gate_decision_binding.decision_field.clone())
        {
            bail!(
                "action '{}' runtime-injected bindings must use distinct field names; '{}' is duplicated",
                action.name,
                gate_decision_binding.decision_field
            );
        }
        if manifest.adapter == AdapterKind::Mcp {
            let tool_name = action
                .bindings
                .get("tool_name")
                .and_then(|value| value.as_str());
            if tool_name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
            {
                bail!(
                    "action '{}' for adapter 'mcp' requires bindings.tool_name",
                    action.name
                );
            }
        }
    }
    validate_entrypoint(plugin_dir, manifest, &package_files)?;
    Ok(())
}

pub(crate) fn validate_package_files(
    plugin_dir: &Path,
    package: &PluginPackage,
) -> Result<BTreeSet<String>> {
    if package.files.is_empty() {
        bail!("plugin package.files cannot be empty");
    }
    let canonical_plugin_dir = fs::canonicalize(plugin_dir)
        .with_context(|| format!("failed canonicalizing plugin dir: {}", plugin_dir.display()))?;
    let mut selected = BTreeSet::new();
    for rel in &package.files {
        validate_package_path(rel)?;
        if rel == crate::manifest::PLUGIN_MANIFEST_FILE_NAME {
            bail!("plugin package.files must not include 'plugin.yaml'");
        }
        if !selected.insert(rel.clone()) {
            bail!("duplicate plugin package file '{rel}'");
        }
        let path = plugin_dir.join(rel);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed reading package file metadata: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "selected plugin package file '{}' is a symlink",
                path.display()
            );
        }
        if !metadata.is_file() {
            bail!(
                "selected plugin package file '{}' is not a regular file",
                path.display()
            );
        }
        let canonical_path = fs::canonicalize(&path)
            .with_context(|| format!("failed canonicalizing package file: {}", path.display()))?;
        if !canonical_path.starts_with(&canonical_plugin_dir) {
            bail!("plugin package file '{rel}' escapes plugin directory");
        }
    }
    Ok(selected)
}

fn validate_package_path(rel: &str) -> Result<()> {
    if rel.trim().is_empty() {
        bail!("plugin package file path cannot be empty");
    }
    if Path::new(rel).is_absolute() {
        bail!("plugin package file '{rel}' must be relative to the plugin directory");
    }
    if rel.contains('\\') {
        bail!("plugin package file '{rel}' must use POSIX path separators");
    }
    if rel
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        bail!("plugin package file '{rel}' contains an invalid path component");
    }
    Ok(())
}

fn ensure_selected_file(selected: &BTreeSet<String>, rel: &str, label: &str) -> Result<()> {
    if rel.trim().is_empty() {
        bail!("{label} path cannot be empty");
    }
    if !selected.contains(rel) {
        bail!("{label} '{rel}' is missing from plugin package.files");
    }
    Ok(())
}

fn ensure_relative_file(plugin_dir: &Path, rel: &str) -> Result<()> {
    if rel.trim().is_empty() {
        bail!("schema reference cannot be empty");
    }
    if Path::new(rel).is_absolute() {
        bail!("schema reference '{rel}' must be a relative path inside the plugin directory");
    }
    let path = plugin_dir.join(rel);
    if !path.exists() {
        bail!("referenced schema '{}' does not exist", path.display());
    }
    ensure_within_plugin_dir(plugin_dir, &path, "schema reference", rel)?;
    if !fs::symlink_metadata(&path)
        .with_context(|| format!("failed reading schema metadata: {}", path.display()))?
        .is_file()
    {
        bail!(
            "referenced schema '{}' is not a regular file",
            path.display()
        );
    }
    Ok(())
}

/// Verifies that `path` resolves to a location inside `plugin_dir`.
///
/// Both paths are canonicalized before the containment check so that relative
/// references using `..` components (for example `../outside.json`) cannot
/// escape the plugin package and still pass validation because the joined
/// path's raw components happen to start with the plugin directory's
/// components.
fn ensure_within_plugin_dir(plugin_dir: &Path, path: &Path, label: &str, rel: &str) -> Result<()> {
    let canonical_plugin_dir = fs::canonicalize(plugin_dir)
        .with_context(|| format!("failed canonicalizing plugin dir: {}", plugin_dir.display()))?;
    let canonical_path = fs::canonicalize(path)
        .with_context(|| format!("failed canonicalizing {}: {}", label, path.display()))?;
    if !canonical_path.starts_with(&canonical_plugin_dir) {
        bail!("{label} '{rel}' escapes plugin directory");
    }
    Ok(())
}

/// Validates an optional relative directory reference (for example an
/// entrypoint `cwd`) against `plugin_dir`.
///
/// Unlike [`ensure_within_plugin_dir`], the referenced directory is not
/// required to exist because plugins may create it at runtime. Absolute paths
/// and paths that canonically escape `plugin_dir` are rejected. When the
/// directory does not exist yet, the raw `..` component count is used as a
/// best-effort containment check.
fn ensure_relative_dir_reference(plugin_dir: &Path, rel: &str, label: &str) -> Result<()> {
    if rel.trim().is_empty() {
        bail!("{label} cannot be empty");
    }
    if Path::new(rel).is_absolute() {
        bail!("{label} '{rel}' must be relative to the plugin directory");
    }
    let path = plugin_dir.join(rel);
    let canonical_plugin_dir = fs::canonicalize(plugin_dir)
        .with_context(|| format!("failed canonicalizing plugin dir: {}", plugin_dir.display()))?;
    match fs::canonicalize(&path) {
        Ok(canonical_path) => {
            if !canonical_path.starts_with(&canonical_plugin_dir) {
                bail!("{label} '{rel}' escapes plugin directory");
            }
        }
        Err(_) => {
            let resolved = canonical_plugin_dir.join(rel);
            for component in resolved.components() {
                use std::path::Component;
                if let Component::ParentDir = component {
                    bail!("{label} '{rel}' escapes plugin directory");
                }
            }
        }
    }
    Ok(())
}

fn validate_entrypoint(
    plugin_dir: &Path,
    manifest: &PluginManifest,
    package_files: &BTreeSet<String>,
) -> Result<()> {
    match (&manifest.adapter, &manifest.entrypoint) {
        (
            AdapterKind::Process,
            PluginEntrypoint::Process {
                command, cwd, env, ..
            },
        ) => {
            if command.trim().is_empty() {
                bail!("process entrypoint.command cannot be empty");
            }
            let path = plugin_dir.join(command);
            if !path.exists() {
                bail!("process entrypoint '{}' does not exist", path.display());
            }
            ensure_within_plugin_dir(plugin_dir, &path, "process entrypoint", command)?;
            ensure_executable_entrypoint(&path, "process")?;
            ensure_selected_file(package_files, command, "process entrypoint")?;
            if let Some(cwd) = cwd {
                ensure_entrypoint_cwd(plugin_dir, cwd, package_files, "process entrypoint.cwd")?;
            }
            validate_runtime_temp_environment(env, "process")?;
        }
        (AdapterKind::Http, PluginEntrypoint::Http { base_url, ca_cert }) => {
            if base_url.trim().is_empty() {
                bail!("http entrypoint.base_url cannot be empty");
            }
            let url = url::Url::parse(base_url.trim())
                .with_context(|| "http entrypoint.base_url must be a valid URL")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("http entrypoint.base_url must use http:// or https://");
            }
            if url.host_str().is_none() {
                bail!("http entrypoint.base_url must include a host");
            }
            if !url.username().is_empty() || url.password().is_some() {
                bail!("http entrypoint.base_url must not contain userinfo");
            }
            if let Some(ca_cert) = ca_cert {
                ensure_relative_file(plugin_dir, ca_cert)?;
                ensure_selected_file(package_files, ca_cert, "http entrypoint.ca_cert")?;
            }
        }
        (
            AdapterKind::Mcp,
            PluginEntrypoint::Mcp {
                command, cwd, env, ..
            },
        ) => {
            if command.trim().is_empty() {
                bail!("mcp entrypoint.command cannot be empty");
            }
            let path = plugin_dir.join(command);
            if !path.exists() {
                bail!("mcp entrypoint '{}' does not exist", path.display());
            }
            ensure_within_plugin_dir(plugin_dir, &path, "mcp entrypoint", command)?;
            ensure_executable_entrypoint(&path, "mcp")?;
            ensure_selected_file(package_files, command, "mcp entrypoint")?;
            if let Some(cwd) = cwd {
                ensure_entrypoint_cwd(plugin_dir, cwd, package_files, "mcp entrypoint.cwd")?;
            }
            validate_runtime_temp_environment(env, "mcp")?;
        }
        _ => {
            bail!(
                "plugin adapter '{}' does not match entrypoint type",
                manifest.adapter.as_str()
            );
        }
    }
    Ok(())
}

fn validate_runtime_temp_environment(
    env: &std::collections::BTreeMap<String, String>,
    adapter: &str,
) -> Result<()> {
    for (name, value) in env {
        runtime_temp_relative_path(value).with_context(|| {
            format!("invalid {adapter} entrypoint.env.{name} runtime temporary declaration")
        })?;
    }
    Ok(())
}

fn ensure_entrypoint_cwd(
    plugin_dir: &Path,
    cwd: &str,
    package_files: &BTreeSet<String>,
    label: &str,
) -> Result<()> {
    ensure_relative_dir_reference(plugin_dir, cwd, label)?;
    if cwd == "." {
        return Ok(());
    }
    validate_package_path(cwd)?;
    let cwd_path = Path::new(cwd);
    let cwd_components: Vec<_> = cwd_path.components().collect();
    if package_files.iter().any(|file| {
        let components: Vec<_> = Path::new(file).components().collect();
        components.len() > cwd_components.len() && components.starts_with(&cwd_components)
    }) {
        return Ok(());
    }
    bail!(
        "{label} '{cwd}' is not the package root or a directory created by a selected package file"
    );
}

#[cfg(unix)]
fn ensure_executable_entrypoint(path: &Path, adapter_label: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "failed reading {} entrypoint metadata: {}",
            adapter_label,
            path.display()
        )
    })?;
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "{} entrypoint '{}' is not executable",
            adapter_label,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable_entrypoint(_path: &Path, _adapter_label: &str) -> Result<()> {
    Ok(())
}
