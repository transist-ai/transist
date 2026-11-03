// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use transist::KernelValue;

use crate::DISPLAY_REASON_MAX_CHARS;
use crate::actionable::ActionableRun;
use crate::terminal::current_terminal_columns;

pub(crate) fn render_gate_command(
    command: &str,
    run_id: &str,
    gate_id: &Option<String>,
    actor: Option<&str>,
    reason: Option<&str>,
) -> String {
    let mut rendered = format!("transist runs gate {} {}", command, run_id);
    if let Some(gate_id) = gate_id.as_deref() {
        rendered.push(' ');
        rendered.push_str(gate_id);
    }
    rendered.push_str(" --actor ");
    rendered.push_str(&render_display_reason(
        actor.unwrap_or("Reviewer <reviewer@example.com>"),
    ));
    if let Some(reason) = reason {
        rendered.push_str(" --reason ");
        rendered.push_str(&render_display_reason(reason));
    }
    rendered
}

fn render_display_reason(reason: &str) -> String {
    let reason = truncate_display_reason(reason, display_reason_limit());
    serde_json::to_string(&reason).unwrap_or_else(|_| "\"\"".to_string())
}

pub(crate) fn display_reason_limit() -> usize {
    let terminal_budget = current_terminal_columns().saturating_sub(32).max(20);
    DISPLAY_REASON_MAX_CHARS.min(terminal_budget)
}

fn truncate_display_reason(reason: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(8);
    let char_count = reason.chars().count();
    if char_count <= max_chars {
        return reason.to_string();
    }

    let keep = max_chars.saturating_sub(3).max(1);
    let mut truncated = reason.chars().take(keep).collect::<String>();
    truncated.push_str("...");
    truncated
}

pub(crate) fn collect_manual_gate_guidance_lines(
    context: &transist::TransistContext,
    root_run_id: &str,
    actionable: &ActionableRun,
    gate: &transist::InspectionGate,
) -> anyhow::Result<Vec<String>> {
    let Some(KernelValue::Object(guidance)) = gate.request_summary.get("operator_guidance") else {
        return Ok(Vec::new());
    };

    let mut lines = Vec::new();
    let condition_lines =
        render_gate_condition_guidance(guidance, &actionable.detail.workflow_inputs);
    if !condition_lines.is_empty() {
        lines.push("Conditions:".to_string());
        for line in condition_lines {
            lines.push(format!("  {}", line));
        }
    }

    let decision_lines =
        render_gate_decision_guidance(guidance, &actionable.detail.workflow_inputs);
    if !decision_lines.is_empty() {
        lines.push("Decision guide:".to_string());
        for (decision, description) in decision_lines {
            lines.push(format!("  {}: {}", decision, description));
        }
    }

    if let Some((label, paths)) =
        render_gate_inspect_guidance(context, root_run_id, actionable, guidance)
    {
        lines.push(label);
        for path in paths {
            lines.push(format!("  {}", shell_quote(&path)));
        }
    }

    Ok(lines)
}

fn render_gate_condition_guidance(
    guidance: &BTreeMap<String, KernelValue>,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Vec<String> {
    let Some(value) = guidance.get("conditions") else {
        return Vec::new();
    };

    match value {
        KernelValue::String(_)
        | KernelValue::Array(_)
        | KernelValue::Bool(_)
        | KernelValue::Number(_) => render_guidance_text(value, workflow_inputs)
            .map(|text| vec![text])
            .unwrap_or_default(),
        KernelValue::Object(entries) => {
            let mut lines = Vec::new();
            if let Some(label) = entries
                .get("label")
                .and_then(|value| render_guidance_text(value, workflow_inputs))
            {
                lines.push(label);
            }
            if let Some(items) = entries.get("items") {
                match items {
                    KernelValue::Array(values) => {
                        lines.extend(
                            values
                                .iter()
                                .filter_map(|value| render_guidance_text(value, workflow_inputs)),
                        );
                    }
                    other => {
                        if let Some(text) = render_guidance_text(other, workflow_inputs) {
                            lines.push(text);
                        }
                    }
                }
            }
            lines
        }
        KernelValue::Null => Vec::new(),
    }
}

fn render_gate_decision_guidance(
    guidance: &BTreeMap<String, KernelValue>,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Vec<(String, String)> {
    let Some(KernelValue::Object(decisions)) = guidance.get("decisions") else {
        return Vec::new();
    };

    let ordered = ["approve", "request_changes", "reject"];
    let mut rendered = Vec::new();
    let mut seen = BTreeSet::new();

    for key in ordered {
        if let Some(text) = render_named_decision_guidance(decisions, key, workflow_inputs) {
            rendered.push((display_gate_decision_name(key).to_string(), text));
            seen.insert(normalize_gate_decision_key(key).to_string());
        }
    }

    for (key, value) in decisions {
        let normalized = normalize_gate_decision_key(key);
        if !seen.insert(normalized.to_string()) {
            continue;
        }
        if let Some(text) = render_guidance_text(value, workflow_inputs) {
            rendered.push((display_gate_decision_name(key).to_string(), text));
        }
    }

    rendered
}

fn render_named_decision_guidance(
    decisions: &BTreeMap<String, KernelValue>,
    key: &str,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Option<String> {
    decisions
        .iter()
        .find(|(candidate, _)| normalize_gate_decision_key(candidate) == key)
        .and_then(|(_, value)| render_guidance_text(value, workflow_inputs))
}

fn render_guidance_text(
    value: &KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Option<String> {
    match value {
        KernelValue::String(raw) => {
            let rendered = render_guidance_string(raw, workflow_inputs);
            let trimmed = rendered.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        KernelValue::Array(values) => {
            let rendered = values
                .iter()
                .filter_map(|value| render_guidance_text(value, workflow_inputs))
                .collect::<Vec<_>>();
            (!rendered.is_empty()).then(|| rendered.join(" "))
        }
        KernelValue::Bool(value) => Some(value.to_string()),
        KernelValue::Number(value) => Some(value.to_string()),
        KernelValue::Null | KernelValue::Object(_) => None,
    }
}

fn render_guidance_string(raw: &str, workflow_inputs: &BTreeMap<String, KernelValue>) -> String {
    if !raw.contains("{{inputs.") {
        return raw.to_string();
    }

    let mut rendered = String::new();
    let mut rest = raw;
    while let Some(start_idx) = rest.find("{{inputs.") {
        rendered.push_str(&rest[..start_idx]);
        let after = &rest[start_idx + "{{inputs.".len()..];
        let Some(end_idx) = after.find("}}") else {
            rendered.push_str(&rest[start_idx..]);
            return rendered;
        };
        let key = after[..end_idx].trim();
        let value = workflow_inputs.get(key).unwrap_or(&KernelValue::Null);
        rendered.push_str(&stringify_guidance_value(value));
        rest = &after[end_idx + 2..];
    }
    rendered.push_str(rest);
    rendered
}

fn stringify_guidance_value(value: &KernelValue) -> String {
    match value {
        KernelValue::Null => "null".to_string(),
        KernelValue::Bool(value) => value.to_string(),
        KernelValue::Number(value) => value.to_string(),
        KernelValue::String(value) => value.clone(),
        KernelValue::Array(_) | KernelValue::Object(_) => {
            serde_json::to_string(&value.clone().to_json()).unwrap_or_else(|_| "null".to_string())
        }
    }
}

fn render_gate_inspect_guidance(
    context: &transist::TransistContext,
    root_run_id: &str,
    actionable: &ActionableRun,
    guidance: &BTreeMap<String, KernelValue>,
) -> Option<(String, Vec<String>)> {
    let Some(KernelValue::Object(inspect)) = guidance.get("inspect") else {
        return None;
    };
    let Some(KernelValue::Array(raw_paths)) = inspect.get("paths") else {
        return None;
    };

    let label = inspect
        .get("label")
        .and_then(|value| render_guidance_text(value, &actionable.detail.workflow_inputs))
        .unwrap_or_else(|| "Inspect:".to_string());
    let repo_path = workflow_input_string(&actionable.detail.workflow_inputs, "repo_path");
    let candidate_run_roots = [root_run_id, actionable.run_id.as_str()]
        .into_iter()
        .filter_map(|run_id| context.run_root(run_id).ok())
        .map(|run_root| run_root.display().to_string())
        .collect::<Vec<_>>();
    let mut resolved_paths = Vec::new();

    for raw_path in raw_paths
        .iter()
        .filter_map(|value| render_guidance_text(value, &actionable.detail.workflow_inputs))
    {
        if let Some(path) = resolve_existing_guidance_path(
            &raw_path,
            &candidate_run_roots,
            context.repo_root().to_str(),
            repo_path.as_deref(),
        ) {
            if !resolved_paths.contains(&path) {
                resolved_paths.push(path);
            }
        }
    }

    (!resolved_paths.is_empty()).then_some((label, resolved_paths))
}

pub(crate) fn resolve_existing_guidance_path(
    raw_path: &str,
    run_roots: &[String],
    workspace_dir: Option<&str>,
    repo_path: Option<&str>,
) -> Option<String> {
    if std::path::Path::new(raw_path).is_absolute() {
        return std::path::Path::new(raw_path)
            .exists()
            .then(|| raw_path.to_string());
    }

    for run_root in run_roots {
        let resolved = resolve_inspect_path(raw_path, Some(run_root), workspace_dir, repo_path);
        if std::path::Path::new(&resolved).exists() {
            return Some(resolved);
        }
    }
    None
}

fn normalize_gate_decision_key(raw: &str) -> &str {
    match raw {
        "request-changes" => "request_changes",
        other => other,
    }
}

fn display_gate_decision_name(raw: &str) -> &str {
    match normalize_gate_decision_key(raw) {
        "request_changes" => "request-changes",
        other => other,
    }
}

pub(crate) fn workflow_input_string(
    inputs: &BTreeMap<String, KernelValue>,
    key: &str,
) -> Option<String> {
    match inputs.get(key) {
        Some(KernelValue::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => None,
    }
}

pub(crate) fn resolve_inspect_path(
    raw_path: &str,
    run_root: Option<&str>,
    workspace_dir: Option<&str>,
    repo_path: Option<&str>,
) -> String {
    if raw_path.is_empty() {
        return String::new();
    }
    let candidate = raw_path.trim();
    if std::path::Path::new(candidate).is_absolute() {
        return candidate.to_string();
    }
    if let Some(rest) = candidate.strip_prefix("run://") {
        if let Some(root) = run_root.and_then(root_run_dir_from_path) {
            return root
                .join(rest.trim_start_matches('/'))
                .display()
                .to_string();
        }
    }
    if let Some(rest) = candidate.strip_prefix("repo://") {
        if let Some(root) = resolve_repo_root_path(workspace_dir, repo_path) {
            return root
                .join(".transist")
                .join(rest.trim_start_matches('/'))
                .display()
                .to_string();
        }
    }
    workspace_dir
        .map(|root| {
            std::path::Path::new(root)
                .join(candidate)
                .display()
                .to_string()
        })
        .unwrap_or_else(|| candidate.to_string())
}

fn root_run_dir_from_path(run_root: &str) -> Option<std::path::PathBuf> {
    std::path::Path::new(run_root)
        .ancestors()
        .find(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with("run_"))
                && candidate.parent().and_then(std::path::Path::file_name)
                    == Some(std::ffi::OsStr::new("runs"))
        })
        .map(std::path::Path::to_path_buf)
}

fn resolve_repo_root_path(
    workspace_dir: Option<&str>,
    repo_path: Option<&str>,
) -> Option<std::path::PathBuf> {
    let raw = repo_path?.trim();
    if raw.is_empty() {
        return None;
    }
    let path = std::path::Path::new(raw);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    workspace_dir.map(|root| std::path::Path::new(root).join(path))
}

pub(crate) fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

pub(crate) fn blocked_without_gate_guidance(root_run_id: &str) -> Vec<String> {
    vec![
        "Outcome: the run is blocked before the actionable subflow reached a manual gate."
            .to_string(),
        "No approve/request-changes/reject action is available yet.".to_string(),
        "Continue execution without recording another decision:".to_string(),
        format!("  transist runs start {}", root_run_id),
        "This command just keeps the run moving until the next gate or terminal state.".to_string(),
    ]
}
