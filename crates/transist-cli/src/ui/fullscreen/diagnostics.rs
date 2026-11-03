// SPDX-License-Identifier: MPL-2.0
//! Structured, redacted diagnostic reports for the fullscreen operator console.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine;

use super::FullscreenPrompt;

const OSC_52_MAX_PAYLOAD_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticSeverity {
    Error,
    Attention,
}

impl DiagnosticSeverity {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Attention => "attention",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticDetailSection {
    pub(crate) heading: String,
    pub(crate) lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticReport {
    pub(crate) severity: DiagnosticSeverity,
    pub(crate) error_code: Option<String>,
    pub(crate) resource_kind: String,
    pub(crate) resource_identifier: String,
    pub(crate) summary: String,
    pub(crate) outcome: Option<String>,
    pub(crate) causes: Vec<String>,
    pub(crate) details: Vec<DiagnosticDetailSection>,
    pub(crate) next_actions: Vec<String>,
    pub(crate) commands: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SessionDiagnostic {
    pub(super) report: DiagnosticReport,
    pub(super) captured_at_ms: u64,
}

impl SessionDiagnostic {
    pub(super) fn capture(report: DiagnosticReport) -> Self {
        Self {
            report,
            captured_at_ms: crate::terminal::current_unix_ms(),
        }
    }
}

impl DiagnosticReport {
    /// Creates a report from a formatted runtime error without resolving or
    /// adding any secret values. Every formatter line is retained once.
    pub(crate) fn from_error(
        severity: DiagnosticSeverity,
        error_code: Option<String>,
        resource_kind: impl Into<String>,
        resource_identifier: impl Into<String>,
        summary: impl Into<String>,
        outcome: Option<String>,
        error: &anyhow::Error,
    ) -> Self {
        Self {
            severity,
            error_code,
            resource_kind: resource_kind.into(),
            resource_identifier: resource_identifier.into(),
            summary: summary.into(),
            outcome,
            causes: crate::signals::format_error_lines(error),
            details: vec![DiagnosticDetailSection {
                heading: "Runtime error details".to_string(),
                lines: Vec::new(),
            }],
            next_actions: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// Renders the report in the stable diagnostic field order.
    pub(crate) fn render(&self) -> String {
        let mut lines = vec![format!("Severity: {}", self.severity.label())];
        if let Some(code) = self.error_code.as_deref().filter(|code| !code.is_empty()) {
            lines.push(format!("Code: {code}"));
        }
        lines.push(format!(
            "Resource: {} {}",
            self.resource_kind, self.resource_identifier
        ));
        lines.push(format!("Summary: {}", self.summary));
        if let Some(outcome) = self
            .outcome
            .as_deref()
            .filter(|outcome| !outcome.is_empty())
        {
            lines.push(format!("Outcome: {outcome}"));
        }
        append_section(&mut lines, "Causes", &self.causes);
        for section in &self.details {
            append_section(&mut lines, &section.heading, &section.lines);
        }
        append_section(&mut lines, "Next actions", &self.next_actions);
        append_section(&mut lines, "Commands", &self.commands);
        lines.join("\n")
    }
}

/// Builds a report exclusively from persisted inspection data for a run.
///
/// External plugin, adapter, model, and stderr text is presentation-only data:
/// this function labels it untrusted and never treats it as execution authority.
#[cfg(test)]
pub(crate) fn persisted_run_diagnostic(
    detail: &transist::InspectionRunDetail,
    severity: DiagnosticSeverity,
    fallback_code: Option<String>,
    summary: impl Into<String>,
) -> DiagnosticReport {
    persisted_run_diagnostic_for_actionable(detail, detail, severity, fallback_code, summary)
}

/// Builds a persisted report for a root run whose actionable failure or gate
/// may live in a nested child. Inspection commands target that child while
/// root-owned recovery, policy, capability, and lease commands keep the root id.
pub(crate) fn persisted_run_diagnostic_for_actionable(
    root_detail: &transist::InspectionRunDetail,
    actionable_detail: &transist::InspectionRunDetail,
    severity: DiagnosticSeverity,
    fallback_code: Option<String>,
    summary: impl Into<String>,
) -> DiagnosticReport {
    let failed_step = actionable_detail
        .steps
        .iter()
        .rev()
        .find(|step| step.status == "failed");
    let preflight = actionable_detail
        .preflight
        .as_ref()
        .filter(|preflight| preflight.blocker_count > 0);
    let cleanup_preflight = root_detail
        .cleanup
        .as_ref()
        .and_then(|cleanup| cleanup.preflight.as_ref())
        .filter(|preflight| preflight.blocker_count > 0);
    let trust_denial = root_detail
        .trust
        .decision
        .as_ref()
        .filter(|decision| decision.outcome == "denied");
    let policy_denial = root_detail
        .trust
        .policy_decision
        .as_ref()
        .filter(|decision| !decision.allowed);
    let error_code = effective_persisted_run_code(root_detail, actionable_detail).or(fallback_code);
    let mut causes = Vec::new();
    let mut details = Vec::new();

    if let Some(step) = failed_step {
        let source = untrusted_output_source(actionable_detail, step);
        if let Some(message) = failed_step_error_message(step)
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            causes.push(format!("Untrusted {source}: {message}"));
        }
        if let Some(stderr) = step
            .output_summary
            .stderr_tail
            .as_deref()
            .map(str::trim)
            .filter(|stderr| !stderr.is_empty())
        {
            causes.push(format!("Untrusted stderr: {stderr}"));
        }
    }

    if let Some(preflight) = preflight {
        details.push(DiagnosticDetailSection {
            heading: "Persisted preflight blockers".to_string(),
            lines: preflight
                .failed_checks
                .iter()
                .map(format_preflight_check)
                .collect(),
        });
        let authority_checks = preflight
            .failed_checks
            .iter()
            .filter(|check| is_authority_check(check))
            .map(format_preflight_check)
            .collect::<Vec<_>>();
        if !authority_checks.is_empty() {
            let mut lines = authority_checks
                .into_iter()
                .map(|check| format!("Requested authority: {check}"))
                .collect::<Vec<_>>();
            lines.push(
                "Granted authority: inspect the persisted effective allowlist with the capabilities command."
                    .to_string(),
            );
            details.push(DiagnosticDetailSection {
                heading: "Requested and granted authority".to_string(),
                lines,
            });
        }
    }

    if let Some(preflight) = cleanup_preflight {
        details.push(DiagnosticDetailSection {
            heading: "Persisted cleanup preflight blockers".to_string(),
            lines: preflight
                .failed_checks
                .iter()
                .map(format_preflight_check)
                .collect(),
        });
    }

    let cleanup_failures = root_detail
        .cleanup
        .as_ref()
        .map(|cleanup| {
            cleanup
                .steps
                .iter()
                .filter(|step| step.status == "failed")
                .map(|step| {
                    let message = step
                        .error_message
                        .as_deref()
                        .map(str::trim)
                        .filter(|message| !message.is_empty())
                        .unwrap_or("cleanup step failed");
                    format!("{}: Untrusted cleanup output: {message}", step.step_id)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has_cleanup_failures = !cleanup_failures.is_empty();
    if has_cleanup_failures {
        details.push(DiagnosticDetailSection {
            heading: "Persisted cleanup failures".to_string(),
            lines: cleanup_failures,
        });
    }
    let aggregate_cleanup_failure = root_detail.cleanup.as_ref().is_some_and(|cleanup| {
        cleanup.state == "failed"
            && cleanup.steps.iter().all(|step| step.status != "failed")
            && cleanup_preflight.is_none()
    });
    if aggregate_cleanup_failure {
        details.push(DiagnosticDetailSection {
            heading: "Persisted cleanup failure".to_string(),
            lines: vec![
                "Cleanup is recorded as failed without an individual failed cleanup step."
                    .to_string(),
            ],
        });
    }

    let blocked_gates = actionable_detail
        .blocked_gates
        .iter()
        .filter(|gate| gate.status == "blocked")
        .map(|gate| format!("{} (step {})", gate.gate_id, gate.step_id))
        .collect::<Vec<_>>();
    if !blocked_gates.is_empty() {
        details.push(DiagnosticDetailSection {
            heading: "Blocked manual gates".to_string(),
            lines: blocked_gates,
        });
    }

    if let Some(decision) = trust_denial {
        let mut lines = Vec::new();
        if let Some(reason) = decision.reason.as_deref() {
            lines.push(format!("Persisted reason: {reason}"));
        }
        if let Some(kind) = decision.failed_verification_type.as_deref() {
            lines.push(format!("Failed verification: {kind}"));
        }
        if !lines.is_empty() {
            details.push(DiagnosticDetailSection {
                heading: "Persisted trust denial".to_string(),
                lines,
            });
        }
    }

    if let Some(decision) = policy_denial {
        let mut lines = vec![format!("Checkpoint: {}", decision.mode)];
        if let Some(code) = decision.reason_code.as_deref() {
            lines.push(format!("Persisted reason code: {code}"));
        }
        if let Some(message) = decision.message.as_deref() {
            lines.push(format!("Persisted message: {message}"));
        }
        details.push(DiagnosticDetailSection {
            heading: "Persisted policy denial".to_string(),
            lines,
        });
    }

    let lease_issue = persisted_lease_issue_evidence(root_detail, actionable_detail);
    if lease_issue && root_detail.active_lease.is_some() {
        details.push(DiagnosticDetailSection {
            heading: "Persisted lease state".to_string(),
            lines: vec![
                "Persisted failure evidence identifies a lease conflict or stale owner; inspect the active run-tree lease before recovery."
                    .to_string(),
            ],
        });
    }
    if actionable_detail.event_count > 0 {
        details.push(DiagnosticDetailSection {
            heading: "Persisted events".to_string(),
            lines: vec![format!(
                "{} event(s) recorded; latest event id: {}. Use the read-only tail command to inspect event data.",
                actionable_detail.event_count,
                actionable_detail.latest_event_id.as_deref().unwrap_or("unknown")
            )],
        });
    }

    let root_run_id = &root_detail.summary.root_run_id;
    let inspection_run_id = &actionable_detail.summary.run_id;
    let authority_denied = preflight
        .into_iter()
        .chain(cleanup_preflight)
        .any(|preflight| preflight.failed_checks.iter().any(is_authority_check));
    let show_command = if failed_step.is_some() || actionable_detail.summary.status == "failed" {
        format!("transist runs show {inspection_run_id} --failed")
    } else {
        format!("transist runs show {inspection_run_id}")
    };
    let mut commands = vec![
        show_command,
        format!("transist runs tail {inspection_run_id} --limit 80"),
    ];
    if preflight.is_some() || cleanup_preflight.is_some() {
        commands.push(format!("transist runs preflight {root_run_id}"));
    }
    if authority_denied {
        commands.push(format!(
            "transist runs capabilities {root_run_id} --format yaml"
        ));
    }
    if policy_denial.is_some() {
        commands.push("transist policy validate".to_string());
        commands.push("transist policy show --effective".to_string());
    }
    if lease_issue && root_detail.active_lease.is_some() {
        commands.push(format!("transist runs lease inspect {root_run_id}"));
    }
    if trust_denial.is_some() {
        commands.push("transist trust list".to_string());
    }
    if root_detail.summary.status == "failed" || actionable_detail.summary.status == "failed" {
        match root_detail.recovery_state.as_deref() {
            Some("cleanup_required") => {
                commands.push(format!("transist runs cleanup {root_run_id}"));
            }
            Some("cleanup_completed") => {}
            _ => commands.extend([
                format!("transist runs retry {root_run_id}"),
                format!("transist runs cleanup {root_run_id}"),
            ]),
        }
        commands.push(format!(
            "transist runs replay {root_run_id} --run-ref <new-run-id>"
        ));
    } else if has_cleanup_failures || aggregate_cleanup_failure || cleanup_preflight.is_some() {
        commands.push(format!("transist runs cleanup {root_run_id}"));
    }

    let mut next_actions = Vec::new();
    if trust_denial.is_some() {
        next_actions.push(
            "Review imported public trust roots and package provenance with CLI/admin setup tools; runtime repair never re-signs artifacts."
                .to_string(),
        );
    }
    if failed_step.is_some()
        || preflight.is_some()
        || cleanup_preflight.is_some()
        || has_cleanup_failures
        || aggregate_cleanup_failure
    {
        next_actions.push(
            "After inspection, use the applicable CLI recovery command such as `transist runs retry <run-id>`, `transist runs cleanup <run-id>`, or `transist runs replay <run-id> <new-run-id>`."
                .to_string(),
        );
    }

    DiagnosticReport {
        severity,
        error_code,
        resource_kind: "run".to_string(),
        resource_identifier: actionable_detail.summary.run_id.clone(),
        summary: summary.into(),
        outcome: actionable_detail
            .terminal_outcome
            .clone()
            .or_else(|| root_detail.terminal_outcome.clone()),
        causes,
        details,
        next_actions,
        commands,
    }
}

pub(super) fn effective_persisted_run_code(
    root_detail: &transist::InspectionRunDetail,
    actionable_detail: &transist::InspectionRunDetail,
) -> Option<String> {
    actionable_detail
        .steps
        .iter()
        .rev()
        .find(|step| step.status == "failed")
        .and_then(failed_step_error_code)
        .or_else(|| {
            actionable_detail
                .preflight
                .as_ref()
                .filter(|preflight| preflight.blocker_count > 0)
                .and_then(preflight_error_code)
        })
        .or_else(|| {
            root_detail
                .cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.preflight.as_ref())
                .filter(|preflight| preflight.blocker_count > 0)
                .and_then(preflight_error_code)
        })
        .or_else(|| {
            root_detail
                .trust
                .decision
                .as_ref()
                .filter(|decision| decision.outcome == "denied")
                .and_then(|decision| decision.reason.clone())
        })
        .or_else(|| {
            root_detail
                .trust
                .policy_decision
                .as_ref()
                .filter(|decision| !decision.allowed)
                .and_then(|decision| decision.reason_code.clone())
        })
}

fn failed_step_error_code(step: &transist::InspectionStep) -> Option<String> {
    step.output_summary
        .child_failure
        .as_ref()
        .and_then(|failure| failure.error_code.clone())
        .or_else(|| step.output_summary.error_code.clone())
}

fn failed_step_error_message(step: &transist::InspectionStep) -> Option<&str> {
    step.output_summary
        .child_failure
        .as_ref()
        .and_then(|failure| {
            failure
                .preflight_first_blocker
                .as_deref()
                .or(failure.error_message.as_deref())
                .or(failure.stderr.as_deref())
        })
        .or(step.output_summary.error_message.as_deref())
}

pub(super) fn persisted_lease_issue_evidence(
    root_detail: &transist::InspectionRunDetail,
    actionable_detail: &transist::InspectionRunDetail,
) -> bool {
    let failed_step_has_evidence = actionable_detail
        .steps
        .iter()
        .filter(|step| step.status == "failed")
        .filter_map(failed_step_error_code)
        .any(|code| is_lease_issue_text(&code));
    let preflight_has_evidence = |preflight: Option<&transist::InspectionPreflightSummary>| {
        preflight.is_some_and(|preflight| {
            preflight.failed_checks.iter().any(|check| {
                is_lease_issue_text(&check.kind) || is_lease_issue_text(&check.message)
            })
        })
    };

    failed_step_has_evidence
        || preflight_has_evidence(actionable_detail.preflight.as_ref())
        || preflight_has_evidence(
            root_detail
                .cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.preflight.as_ref()),
        )
}

fn is_lease_issue_text(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    (value.contains("lease") && value.contains("conflict"))
        || value.contains("already leased")
        || (value.contains("lease") && value.contains("stale owner"))
        || (value.contains("lease") && value.contains("stale_owner"))
        || (value.contains("lease") && value.contains("orphaned owner"))
}

fn preflight_error_code(preflight: &transist::InspectionPreflightSummary) -> Option<String> {
    preflight
        .failed_checks
        .iter()
        .find_map(|check| extract_persisted_error_code(&check.message))
}

fn extract_persisted_error_code(message: &str) -> Option<String> {
    message
        .split("code=")
        .nth(1)
        .and_then(|suffix| {
            suffix
                .split(|character: char| !matches!(character, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '-'))
                .next()
        })
        .filter(|code| !code.is_empty())
        .map(str::to_string)
}

fn format_preflight_check(check: &transist::InspectionPreflightFailedCheck) -> String {
    let subject = match (&check.plugin_id, &check.action) {
        (Some(plugin), Some(action)) => format!(" ({plugin}/{action})"),
        (Some(plugin), None) => format!(" ({plugin})"),
        (None, Some(action)) => format!(" ({action})"),
        (None, None) => String::new(),
    };
    let steps = if check.step_ids.is_empty() {
        String::new()
    } else {
        format!(" [{}]", check.step_ids.join(", "))
    };
    format!(
        "{}{}{}: {}",
        check.kind,
        subject,
        steps,
        check.message.trim()
    )
}

fn is_authority_check(check: &transist::InspectionPreflightFailedCheck) -> bool {
    let message = check.message.to_ascii_lowercase();
    check.kind.contains("capability")
        || check.kind.contains("policy")
        || message.contains("capability_not_granted")
        || message.contains("policy.allow.capabilities")
        || message.contains("policy_denied")
}

fn untrusted_output_source(
    detail: &transist::InspectionRunDetail,
    step: &transist::InspectionStep,
) -> String {
    let model = detail
        .locality
        .steps
        .iter()
        .find(|locality| locality.step_id == step.step_id)
        .and_then(|locality| {
            locality
                .model
                .as_deref()
                .map(|model| (locality.provider.as_deref().unwrap_or("unknown"), model))
        });
    match (
        step.input_summary.plugin_id.as_deref(),
        step.input_summary.action.as_deref(),
        step.input_summary.adapter_kind.as_deref(),
        model,
    ) {
        (Some(plugin), Some(action), Some(adapter), Some((provider, model))) => {
            format!(
                "plugin/adapter/model output ({plugin}/{action} via {adapter}; {provider}/{model})"
            )
        }
        (_, _, _, Some((provider, model))) => format!("model output ({provider}/{model})"),
        (Some(plugin), Some(action), Some(adapter), None) => {
            format!("plugin/adapter output ({plugin}/{action} via {adapter})")
        }
        (Some(plugin), Some(action), None, None) => format!("plugin output ({plugin}/{action})"),
        (_, _, Some(adapter), None) => format!("adapter output ({adapter})"),
        _ => "external output".to_string(),
    }
}

fn append_section(lines: &mut Vec<String>, heading: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!("{heading}:"));
    lines.extend(values.iter().cloned());
}

pub(crate) fn show_diagnostic_report(
    prompt: &mut FullscreenPrompt,
    title: &str,
    report: &DiagnosticReport,
) -> Result<()> {
    prompt.run_diagnostic_pager(title, &report.render())
}

pub(crate) fn copy_diagnostic_to_clipboard(report: &str) -> Result<String> {
    let mut stdout = std::io::stdout();
    write_osc_52_clipboard_sequence(&mut stdout, report)?;
    stdout.flush().context("flush OSC 52 clipboard sequence")?;
    Ok("Clipboard sequence sent.".to_string())
}

fn write_osc_52_clipboard_sequence(writer: &mut impl Write, report: &str) -> Result<()> {
    if report.len() > OSC_52_MAX_PAYLOAD_BYTES {
        bail!(
            "Diagnostic report is {} bytes; clipboard limit is {} bytes.",
            report.len(),
            OSC_52_MAX_PAYLOAD_BYTES
        );
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(report);
    write!(writer, "\x1b]52;c;{encoded}\x07").context("write OSC 52 clipboard sequence")
}

pub(crate) fn open_diagnostic_in_external_pager(
    prompt: &mut FullscreenPrompt,
    report: &str,
) -> Result<String> {
    let pager = transist::load_user_preferred_pager()?
        .unwrap_or_else(|| std::env::var("PAGER").unwrap_or_default());
    let pager = pager.trim();
    if pager.is_empty() {
        return Ok("No external pager is configured.".to_string());
    }

    prompt.with_suspended_terminal(|| pipe_to_pager(pager, report))?;
    Ok("Diagnostic report sent to external pager.".to_string())
}

fn pipe_to_pager(command: &str, report: &str) -> Result<()> {
    let argv = shell_words::split(command).ok();
    let mut pager_command = match argv {
        Some(argv) if !argv.is_empty() => {
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            command
        }
        _ => {
            let mut shell = Command::new("sh");
            shell.arg("-c").arg(command);
            shell
        }
    };
    let mut child = pager_command
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("start external pager '{command}'"))?;
    child
        .stdin
        .take()
        .context("open external pager stdin")?
        .write_all(report.as_bytes())
        .context("write diagnostic report to external pager")?;
    let status = child.wait().context("wait for external pager")?;
    if status.success() {
        Ok(())
    } else {
        bail!("external pager exited with {status}")
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn run_detail(
        run_id: &str,
        root_run_id: &str,
        status: &str,
    ) -> transist::InspectionRunDetail {
        transist::InspectionRunDetail {
            summary: transist::InspectionRunSummary {
                run_id: run_id.to_string(),
                workflow_id: "demo".to_string(),
                workflow_version: "v1".to_string(),
                status: status.to_string(),
                created_at_ms: 0,
                updated_at_ms: 0,
                started_at_ms: None,
                finished_at_ms: None,
                current_step_id: None,
                has_blocked_gates: false,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: root_run_id.to_string(),
                parent_run_id: (run_id != root_run_id).then(|| root_run_id.to_string()),
            },
            workflow_inputs: BTreeMap::new(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: None,
            actionable_run_id: None,
            active_lease: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            blocked_gates: Vec::new(),
            steps: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: BTreeMap::new(),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: format!("transist runs evidence export {root_run_id}"),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    pub(crate) fn failed_step(
        error_code: Option<&str>,
        message: Option<&str>,
    ) -> transist::InspectionStep {
        transist::InspectionStep {
            step_id: "verify".to_string(),
            step_kind: "invoke".to_string(),
            status: "failed".to_string(),
            execution: Some(1),
            started_at_ms: None,
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: true,
                invoke_id: None,
                status: Some("failed".to_string()),
                exit_code: None,
                error_code: error_code.map(str::to_string),
                error_message: message.map(str::to_string),
                stderr_tail: None,
                child_run_id: None,
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: false,
            latest_output: None,
            executions: Vec::new(),
        }
    }

    fn failed_preflight(code: &str) -> transist::InspectionPreflightSummary {
        transist::InspectionPreflightSummary {
            status: "failed".to_string(),
            checked_at_ms: 1,
            blocker_count: 1,
            warning_count: 0,
            failed_checks: vec![transist::InspectionPreflightFailedCheck {
                kind: "step_policy".to_string(),
                message: format!("code={code}: denied"),
                step_ids: vec!["verify".to_string()],
                plugin_id: None,
                action: None,
            }],
        }
    }

    #[test]
    fn healthy_running_active_lease_is_not_a_diagnostic() {
        let mut detail = run_detail("run-1", "run-1", "running");
        detail.active_lease = Some(transist::RunTreeLeaseRecord {
            schema_version: transist::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
            root_run_id: "run-1".to_string(),
            lease_id: "lease-1".to_string(),
            owner_id: "runner-1".to_string(),
            management_mode: transist::ManagementMode::Standalone,
            pid: 42,
            hostname: "localhost".to_string(),
            command: "transist run run-1".to_string(),
            transist_version: "test".to_string(),
            acquired_at_ms: 1,
            last_heartbeat_ms: 2,
            external: None,
        });

        assert!(!super::super::run_detail::run_has_persisted_diagnostic(
            &detail, &detail
        ));
        let report =
            persisted_run_diagnostic(&detail, DiagnosticSeverity::Error, None, "diagnostic");
        assert!(!report.render().to_ascii_lowercase().contains("lease"));
    }

    #[test]
    fn sparse_failed_run_has_a_fallback_diagnostic() {
        let detail = run_detail("run-1", "run-1", "failed");

        assert!(super::super::run_detail::run_has_persisted_diagnostic(
            &detail, &detail
        ));
        let report = persisted_run_diagnostic(
            &detail,
            DiagnosticSeverity::Error,
            Some("run.failed".to_string()),
            "failed",
        );
        assert_eq!(report.error_code.as_deref(), Some("run.failed"));
    }

    #[test]
    fn blocked_manual_gate_has_a_persisted_diagnostic() {
        let mut detail = run_detail("run-1", "run-1", "blocked");
        detail.blocked_gates.push(transist::InspectionGate {
            gate_id: "review".to_string(),
            status: "blocked".to_string(),
            step_id: "approve".to_string(),
            created_at_ms: None,
            decision: None,
            decision_history: Vec::new(),
            request_summary: BTreeMap::new(),
        });

        assert!(super::super::run_detail::run_has_persisted_diagnostic(
            &detail, &detail
        ));
        let report = persisted_run_diagnostic(
            &detail,
            DiagnosticSeverity::Attention,
            Some("run.blocked".to_string()),
            "blocked",
        );
        assert_eq!(report.severity, DiagnosticSeverity::Attention);
        assert!(report.render().contains("review (step approve)"));
    }

    #[test]
    fn nested_child_failure_targets_child_inspection_and_root_control_commands() {
        let mut root = run_detail("root", "root", "failed");
        root.active_lease = Some(transist::RunTreeLeaseRecord {
            schema_version: transist::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
            root_run_id: "root".to_string(),
            lease_id: "lease-1".to_string(),
            owner_id: "runner-1".to_string(),
            management_mode: transist::ManagementMode::Standalone,
            pid: 42,
            hostname: "localhost".to_string(),
            command: "transist run root".to_string(),
            transist_version: "test".to_string(),
            acquired_at_ms: 1,
            last_heartbeat_ms: 2,
            external: None,
        });
        let mut child = run_detail("root__child", "root", "failed");
        child.steps.push(failed_step(
            Some("plugin.effective_failure"),
            Some("failed"),
        ));
        child.preflight = Some(failed_preflight("capability_not_granted"));
        child
            .preflight
            .as_mut()
            .expect("preflight")
            .failed_checks
            .push(transist::InspectionPreflightFailedCheck {
                kind: "run_tree_lease_conflict".to_string(),
                message: "persisted lease conflict".to_string(),
                step_ids: Vec::new(),
                plugin_id: None,
                action: None,
            });

        let report = persisted_run_diagnostic_for_actionable(
            &root,
            &child,
            DiagnosticSeverity::Error,
            Some("run.failed".to_string()),
            "failed",
        );

        assert_eq!(report.resource_identifier, "root__child");
        assert_eq!(
            report.error_code.as_deref(),
            Some("plugin.effective_failure")
        );
        assert!(
            report
                .commands
                .contains(&"transist runs show root__child --failed".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs tail root__child --limit 80".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs preflight root".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs capabilities root --format yaml".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs lease inspect root".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs retry root".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs cleanup root".to_string())
        );
        assert!(
            report
                .commands
                .contains(&"transist runs replay root --run-ref <new-run-id>".to_string())
        );
    }

    #[test]
    fn cleanup_preflight_blockers_are_included_and_supply_the_effective_code() {
        let mut detail = run_detail("run-1", "run-1", "failed");
        detail.cleanup = Some(transist::InspectionCleanupSummary {
            trigger: "failure".to_string(),
            state: "failed".to_string(),
            started_at_ms: None,
            finished_at_ms: None,
            preflight: Some(failed_preflight("cleanup.capability_denied")),
            steps: Vec::new(),
        });

        let report = persisted_run_diagnostic(
            &detail,
            DiagnosticSeverity::Error,
            Some("run.failed".to_string()),
            "failed",
        );
        assert_eq!(
            report.error_code.as_deref(),
            Some("cleanup.capability_denied")
        );
        assert!(
            report
                .render()
                .contains("Persisted cleanup preflight blockers:")
        );
    }

    #[test]
    fn aggregate_cleanup_failure_is_reported_without_a_failed_step() {
        let mut detail = run_detail("run-1", "run-1", "failed");
        detail.cleanup = Some(transist::InspectionCleanupSummary {
            trigger: "failure".to_string(),
            state: "failed".to_string(),
            started_at_ms: None,
            finished_at_ms: None,
            preflight: None,
            steps: Vec::new(),
        });

        let report = persisted_run_diagnostic(
            &detail,
            DiagnosticSeverity::Error,
            Some("run.failed".to_string()),
            "failed",
        );
        assert!(
            report.render().contains(
                "Cleanup is recorded as failed without an individual failed cleanup step."
            )
        );
    }

    #[test]
    fn renders_error_causes_once_in_order() {
        let error = anyhow::anyhow!("outer").context("middle").context("inner");
        let report = DiagnosticReport::from_error(
            DiagnosticSeverity::Error,
            Some("run.failed".to_string()),
            "run",
            "run-1",
            "Run failed",
            None,
            &error,
        );
        let rendered = report.render();
        let expected = crate::signals::format_error_lines(&error);
        for line in expected {
            assert_eq!(rendered.matches(&line).count(), 1, "line: {line}");
        }
    }

    #[test]
    fn omits_empty_optional_sections() {
        let report = DiagnosticReport {
            severity: DiagnosticSeverity::Error,
            error_code: None,
            resource_kind: "home".to_string(),
            resource_identifier: "/tmp/.transist".to_string(),
            summary: "Home is not initialized".to_string(),
            outcome: None,
            causes: Vec::new(),
            details: Vec::new(),
            next_actions: Vec::new(),
            commands: Vec::new(),
        };
        assert_eq!(
            report.render(),
            "Severity: error\nResource: home /tmp/.transist\nSummary: Home is not initialized"
        );
    }

    #[test]
    fn osc_52_output_has_exact_payload_boundaries() {
        for size in [0, OSC_52_MAX_PAYLOAD_BYTES] {
            let report = "x".repeat(size);
            let mut output = Vec::new();
            write_osc_52_clipboard_sequence(&mut output, &report).expect("encode report");
            let encoded = base64::engine::general_purpose::STANDARD.encode(report);
            assert_eq!(output, format!("\x1b]52;c;{encoded}\x07").as_bytes());
        }

        let mut output = Vec::new();
        let report = "x".repeat(OSC_52_MAX_PAYLOAD_BYTES + 1);
        assert!(write_osc_52_clipboard_sequence(&mut output, &report).is_err());
        assert!(output.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn pipes_diagnostics_through_shell_form_pager_commands() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = temp.path().join("pager output.txt");
        let command = format!("sh -c 'cat > \"$1\"' sh '{}'", output.display());

        pipe_to_pager(&command, "diagnostic body").expect("run shell-form pager");

        assert_eq!(
            std::fs::read_to_string(output).expect("read pager output"),
            "diagnostic body"
        );
    }
}
