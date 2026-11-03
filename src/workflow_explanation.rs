// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use transist_compiler::{
    capture_workflow_definition,
    explanation::{WorkflowExplanation, compare_explanations, explain_snapshot},
};
use transist_plugins::ValidatedPluginCatalog;
use transist_protocol::KernelValue;

/// Selects an optional local catalog home without initialization or runtime posture.
/// Follows explicit path, environment, user preference, then per-user default.
/// Missing OS user-directory variables leave the catalog unresolved, not created.
pub fn resolve_workflow_explanation_home(
    cwd: &Path,
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let environment = std::env::var_os("TRANSIST_HOME").map(PathBuf::from);
    if let Some(path) = explicit.or_else(|| {
        environment
            .as_deref()
            .filter(|path| !path.as_os_str().is_empty())
    }) {
        return Ok(Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }));
    }
    let preference = if crate::user_settings_path().is_ok() {
        crate::load_user_home_path()?
    } else {
        None
    };
    Ok(preference.or_else(crate::per_user_default_home))
}

/// Explains authored source and optionally compares a previous definition using
/// the same read-only local contract snapshot and ordinary prospective inputs.
/// Does not require or initialize a runtime home, evaluate policy/trust, look up
/// secrets, probe prerequisites, download schemas, or invoke any adapter.
pub fn explain_workflow_file(
    home: Option<&Path>,
    path: &Path,
    inputs: &BTreeMap<String, KernelValue>,
    compare_to: Option<&Path>,
) -> Result<WorkflowExplanation> {
    let snapshot = capture_workflow_definition(path)?;
    let previous = compare_to.map(capture_workflow_definition).transpose()?;
    let catalog = home.map(|home| ValidatedPluginCatalog::new(home.to_path_buf(), BTreeMap::new()));
    let mut current = explain_snapshot(&snapshot, catalog.as_ref(), inputs)?;
    if let Some(previous) = previous {
        current.comparison = Some(compare_explanations(
            &explain_snapshot(&previous, catalog.as_ref(), inputs)?,
            &current,
        )?);
    }
    Ok(current)
}

/// Formats an application explanation DTO as operator-facing static-authoring text.
/// Takes no runtime context and performs no I/O, validation, or execution.
pub fn render_workflow_explanation(explanation: &WorkflowExplanation) -> Result<String> {
    transist_compiler::explanation::render_explanation_text(explanation)
}
