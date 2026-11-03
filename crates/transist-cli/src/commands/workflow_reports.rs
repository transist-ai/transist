// SPDX-License-Identifier: MPL-2.0
//! Concise presentation shared by command and interactive workflow reviews.

use std::fmt::Write;
use transist::{
    ValidationStatus, WorkflowAuthorizationStatus, WorkflowAuthorizationSummary, WorkflowCheckMode,
    WorkflowCheckSummary, WorkflowReadinessStatus,
};

const FULL_REPORT: &str = "Full report: rerun this command with --format json.\n";

/// Escapes terminal controls without shortening authority-bearing values.
fn escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn validation_status(status: ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Passed => "passed",
        ValidationStatus::DeferredToDispatch => "deferred_to_dispatch",
        ValidationStatus::Warning => "warning",
        ValidationStatus::NotEvaluated => "not_evaluated",
        ValidationStatus::Failed => "failed",
    }
}

fn readiness_status(status: WorkflowReadinessStatus) -> &'static str {
    match status {
        WorkflowReadinessStatus::Ready => "ready",
        WorkflowReadinessStatus::NotReady => "not_ready",
        WorkflowReadinessStatus::NotDeliverable => "not_deliverable",
        WorkflowReadinessStatus::InvalidDefinition => "invalid_definition",
    }
}

fn findings(text: &mut String, summary: &WorkflowCheckSummary) {
    for finding in &summary.findings {
        writeln!(
            text,
            "- {} [{}] ({} occurrences): {}",
            validation_status(finding.status),
            escape(&finding.code),
            finding.occurrences,
            escape(&finding.message)
        )
        .unwrap();
        if !finding.example_step_paths.is_empty() {
            writeln!(
                text,
                "  Example steps: {}",
                finding
                    .example_step_paths
                    .iter()
                    .map(|s| escape(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .unwrap();
        }
        if let Some(remediation) = &finding.remediation {
            writeln!(text, "  Remediation: {}", escape(remediation)).unwrap();
        }
    }
}

// The exact fragment is the sole source of grant rows and binding, including plugin versions
// and selected actions. JSON escapes embedded controls while preserving every policy field.
fn policy(text: &mut String, fragment: &serde_json::Value) {
    // JSON escapes embedded newlines, but DEL/C1 controls need explicit escaping too.
    let json = serde_json::to_string_pretty(fragment).expect("policy value serializes");
    let json = json.chars().fold(String::new(), |mut output, ch| {
        if ch.is_control() && ch != '\n' {
            write!(output, "\\u{:04x}", ch as u32).unwrap();
        } else {
            output.push(ch);
        }
        output
    });
    writeln!(text, "Exact policy:\n{}", json).unwrap();
}

/// Renders a compact check using summary data only; does not evaluate or write anything.
pub(crate) fn render_check_summary(summary: &WorkflowCheckSummary) -> String {
    let mode = match summary.mode {
        WorkflowCheckMode::Authored => "authored",
        WorkflowCheckMode::Bound => "bound",
        WorkflowCheckMode::Readiness => "readiness",
    };
    let mut text = format!(
        "Workflow: {}\nMode: {mode}\nStatus: {}\n",
        escape(&summary.workflow_ref),
        summary
            .readiness_status
            .map(readiness_status)
            .unwrap_or_else(|| validation_status(summary.status))
    );
    writeln!(
        text,
        "Evaluation mode: {}\nValidation status: {}",
        match summary.evaluation_mode {
            transist::WorkflowEvaluationMode::Validation => "validation",
            transist::WorkflowEvaluationMode::LiveReadiness => "live_readiness",
        },
        validation_status(summary.validation_status)
    )
    .unwrap();
    if summary.evaluation_mode == transist::WorkflowEvaluationMode::Validation
        && summary.readiness_status.is_some()
        && summary.is_validation_passed()
    {
        text.push_str("Validation passed; live readiness not checked.\n");
    }
    if let Some(workspace) = &summary.workspace_dir {
        writeln!(text, "Workspace: {}", escape(&workspace.to_string_lossy())).unwrap();
    }
    writeln!(text, "Passing checks: {}", summary.passed_checks).unwrap();
    findings(&mut text, summary);
    if !summary.required_input_names.is_empty() {
        writeln!(
            text,
            "Required inputs: {}",
            summary
                .required_input_names
                .iter()
                .map(|name| escape(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
    writeln!(text, "Missing grants: {}", summary.missing_capability_count).unwrap();
    if summary.missing_capability_count > 0 {
        if let Some(suggestion) = &summary.policy_suggestion {
            policy(
                &mut text,
                &serde_json::to_value(suggestion).expect("policy hint serializes"),
            );
        }
    }
    text.push_str(if summary.authorization_eligible {
        "Next action: review exact grants with workflows authorize.\n"
    } else if summary.is_ready() {
        "Next action: launch the workflow with these inputs.\n"
    } else if summary.is_validation_passed() && summary.readiness_status.is_some() {
        "Next action: check --live for live readiness, or prepare with these inputs.\n"
    } else if summary.status == ValidationStatus::Passed
        && summary.mode != WorkflowCheckMode::Readiness
    {
        "Next action: check readiness with prospective inputs.\n"
    } else {
        "Next action: resolve the findings and check again.\n"
    });
    text.push_str(FULL_REPORT);
    text
}

/// Renders a plan or receipt from its actual fragment; never treats pre-application gaps as current.
pub(crate) fn render_authorization_summary(summary: &WorkflowAuthorizationSummary) -> String {
    let (status, label) = match summary.status {
        WorkflowAuthorizationStatus::Planned => ("planned", "Grants to add"),
        WorkflowAuthorizationStatus::Applied => ("applied", "Grants added"),
        WorkflowAuthorizationStatus::AlreadyAuthorized => {
            ("already_authorized", "No changes needed")
        }
        WorkflowAuthorizationStatus::Declined => ("declined", "No grants written"),
        WorkflowAuthorizationStatus::Blocked => ("blocked", "No grants written"),
    };
    let mut text = format!(
        "Workflow: {}\nWorkflow scope: {}\nPolicy destination: {}\nOutcome: {status}\n{label}\n",
        escape(&summary.workflow),
        escape(&summary.workflow),
        summary
            .policy_output
            .as_ref()
            .map(|path| escape(&path.to_string_lossy()))
            .unwrap_or_else(|| "unavailable".into())
    );
    writeln!(
        text,
        "Evaluation mode: {}\nValidation status: {}",
        match summary.readiness.evaluation_mode {
            transist::WorkflowEvaluationMode::Validation => "validation",
            transist::WorkflowEvaluationMode::LiveReadiness => "live_readiness",
        },
        validation_status(summary.readiness.validation_status)
    )
    .unwrap();
    if summary.readiness.evaluation_mode == transist::WorkflowEvaluationMode::Validation {
        text.push_str("Live readiness not checked. Preparation still checks live readiness.\n");
    }
    if let Some(fragment) = &summary.policy {
        policy(&mut text, fragment);
    }
    if summary.status == WorkflowAuthorizationStatus::Blocked {
        findings(&mut text, &summary.readiness);
    }
    text.push_str(FULL_REPORT);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_escapes_all_terminal_controls_and_round_trips_exact_values() {
        let fragment = serde_json::json!({"resource":"file:///exact\u{7f}\u{85}\u{1b}\npath", "operations":["read"]});
        let mut text = String::new();
        policy(&mut text, &fragment);
        assert!(!text.contains('\u{7f}'));
        assert!(!text.contains('\u{85}'));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                text.strip_prefix("Exact policy:\n").unwrap()
            )
            .unwrap(),
            fragment
        );
    }

    fn summary() -> WorkflowCheckSummary {
        WorkflowCheckSummary {
            mode: WorkflowCheckMode::Readiness,
            evaluation_mode: transist::WorkflowEvaluationMode::LiveReadiness,
            validation_status: ValidationStatus::Failed,
            workflow_ref: "demo@v123".into(),
            workspace_dir: Some("/long/workspace".into()),
            status: ValidationStatus::Failed,
            readiness_status: Some(WorkflowReadinessStatus::NotDeliverable),
            passed_checks: 10,
            findings: vec![transist::WorkflowCheckFinding {
                code: "unknown\u{1b}[2J".into(),
                status: ValidationStatus::Failed,
                message: "message\nforged".into(),
                remediation: Some("fix\rnow".into()),
                occurrences: 1000,
                example_step_paths: vec!["nested/step\tname".into()],
            }],
            required_input_names: vec!["input\0name".into()],
            missing_capability_count: 2,
            policy_suggestion: None,
            authorization_eligible: false,
        }
    }

    #[test]
    fn workflow_validation_fast_path_rendering_distinguishes_passing_validation_from_live_readiness()
     {
        let mut summary = summary();
        summary.evaluation_mode = transist::WorkflowEvaluationMode::Validation;
        summary.mode = WorkflowCheckMode::Bound;
        summary.validation_status = ValidationStatus::Passed;
        summary.status = ValidationStatus::Passed;
        summary.readiness_status = Some(WorkflowReadinessStatus::NotReady);
        summary.findings.clear();
        summary.missing_capability_count = 0;
        let text = render_check_summary(&summary);
        assert!(text.contains("Evaluation mode: validation\nValidation status: passed"));
        assert!(text.contains("Validation passed; live readiness not checked."));
        assert!(text.contains("Status: not_ready"));
        for status in [
            ValidationStatus::Failed,
            ValidationStatus::Warning,
            ValidationStatus::NotEvaluated,
        ] {
            summary.validation_status = status;
            assert!(!summary.is_validation_passed());
            assert!(!render_check_summary(&summary).contains("Validation passed;"));
        }
    }

    #[test]
    fn check_escapes_controls_preserves_unknown_findings_and_eligibility() {
        let mut summary = summary();
        let text = render_check_summary(&summary);
        assert!(text.contains("Workflow: demo@v123\nMode: readiness\nStatus: not_deliverable"));
        for expected in [
            "unknown\\u{1b}[2J",
            "message\\nforged",
            "fix\\rnow",
            "nested/step\\tname",
            "input\\u{0}name",
            "1000 occurrences",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("workflows authorize"));
        summary.authorization_eligible = true;
        assert!(render_check_summary(&summary).contains("workflows authorize"));
        for mode in [
            WorkflowCheckMode::Authored,
            WorkflowCheckMode::Bound,
            WorkflowCheckMode::Readiness,
        ] {
            summary.mode = mode;
            assert!(render_check_summary(&summary).ends_with(FULL_REPORT));
        }
    }

    #[test]
    fn revised_authoring_summary_renders_deferred_work_without_a_validation_blocker() {
        let mut summary = summary();
        summary.evaluation_mode = transist::WorkflowEvaluationMode::Validation;
        summary.mode = WorkflowCheckMode::Bound;
        summary.validation_status = ValidationStatus::Passed;
        summary.status = ValidationStatus::Passed;
        summary.readiness_status = Some(WorkflowReadinessStatus::NotReady);
        summary.findings[0].status = ValidationStatus::DeferredToDispatch;
        summary.findings[0].code = "deferred_to_dispatch".into();
        let text = render_check_summary(&summary);
        assert!(text.contains("deferred_to_dispatch"));
        assert!(text.contains("Validation passed; live readiness not checked."));
        assert!(summary.is_validation_passed());
    }

    #[test]
    fn authorization_outcomes_render_exact_applied_policy_and_unabridged_authority() {
        let resource = format!("file:///{}", "long-directory/".repeat(100));
        let fragment = serde_json::json!({"schema":"transist.policy_fragment.v1",
            "grant_sets":{"authorization-exact":{"allow":{"plugin_grants":[{"plugin":"package@v123456789", "actions":["exact_action", "second_action"]}],
            "capabilities":[{"capability":"filesystem", "resource":resource, "operations":["read", "write"]}]}}},
            "workflow_grants":[{"workflow":"demo@v123", "use":["authorization-exact"]}]});
        for (status, label) in [
            (WorkflowAuthorizationStatus::Planned, "Grants to add"),
            (WorkflowAuthorizationStatus::Applied, "Grants added"),
            (
                WorkflowAuthorizationStatus::AlreadyAuthorized,
                "No changes needed",
            ),
            (WorkflowAuthorizationStatus::Declined, "No grants written"),
            (WorkflowAuthorizationStatus::Blocked, "No grants written"),
        ] {
            let result = WorkflowAuthorizationSummary {
                workflow: "demo@v123".into(),
                status,
                readiness: summary(),
                policy: Some(fragment.clone()),
                policy_output: Some("/policy/destination\u{1b}.yaml".into()),
            };
            let text = render_authorization_summary(&result);
            assert!(text.contains(label));
            assert!(text.contains("Workflow scope: demo@v123"));
            assert!(text.contains(&resource));
            assert!(text.contains("package@v123456789"));
            assert!(text.contains("exact_action"));
            assert!(text.contains("second_action"));
            assert!(text.contains("destination\\u{1b}.yaml"));
            assert!(text.contains(&serde_json::to_string_pretty(&fragment).unwrap()));
            assert!(text.ends_with(FULL_REPORT));
            if status == WorkflowAuthorizationStatus::Applied {
                assert!(!text.contains("Missing grants"));
                assert!(!text.contains("message"));
            }
        }
        let result = WorkflowAuthorizationSummary {
            workflow: "demo@v123".into(),
            status: WorkflowAuthorizationStatus::Blocked,
            readiness: summary(),
            policy: None,
            policy_output: None,
        };
        assert!(render_authorization_summary(&result).contains("Policy destination: unavailable"));
    }
}
