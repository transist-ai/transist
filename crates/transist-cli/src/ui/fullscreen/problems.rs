// SPDX-License-Identifier: MPL-2.0
//! Read-only, derived runtime problems for the fullscreen operator console.

use std::path::Path;

use anyhow::{Context, Result};

use super::diagnostics::{
    DiagnosticReport, DiagnosticSeverity, SessionDiagnostic, effective_persisted_run_code,
    persisted_lease_issue_evidence, persisted_run_diagnostic_for_actionable,
};
use super::{FullscreenPrompt, launchable_workflow_refs};

#[cfg(test)]
const UI_OWNED_FALLBACK_CODES: [&str; 9] = [
    "tui.home_uninitialized",
    "tui.runtime_config_invalid",
    "tui.policy_invalid",
    "tui.plugin_inventory_failed",
    "tui.workflow_inventory_failed",
    "tui.no_launchable_workflows",
    "tui.operation_failed",
    "run.failed",
    "run.blocked",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProblemSeverity {
    Error,
    Attention,
}

impl ProblemSeverity {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Attention => "attention",
        }
    }

    fn diagnostic_severity(self) -> DiagnosticSeverity {
        match self {
            Self::Error => DiagnosticSeverity::Error,
            Self::Attention => DiagnosticSeverity::Attention,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProblemCategory {
    RuntimeConfiguration,
    PluginResolution,
    WorkflowResolution,
    Capability,
    Policy,
    Trust,
    PersistedPreflight,
    Cleanup,
    FailedRoot,
    BlockedGate,
    NestedFailure,
    Lease,
    RunInspection,
    Session,
}

impl ProblemCategory {
    fn label(self) -> &'static str {
        match self {
            Self::RuntimeConfiguration => "runtime configuration",
            Self::PluginResolution => "plugin resolution",
            Self::WorkflowResolution => "workflow resolution",
            Self::Capability => "capability",
            Self::Policy => "policy",
            Self::Trust => "trust",
            Self::PersistedPreflight => "persisted preflight",
            Self::Cleanup => "cleanup",
            Self::FailedRoot => "failed root",
            Self::BlockedGate => "blocked gate",
            Self::NestedFailure => "nested failure",
            Self::Lease => "lease",
            Self::RunInspection => "run inspection",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone)]
enum ProblemSource {
    Home,
    RuntimeConfiguration(DiagnosticReport),
    Policy(DiagnosticReport),
    PluginInventory(DiagnosticReport),
    WorkflowInventory(DiagnosticReport),
    NoLaunchableWorkflows,
    Run {
        root_run_id: String,
        actionable_run_id: String,
        report: DiagnosticReport,
    },
    RunInspection(DiagnosticReport),
    Session(DiagnosticReport),
}

/// One derived, non-persisted operator problem.
#[derive(Debug, Clone)]
pub(super) struct ProblemItem {
    severity: ProblemSeverity,
    category: ProblemCategory,
    resource: String,
    code: String,
    summary: String,
    timestamp_ms: Option<u64>,
    source: ProblemSource,
}

impl ProblemItem {
    pub(super) fn list_label(&self) -> String {
        let timestamp = self
            .timestamp_ms
            .map(super::format_unix_ms_iso_utc)
            .map(|value| format!("  {value}"))
            .unwrap_or_default();
        format!(
            "[{:9}] {:20} {}: {} ({}){}",
            self.severity.label(),
            self.category.label(),
            self.resource,
            self.summary,
            self.code,
            timestamp
        )
    }

    fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Severity: {}", self.severity.label()),
            format!("Category: {}", self.category.label()),
            format!("Code: {}", self.code),
            format!("Resource: {}", self.resource),
            format!("Summary: {}", self.summary),
        ];
        if let Some(timestamp_ms) = self.timestamp_ms {
            lines.push(format!(
                "Observed: {}",
                super::format_unix_ms_iso_utc(timestamp_ms)
            ));
        }
        if let ProblemSource::Run {
            root_run_id,
            actionable_run_id,
            ..
        } = &self.source
        {
            lines.push(format!("Root run ID: {root_run_id}"));
            lines.push(format!("Actionable run ID: {actionable_run_id}"));
        }
        lines
    }

    fn build_report(&self, _context: &transist::TransistContext) -> Result<DiagnosticReport> {
        match &self.source {
            ProblemSource::PluginInventory(report)
            | ProblemSource::WorkflowInventory(report)
            | ProblemSource::RunInspection(report)
            | ProblemSource::Session(report) => Ok(report.clone()),
            ProblemSource::Run { report, .. } => Ok(report.clone()),
            ProblemSource::Home => Ok(DiagnosticReport {
                severity: self.severity.diagnostic_severity(),
                error_code: Some(self.code.clone()),
                resource_kind: "home".to_string(),
                resource_identifier: self.resource.clone(),
                summary: self.summary.clone(),
                outcome: None,
                causes: Vec::new(),
                details: Vec::new(),
                next_actions: vec![
                    "Initialize the effective home before using runtime operations.".to_string(),
                ],
                commands: vec!["transist home init".to_string()],
            }),
            ProblemSource::RuntimeConfiguration(report) | ProblemSource::Policy(report) => {
                Ok(report.clone())
            }
            ProblemSource::NoLaunchableWorkflows => Ok(DiagnosticReport {
                severity: self.severity.diagnostic_severity(),
                error_code: Some(self.code.clone()),
                resource_kind: "workflow inventory".to_string(),
                resource_identifier: self.resource.clone(),
                summary: self.summary.clone(),
                outcome: None,
                causes: Vec::new(),
                details: Vec::new(),
                next_actions: vec![
                    "Install or promote a workflow with a compiled recipe, then refresh."
                        .to_string(),
                ],
                commands: Vec::new(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProblemsViewState {
    pub(super) items: Vec<ProblemItem>,
    pub(super) selected: usize,
    pub(super) status_line: String,
}

impl ProblemsViewState {
    fn new(items: Vec<ProblemItem>) -> Self {
        let status_line = if items.is_empty() {
            "No detected problems.".to_string()
        } else {
            "Cause-oriented problems derived from local state. Generic failures remain in Runs."
                .to_string()
        };
        Self {
            items,
            selected: 0,
            status_line,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta)
            .clamp(0, self.items.len().saturating_sub(1) as isize) as usize;
    }

    pub(super) fn selected_detail_lines(&self) -> Vec<String> {
        self.items
            .get(self.selected)
            .map(ProblemItem::detail_lines)
            .unwrap_or_else(|| vec!["No detected problems.".to_string()])
    }
}

fn problems_state_from_dashboard(
    dashboard: &super::DashboardData,
    session_error: Option<&SessionDiagnostic>,
) -> ProblemsViewState {
    ProblemsViewState::new(collect_problems(
        dashboard.initialized,
        dashboard.context.home_dir(),
        &dashboard.run_problem_items,
        dashboard.plugin_inventory_diagnostic.as_ref(),
        dashboard.workflow_inventory_diagnostic.as_ref(),
        &dashboard.workflows,
        session_error,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn problem_count(
    initialized: bool,
    home_dir: &Path,
    run_problem_items: &[ProblemItem],
    plugin_inventory_diagnostic: Option<&DiagnosticReport>,
    workflow_inventory_diagnostic: Option<&DiagnosticReport>,
    workflows: &[transist::InspectionWorkflowSummary],
    session_error: Option<&SessionDiagnostic>,
) -> usize {
    collect_problems(
        initialized,
        home_dir,
        run_problem_items,
        plugin_inventory_diagnostic,
        workflow_inventory_diagnostic,
        workflows,
        session_error,
    )
    .len()
}

#[allow(clippy::too_many_arguments)]
fn collect_problems(
    initialized: bool,
    home_dir: &Path,
    run_problem_items: &[ProblemItem],
    plugin_inventory_diagnostic: Option<&DiagnosticReport>,
    workflow_inventory_diagnostic: Option<&DiagnosticReport>,
    workflows: &[transist::InspectionWorkflowSummary],
    session_error: Option<&SessionDiagnostic>,
) -> Vec<ProblemItem> {
    let mut items = Vec::new();
    if !initialized {
        items.push(ProblemItem {
            severity: ProblemSeverity::Error,
            category: ProblemCategory::RuntimeConfiguration,
            resource: home_dir.display().to_string(),
            code: "tui.home_uninitialized".to_string(),
            summary: "The effective home is not initialized.".to_string(),
            timestamp_ms: None,
            source: ProblemSource::Home,
        });
    } else if let Err((error, policy_error)) = validate_composed_configuration(home_dir) {
        let code = if policy_error {
            "tui.policy_invalid"
        } else {
            "tui.runtime_config_invalid"
        };
        let mut report = DiagnosticReport::from_error(
            DiagnosticSeverity::Error,
            Some(code.to_string()),
            "runtime configuration",
            home_dir.display().to_string(),
            if policy_error {
                "The fully composed runtime policy is invalid."
            } else {
                "The runtime configuration is invalid."
            },
            None,
            &error,
        );
        report.next_actions.push(
            "Inspect the effective configuration and correct the reported local source."
                .to_string(),
        );
        report.commands.push("transist policy validate".to_string());
        if policy_error {
            report
                .commands
                .push("transist policy show --effective".to_string());
        }
        items.push(ProblemItem {
            severity: ProblemSeverity::Error,
            category: if policy_error {
                ProblemCategory::Policy
            } else {
                ProblemCategory::RuntimeConfiguration
            },
            resource: home_dir.display().to_string(),
            code: code.to_string(),
            summary: report.summary.clone(),
            timestamp_ms: None,
            source: if policy_error {
                ProblemSource::Policy(report)
            } else {
                ProblemSource::RuntimeConfiguration(report)
            },
        });
    }
    if let Some(report) = plugin_inventory_diagnostic {
        items.push(problem_from_report(
            report.clone(),
            ProblemSource::PluginInventory(report.clone()),
            ProblemCategory::PluginResolution,
            None,
        ));
    }
    if let Some(report) = workflow_inventory_diagnostic {
        items.push(problem_from_report(
            report.clone(),
            ProblemSource::WorkflowInventory(report.clone()),
            ProblemCategory::WorkflowResolution,
            None,
        ));
    }
    if initialized
        && workflow_inventory_diagnostic.is_none()
        && launchable_workflow_refs(workflows).is_empty()
    {
        items.push(ProblemItem {
            severity: ProblemSeverity::Error,
            category: ProblemCategory::WorkflowResolution,
            resource: home_dir.display().to_string(),
            code: "tui.no_launchable_workflows".to_string(),
            summary: "No runnable workflow is installed.".to_string(),
            timestamp_ms: None,
            source: ProblemSource::NoLaunchableWorkflows,
        });
    }

    items.extend_from_slice(run_problem_items);
    if let Some(report) = session_error {
        items.push(problem_from_report(
            report.report.clone(),
            ProblemSource::Session(report.report.clone()),
            ProblemCategory::Session,
            Some(report.captured_at_ms),
        ));
    }
    items.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| match (left.timestamp_ms, right.timestamp_ms) {
                (Some(left), Some(right)) => right.cmp(&left),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| left.resource.cmp(&right.resource))
            .then_with(|| left.code.cmp(&right.code))
    });
    items
}

fn validate_composed_configuration(
    home_dir: &Path,
) -> std::result::Result<(), (anyhow::Error, bool)> {
    if let Err(error) = transist::validate_runtime_configuration(home_dir) {
        let is_policy = format!("{error:#}").to_ascii_lowercase().contains("policy");
        return Err((error, is_policy));
    }
    transist::validate_effective_runtime_policy(home_dir).map_err(|error| (error, true))
}

pub(super) fn collect_run_problems(
    context: &transist::TransistContext,
    summaries: &[transist::InspectionRunSummary],
) -> Vec<ProblemItem> {
    let inspection = context.inspection();
    collect_run_problems_with(summaries, |run_id| inspection.get_run(run_id))
}

fn collect_run_problems_with(
    summaries: &[transist::InspectionRunSummary],
    mut load: impl FnMut(&str) -> Result<Option<transist::InspectionRunDetail>>,
) -> Vec<ProblemItem> {
    let mut by_root = std::collections::BTreeMap::new();
    for summary in summaries {
        if !matches!(summary.status.as_str(), "failed" | "blocked") {
            continue;
        }
        by_root
            .entry(summary.root_run_id.clone())
            .and_modify(|current: &mut &transist::InspectionRunSummary| {
                let candidate_key = (
                    summary.run_id == summary.root_run_id,
                    summary.updated_at_ms,
                    std::cmp::Reverse(summary.run_id.as_str()),
                );
                let current_key = (
                    current.run_id == current.root_run_id,
                    current.updated_at_ms,
                    std::cmp::Reverse(current.run_id.as_str()),
                );
                if candidate_key > current_key {
                    *current = summary;
                }
            })
            .or_insert(summary);
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    for (root_run_id, summary) in by_root {
        let root = match load(&root_run_id)
            .and_then(|detail| detail.with_context(|| format!("root run {root_run_id} is missing")))
        {
            Ok(detail) => detail,
            Err(error) => {
                items.push(run_inspection_problem(summary, &root_run_id, &error));
                continue;
            }
        };
        let actionable_run_id = root
            .actionable_run_id
            .clone()
            .unwrap_or_else(|| root.summary.run_id.clone());
        let actionable = if actionable_run_id == root.summary.run_id {
            root.clone()
        } else {
            match load(&actionable_run_id).and_then(|detail| {
                detail.with_context(|| format!("actionable run {actionable_run_id} is missing"))
            }) {
                Ok(detail) => detail,
                Err(error) => {
                    items.push(run_inspection_problem(summary, &root_run_id, &error));
                    continue;
                }
            }
        };
        let Some((severity, category, fallback_code, summary_text, timestamp_ms)) =
            actionable_persisted_problem(&root, &actionable)
        else {
            continue;
        };
        let persisted_code = effective_persisted_run_code(&root, &actionable);
        let code = persisted_code
            .clone()
            .unwrap_or_else(|| fallback_code.to_string());
        if !seen.insert((root_run_id.clone(), code.clone())) {
            continue;
        }
        let summary_text = summary_text.to_string();
        let report = persisted_run_diagnostic_for_actionable(
            &root,
            &actionable,
            severity.diagnostic_severity(),
            Some(fallback_code.to_string()),
            &summary_text,
        );
        items.push(ProblemItem {
            severity,
            category,
            resource: format!("run {actionable_run_id}"),
            code,
            summary: summary_text,
            timestamp_ms: Some(timestamp_ms),
            source: ProblemSource::Run {
                root_run_id,
                actionable_run_id,
                report,
            },
        });
    }
    items
}

fn run_inspection_problem(
    summary: &transist::InspectionRunSummary,
    root_run_id: &str,
    error: &anyhow::Error,
) -> ProblemItem {
    let mut report = DiagnosticReport::from_error(
        DiagnosticSeverity::Error,
        Some("tui.operation_failed".to_string()),
        "run",
        root_run_id,
        "Persisted run detail is unreadable and requires operator inspection.",
        Some("The dashboard omitted cause classification for this run.".to_string()),
        error,
    );
    report
        .commands
        .push(format!("transist runs show {root_run_id}"));
    ProblemItem {
        severity: ProblemSeverity::Error,
        category: ProblemCategory::RunInspection,
        resource: format!("run {root_run_id}"),
        code: "tui.operation_failed".to_string(),
        summary: report.summary.clone(),
        timestamp_ms: Some(summary.updated_at_ms),
        source: ProblemSource::RunInspection(report),
    }
}

fn actionable_persisted_problem(
    root: &transist::InspectionRunDetail,
    actionable: &transist::InspectionRunDetail,
) -> Option<(
    ProblemSeverity,
    ProblemCategory,
    &'static str,
    &'static str,
    u64,
)> {
    let classification = classify_persisted_run_problem(root, actionable);
    let has_failed_step_code = actionable
        .steps
        .iter()
        .rev()
        .find(|step| step.status == "failed")
        .is_some_and(|_| effective_persisted_run_code(root, actionable).is_some());
    let actionable_cause = !actionable.blocked_gates.is_empty()
        || root.cleanup.as_ref().is_some_and(|cleanup| {
            cleanup.state == "failed"
                || cleanup
                    .preflight
                    .as_ref()
                    .is_some_and(|preflight| preflight.blocker_count > 0)
                || cleanup.steps.iter().any(|step| step.status == "failed")
        })
        || actionable
            .preflight
            .as_ref()
            .is_some_and(|preflight| preflight.blocker_count > 0)
        || root
            .trust
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == "denied")
        || root
            .trust
            .policy_decision
            .as_ref()
            .is_some_and(|decision| !decision.allowed)
        || persisted_lease_issue_evidence(root, actionable)
        || actionable.summary.run_id != root.summary.run_id
        || classification.1 != ProblemCategory::FailedRoot
        || has_failed_step_code;
    actionable_cause.then_some(classification)
}

#[cfg(test)]
fn build_selected_run_report(
    root: &transist::InspectionRunDetail,
    actionable: &transist::InspectionRunDetail,
) -> DiagnosticReport {
    let (severity, _, fallback_code, summary, _) = classify_persisted_run_problem(root, actionable);
    persisted_run_diagnostic_for_actionable(
        root,
        actionable,
        severity.diagnostic_severity(),
        Some(fallback_code.to_string()),
        summary,
    )
}

fn classify_persisted_run_problem(
    root: &transist::InspectionRunDetail,
    actionable: &transist::InspectionRunDetail,
) -> (
    ProblemSeverity,
    ProblemCategory,
    &'static str,
    &'static str,
    u64,
) {
    if !actionable.blocked_gates.is_empty() {
        return (
            ProblemSeverity::Attention,
            ProblemCategory::BlockedGate,
            "run.blocked",
            "Manual gate is blocked and needs an operator decision.",
            actionable.summary.updated_at_ms,
        );
    }
    if let Some(cleanup) = root.cleanup.as_ref().filter(|cleanup| {
        cleanup.state == "failed"
            || cleanup
                .preflight
                .as_ref()
                .is_some_and(|preflight| preflight.blocker_count > 0)
            || cleanup.steps.iter().any(|step| step.status == "failed")
    }) {
        return (
            ProblemSeverity::Error,
            ProblemCategory::Cleanup,
            "run.failed",
            "Persisted cleanup failed or is blocked.",
            cleanup
                .finished_at_ms
                .or(cleanup.started_at_ms)
                .or_else(|| cleanup.preflight.as_ref().map(|value| value.checked_at_ms))
                .unwrap_or(root.summary.updated_at_ms),
        );
    }
    if let Some(preflight) = actionable
        .preflight
        .as_ref()
        .filter(|preflight| preflight.blocker_count > 0)
    {
        let evidence = preflight
            .failed_checks
            .iter()
            .flat_map(|check| [check.kind.as_str(), check.message.as_str()])
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        return (
            ProblemSeverity::Error,
            classify_preflight_category(&evidence),
            "run.failed",
            "Persisted preflight has blocking checks.",
            preflight.checked_at_ms,
        );
    }
    if root
        .trust
        .decision
        .as_ref()
        .is_some_and(|decision| decision.outcome == "denied")
    {
        return (
            ProblemSeverity::Error,
            ProblemCategory::Trust,
            "run.failed",
            "Persisted trust verification denied the run.",
            root.summary.updated_at_ms,
        );
    }
    if root
        .trust
        .policy_decision
        .as_ref()
        .is_some_and(|decision| !decision.allowed)
    {
        return (
            ProblemSeverity::Error,
            ProblemCategory::Policy,
            "run.failed",
            "Persisted policy denied the run.",
            root.summary.updated_at_ms,
        );
    }
    if persisted_lease_issue_evidence(root, actionable) {
        return (
            ProblemSeverity::Error,
            ProblemCategory::Lease,
            "run.failed",
            "Persisted failure identifies a lease conflict or stale owner.",
            actionable.summary.updated_at_ms,
        );
    }
    if actionable.summary.run_id != root.summary.run_id {
        return (
            ProblemSeverity::Error,
            ProblemCategory::NestedFailure,
            "run.failed",
            "A nested run failed.",
            actionable.summary.updated_at_ms,
        );
    }
    let code_and_message = actionable
        .steps
        .iter()
        .rev()
        .find(|step| step.status == "failed")
        .map(|step| {
            format!(
                "{} {}",
                effective_persisted_run_code(root, actionable).unwrap_or_default(),
                step.output_summary
                    .error_message
                    .as_deref()
                    .unwrap_or_default()
            )
            .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let category = classify_failure_category(&code_and_message);
    let summary = match category {
        ProblemCategory::Capability => {
            "Persisted capability or authority failure requires attention."
        }
        ProblemCategory::Policy => "Persisted policy failure requires attention.",
        ProblemCategory::PluginResolution => {
            "Persisted plugin or action failure requires attention."
        }
        ProblemCategory::WorkflowResolution => {
            "Persisted workflow or subflow failure requires attention."
        }
        ProblemCategory::Lease => "Persisted lease failure requires attention.",
        _ if root.summary.status == "blocked" => "Root run is blocked before dispatch or recovery.",
        _ => "Run recorded a specific persisted failure.",
    };
    (
        ProblemSeverity::Error,
        category,
        if root.summary.status == "blocked" {
            "run.blocked"
        } else {
            "run.failed"
        },
        summary,
        actionable
            .steps
            .iter()
            .rev()
            .find(|step| step.status == "failed")
            .and_then(|step| step.finished_at_ms.or(step.started_at_ms))
            .unwrap_or(root.summary.updated_at_ms),
    )
}

fn classify_preflight_category(evidence: &str) -> ProblemCategory {
    if is_lease_evidence(evidence) {
        ProblemCategory::Lease
    } else if evidence.contains("capability") || evidence.contains("authority") {
        ProblemCategory::Capability
    } else if evidence.contains("policy") {
        ProblemCategory::Policy
    } else if evidence.contains("plugin") || evidence.contains("action") {
        ProblemCategory::PluginResolution
    } else if evidence.contains("workflow") || evidence.contains("subflow") {
        ProblemCategory::WorkflowResolution
    } else {
        ProblemCategory::PersistedPreflight
    }
}

fn classify_failure_category(evidence: &str) -> ProblemCategory {
    if is_lease_evidence(evidence) {
        ProblemCategory::Lease
    } else if evidence.contains("capability") || evidence.contains("authority") {
        ProblemCategory::Capability
    } else if evidence.contains("policy") || evidence.contains("authorization") {
        ProblemCategory::Policy
    } else if evidence.contains("plugin") || evidence.contains("action") {
        ProblemCategory::PluginResolution
    } else if evidence.contains("workflow") || evidence.contains("subflow") {
        ProblemCategory::WorkflowResolution
    } else {
        ProblemCategory::FailedRoot
    }
}

fn is_lease_evidence(evidence: &str) -> bool {
    (evidence.contains("lease") && evidence.contains("conflict"))
        || evidence.contains("already leased")
        || (evidence.contains("lease") && evidence.contains("stale owner"))
        || (evidence.contains("lease") && evidence.contains("stale_owner"))
        || (evidence.contains("lease") && evidence.contains("orphaned owner"))
}

fn problem_from_report(
    report: DiagnosticReport,
    source: ProblemSource,
    category: ProblemCategory,
    timestamp_ms: Option<u64>,
) -> ProblemItem {
    ProblemItem {
        severity: match report.severity {
            DiagnosticSeverity::Error => ProblemSeverity::Error,
            DiagnosticSeverity::Attention => ProblemSeverity::Attention,
        },
        category,
        resource: format!("{} {}", report.resource_kind, report.resource_identifier),
        code: report
            .error_code
            .clone()
            .unwrap_or_else(|| "tui.operation_failed".to_string()),
        summary: report.summary.clone(),
        timestamp_ms,
        source,
    }
}

fn handle_problems_refresh_result(
    dashboard: &mut super::DashboardData,
    session_error: &mut Option<SessionDiagnostic>,
    refreshed: Result<super::DashboardData>,
) -> String {
    match refreshed {
        Ok(refreshed) => {
            *dashboard = refreshed;
            "Persisted problems recomputed.".to_string()
        }
        Err(error) => {
            let report = DiagnosticReport::from_error(
                DiagnosticSeverity::Error,
                Some("tui.operation_failed".to_string()),
                "problems refresh",
                dashboard.context.home_dir().display().to_string(),
                "Unable to refresh Problems; the current list was retained.",
                None,
                &error,
            );
            *session_error = Some(SessionDiagnostic::capture(report));
            "Refresh failed; retained the current Problems list.".to_string()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_problems_screen(
    prompt: &mut FullscreenPrompt,
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    initial_dashboard: &super::DashboardData,
    session_error: &mut Option<SessionDiagnostic>,
) -> Result<()> {
    let mut dashboard = initial_dashboard.clone();
    let mut state = problems_state_from_dashboard(&dashboard, session_error.as_ref());
    loop {
        prompt.dashboard_state = None;
        prompt.run_browser_state = None;
        prompt.problems_view_state = Some(state.clone());
        prompt.draw()?;
        match crossterm::event::read()? {
            crossterm::event::Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            crossterm::event::Event::Key(key)
                if key.kind == crossterm::event::KeyEventKind::Press =>
            {
                match key.code {
                    crossterm::event::KeyCode::Esc => {
                        prompt.problems_view_state = None;
                        return Ok(());
                    }
                    crossterm::event::KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        prompt.problems_view_state = None;
                        return Ok(());
                    }
                    crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                        state.move_selection(1)
                    }
                    crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                        state.move_selection(-1)
                    }
                    crossterm::event::KeyCode::Enter | crossterm::event::KeyCode::Char('o') => {
                        if let Some(item) = state.items.get(state.selected).cloned() {
                            let report = item.build_report(&dashboard.context)?;
                            super::diagnostics::show_diagnostic_report(
                                prompt,
                                "Problem diagnostic",
                                &report,
                            )?;
                        }
                    }
                    crossterm::event::KeyCode::Char('d') => {
                        if matches!(
                            state.items.get(state.selected).map(|item| &item.source),
                            Some(ProblemSource::Session(_))
                        ) {
                            *session_error = None;
                            state
                                .items
                                .retain(|item| !matches!(item.source, ProblemSource::Session(_)));
                            state.selected =
                                state.selected.min(state.items.len().saturating_sub(1));
                            state.status_line =
                                "Dismissed the latest session operation failure.".to_string();
                        } else {
                            state.status_line =
                                "Only the latest session operation failure can be dismissed."
                                    .to_string();
                        }
                    }
                    crossterm::event::KeyCode::Char('r') => {
                        let refreshed =
                            super::build_root_dashboard(repo_root, cwd, explicit_home, None);
                        let refresh_status = handle_problems_refresh_result(
                            &mut dashboard,
                            session_error,
                            refreshed,
                        );
                        state = problems_state_from_dashboard(&dashboard, session_error.as_ref());
                        state.status_line = refresh_status;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::diagnostics::effective_persisted_run_code;
    use super::super::diagnostics::tests::{failed_step, run_detail};
    use super::*;

    fn preflight(kind: &str, message: &str) -> transist::InspectionPreflightSummary {
        transist::InspectionPreflightSummary {
            status: "failed".to_string(),
            checked_at_ms: 42,
            blocker_count: 1,
            warning_count: 0,
            failed_checks: vec![transist::InspectionPreflightFailedCheck {
                kind: kind.to_string(),
                message: message.to_string(),
                step_ids: vec!["step".to_string()],
                plugin_id: None,
                action: None,
            }],
        }
    }

    fn summary(
        run_id: &str,
        root_run_id: &str,
        status: &str,
        updated_at_ms: u64,
        has_blocked_gates: bool,
    ) -> transist::InspectionRunSummary {
        transist::InspectionRunSummary {
            run_id: run_id.to_string(),
            workflow_id: "workflow".to_string(),
            workflow_version: "v1".to_string(),
            status: status.to_string(),
            created_at_ms: updated_at_ms,
            updated_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: None,
            has_blocked_gates,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: root_run_id.to_string(),
            parent_run_id: (run_id != root_run_id).then(|| root_run_id.to_string()),
        }
    }

    #[test]
    fn problems_uninitialized_home_uses_the_owned_fallback_code() {
        let items = collect_problems(false, Path::new("/home"), &[], None, None, &[], None);
        assert_eq!(items[0].code, "tui.home_uninitialized");
        assert_eq!(items[0].timestamp_ms, None);
        assert!(!items[0].list_label().contains("1970-"));
        assert!(
            !items[0]
                .detail_lines()
                .iter()
                .any(|line| line.starts_with("Observed:"))
        );
    }

    #[test]
    fn generic_failed_run_is_omitted_and_concrete_code_is_included() {
        let summaries = vec![
            summary("generic", "generic", "failed", 10, false),
            summary("specific", "specific", "failed", 20, false),
        ];
        let generic = run_detail("generic", "generic", "failed");
        let mut specific = run_detail("specific", "specific", "failed");
        specific
            .steps
            .push(failed_step(Some("plugin.action_failed"), Some("failed")));
        let details = std::collections::BTreeMap::from([
            ("generic".to_string(), generic),
            ("specific".to_string(), specific),
        ]);

        let items = collect_run_problems_with(&summaries, |id| Ok(details.get(id).cloned()));

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].code, "plugin.action_failed");
        assert_eq!(items[0].category, ProblemCategory::PluginResolution);
        assert_eq!(
            items[0].summary,
            "Persisted plugin or action failure requires attention."
        );
        assert!(items[0].list_label().contains("plugin resolution"));
        assert!(items[0].list_label().contains("plugin.action_failed"));
    }

    #[test]
    fn problems_sort_timestamped_items_before_untimestamped_items_of_the_same_severity() {
        let mut detail = run_detail("root", "root", "failed");
        detail
            .steps
            .push(failed_step(Some("persisted.failure"), Some("failed")));
        let run_items =
            collect_run_problems_with(&[summary("root", "root", "failed", 10, false)], |_| {
                Ok(Some(detail.clone()))
            });
        let items = collect_problems(true, Path::new("/home"), &run_items, None, None, &[], None);

        assert_eq!(items[0].code, "persisted.failure");
        assert_eq!(items[0].timestamp_ms, Some(0));
        assert_eq!(items[1].code, "tui.no_launchable_workflows");
        assert_eq!(items[1].timestamp_ms, None);
        assert!(items[0].list_label().contains("1970-"));
        assert!(
            items[0]
                .detail_lines()
                .iter()
                .any(|line| line.starts_with("Observed:"))
        );
    }

    #[test]
    fn blocked_gate_is_actionable_with_visible_category_and_code() {
        let mut detail = run_detail("root", "root", "blocked");
        detail.blocked_gates.push(transist::InspectionGate {
            gate_id: "review".to_string(),
            status: "blocked".to_string(),
            step_id: "review".to_string(),
            created_at_ms: Some(30),
            decision: None,
            decision_history: Vec::new(),
            request_summary: Default::default(),
        });

        let items =
            collect_run_problems_with(&[summary("root", "root", "blocked", 30, true)], |_| {
                Ok(Some(detail.clone()))
            });

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, ProblemCategory::BlockedGate);
        assert_eq!(items[0].code, "run.blocked");
        assert!(items[0].list_label().contains("blocked gate"));
        assert!(
            items[0]
                .detail_lines()
                .contains(&"Code: run.blocked".to_string())
        );
    }

    #[test]
    fn run_problems_deduplicate_by_root_and_code_and_retain_actionable_id() {
        let mut root = run_detail("root", "root", "failed");
        root.actionable_run_id = Some("child".to_string());
        let child = run_detail("child", "root", "failed");
        let details = std::collections::BTreeMap::from([
            ("root".to_string(), root),
            ("child".to_string(), child),
        ]);
        let summaries = vec![
            summary("root", "root", "failed", 20, false),
            summary("duplicate", "root", "failed", 10, false),
        ];

        let items = collect_run_problems_with(&summaries, |id| Ok(details.get(id).cloned()));

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, ProblemCategory::NestedFailure);
        assert_eq!(items[0].resource, "run child");
        assert!(matches!(
            &items[0].source,
            ProblemSource::Run { root_run_id, actionable_run_id, .. }
                if root_run_id == "root" && actionable_run_id == "child"
        ));
    }

    #[test]
    fn unreadable_failed_candidate_becomes_problem_and_count_matches_rows() {
        let run_items =
            collect_run_problems_with(&[summary("root", "root", "failed", 10, false)], |_| {
                Err(anyhow::anyhow!("corrupt detail"))
            });
        assert_eq!(run_items.len(), 1);
        assert_eq!(run_items[0].category, ProblemCategory::RunInspection);
        assert_eq!(run_items[0].code, "tui.operation_failed");

        let items = collect_problems(true, Path::new("/home"), &run_items, None, None, &[], None);
        assert_eq!(
            problem_count(true, Path::new("/home"), &run_items, None, None, &[], None),
            items.len()
        );
    }

    #[test]
    fn selected_run_report_uses_actionable_persisted_code_and_evidence() {
        let root = run_detail("root", "root", "failed");
        let mut child = run_detail("child", "root", "failed");
        child.steps.push(failed_step(
            Some("plugin.persisted_failure"),
            Some("persisted plugin failure"),
        ));
        child.preflight = Some(preflight(
            "capability",
            "denied code=capability_not_granted",
        ));

        let report = build_selected_run_report(&root, &child);

        assert_eq!(
            report.error_code.as_deref(),
            Some("plugin.persisted_failure")
        );
        assert_eq!(report.resource_identifier, "child");
        assert!(report.render().contains("Persisted preflight blockers"));
        assert!(report.render().contains("Untrusted external output"));
    }

    #[test]
    fn persisted_problem_categories_cover_preflight_resolution_capability_and_policy() {
        for (kind, message, expected) in [
            (
                "host",
                "binary missing",
                ProblemCategory::PersistedPreflight,
            ),
            (
                "plugin_resolution",
                "plugin missing",
                ProblemCategory::PluginResolution,
            ),
            (
                "workflow_resolution",
                "subflow missing",
                ProblemCategory::WorkflowResolution,
            ),
            (
                "capability",
                "capability_not_granted",
                ProblemCategory::Capability,
            ),
            ("step_policy", "policy_denied", ProblemCategory::Policy),
        ] {
            let mut detail = run_detail("root", "root", "failed");
            detail.preflight = Some(preflight(kind, message));
            assert_eq!(classify_persisted_run_problem(&detail, &detail).1, expected);
        }
        let mut detail = run_detail("root", "root", "failed");
        detail.preflight = Some(preflight(
            "capability",
            "request denied code=capability_not_granted",
        ));
        assert_eq!(
            effective_persisted_run_code(&detail, &detail).as_deref(),
            Some("capability_not_granted")
        );
    }

    #[test]
    fn persisted_problem_categories_cover_cleanup_failed_root_gate_and_nested_failure() {
        let mut cleanup = run_detail("root", "root", "failed");
        cleanup.cleanup = Some(transist::InspectionCleanupSummary {
            trigger: "failure".to_string(),
            state: "failed".to_string(),
            started_at_ms: Some(10),
            finished_at_ms: Some(20),
            preflight: None,
            steps: Vec::new(),
        });
        assert_eq!(
            classify_persisted_run_problem(&cleanup, &cleanup).1,
            ProblemCategory::Cleanup
        );

        let mut failed = run_detail("root", "root", "failed");
        failed
            .steps
            .push(failed_step(Some("tool_error"), Some("failed")));
        assert_eq!(
            classify_persisted_run_problem(&failed, &failed).1,
            ProblemCategory::FailedRoot
        );

        let mut gate = run_detail("root", "root", "blocked");
        gate.blocked_gates.push(transist::InspectionGate {
            gate_id: "review".to_string(),
            status: "blocked".to_string(),
            step_id: "review".to_string(),
            created_at_ms: Some(30),
            decision: None,
            decision_history: Vec::new(),
            request_summary: Default::default(),
        });
        assert_eq!(
            classify_persisted_run_problem(&gate, &gate).1,
            ProblemCategory::BlockedGate
        );

        let root = run_detail("root", "root", "failed");
        let child = run_detail("child", "root", "failed");
        assert_eq!(
            classify_persisted_run_problem(&root, &child).1,
            ProblemCategory::NestedFailure
        );
    }

    #[test]
    fn no_launchable_workflow_is_independent_from_plugin_inventory_failure() {
        let report = DiagnosticReport::from_error(
            DiagnosticSeverity::Error,
            Some("tui.plugin_inventory_failed".to_string()),
            "plugin inventory",
            "/home",
            "plugin inventory failed",
            None,
            &anyhow::anyhow!("broken inventory"),
        );
        let items = collect_problems(
            true,
            Path::new("/home"),
            &[],
            Some(&report),
            None,
            &[],
            None,
        );
        assert!(
            items
                .iter()
                .any(|item| item.code == "tui.plugin_inventory_failed")
        );
        assert!(
            items
                .iter()
                .any(|item| item.code == "tui.no_launchable_workflows")
        );
        assert!(items.iter().all(|item| item.timestamp_ms.is_none()));
    }

    #[test]
    fn workflow_inventory_problem_has_no_timestamp_without_a_source_timestamp() {
        let report = DiagnosticReport::from_error(
            DiagnosticSeverity::Error,
            Some("tui.workflow_inventory_failed".to_string()),
            "workflow inventory",
            "/home",
            "workflow inventory failed",
            None,
            &anyhow::anyhow!("broken inventory"),
        );
        let items = collect_problems(
            true,
            Path::new("/home"),
            &[],
            None,
            Some(&report),
            &[],
            None,
        );

        let item = items
            .iter()
            .find(|item| item.code == "tui.workflow_inventory_failed")
            .expect("workflow inventory problem");
        assert_eq!(item.timestamp_ms, None);
        assert!(
            !item
                .detail_lines()
                .iter()
                .any(|line| line.starts_with("Observed:"))
        );
    }

    #[test]
    fn ui_owned_fallback_allowlist_and_missing_report_fallback_are_exact() {
        let session = DiagnosticReport::from_error(
            DiagnosticSeverity::Error,
            None,
            "operation",
            "test",
            "failed",
            None,
            &anyhow::anyhow!("failed"),
        );
        let item = problem_from_report(
            session.clone(),
            ProblemSource::Session(session),
            ProblemCategory::Session,
            Some(1),
        );
        assert_eq!(
            UI_OWNED_FALLBACK_CODES,
            [
                "tui.home_uninitialized",
                "tui.runtime_config_invalid",
                "tui.policy_invalid",
                "tui.plugin_inventory_failed",
                "tui.workflow_inventory_failed",
                "tui.no_launchable_workflows",
                "tui.operation_failed",
                "run.failed",
                "run.blocked",
            ]
        );
        assert_eq!(item.code, "tui.operation_failed");
        assert!(UI_OWNED_FALLBACK_CODES.contains(&item.code.as_str()));
    }

    #[test]
    fn dashboard_load_failure_snapshot_can_open_timestamped_session_problem() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dashboard = super::super::build_dashboard_error(temp.path(), temp.path(), None)
            .expect("fallback dashboard");
        let session = SessionDiagnostic {
            report: DiagnosticReport::from_error(
                DiagnosticSeverity::Error,
                Some("tui.operation_failed".to_string()),
                "dashboard action",
                "load",
                "Dashboard load failed.",
                None,
                &anyhow::anyhow!("invalid configuration"),
            ),
            captured_at_ms: 99,
        };
        let state = problems_state_from_dashboard(&dashboard, Some(&session));
        let item = state
            .items
            .iter()
            .find(|item| item.code == "tui.operation_failed")
            .expect("session problem");
        assert_eq!(item.category, ProblemCategory::Session);
        assert_eq!(item.timestamp_ms, Some(99));
    }

    #[test]
    fn failed_problems_refresh_retains_snapshot_and_replaces_session_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut dashboard =
            super::super::build_dashboard_error(temp.path(), temp.path(), None).expect("dashboard");
        let retained_home = dashboard.context.home_dir().to_path_buf();
        let mut session = Some(SessionDiagnostic {
            report: DiagnosticReport::from_error(
                DiagnosticSeverity::Error,
                Some("tui.operation_failed".to_string()),
                "old operation",
                "old",
                "Old failure.",
                None,
                &anyhow::anyhow!("old"),
            ),
            captured_at_ms: 1,
        });

        let status = handle_problems_refresh_result(
            &mut dashboard,
            &mut session,
            Err(anyhow::anyhow!("refresh failed")),
        );

        assert_eq!(dashboard.context.home_dir(), retained_home);
        assert!(status.contains("retained"));
        let replacement = session.expect("replacement session failure");
        assert_eq!(
            replacement.report.error_code.as_deref(),
            Some("tui.operation_failed")
        );
        assert_eq!(replacement.report.resource_kind, "problems refresh");
        assert!(replacement.captured_at_ms > 1);
    }

    #[test]
    fn fully_composed_policy_error_retains_fragment_error_chain() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        std::fs::create_dir_all(home.join("policy")).expect("policy dir");
        std::fs::write(
            home.join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [bad.yaml]\n",
        )
        .expect("config");
        std::fs::write(
            home.join("policy/bad.yaml"),
            "schema: transist.policy_fragment.v1\ngrant_sets: [not-a-map]\n",
        )
        .expect("fragment");

        let items = collect_problems(true, home, &[], None, None, &[], None);
        let report = items
            .iter()
            .find(|item| item.code == "tui.policy_invalid")
            .expect("policy problem")
            .build_report(&transist::TransistContext::with_home(home, home))
            .expect("report");
        assert_eq!(
            items
                .iter()
                .find(|item| item.code == "tui.policy_invalid")
                .expect("policy problem")
                .timestamp_ms,
            None
        );
        let rendered = report.render();
        assert!(rendered.contains("invalid policy fragment"), "{rendered}");
        assert!(rendered.contains("bad.yaml"), "{rendered}");
        assert!(
            report
                .commands
                .contains(&"transist policy validate".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist policy show --effective".to_string())
        );
    }
}
