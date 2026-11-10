// SPDX-License-Identifier: MPL-2.0
//! Incremental diagnostic collection shared by authored, bound, and live evaluation.

use super::*;

/// Scope of a prospective installed-workflow evaluation.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEvaluationMode {
    /// Validate current inputs, authority, prerequisites, and secret availability without probes.
    Validation,
    /// Also perform fresh adapter/plugin readiness probes.
    LiveReadiness,
}

#[cfg(test)]
thread_local! {
    static FULL_RESULTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LIVE_EVALUATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static READINESS_RESULTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static AUTHORIZATION_RESULTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Evaluation surface represented by a compact workflow check.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCheckMode {
    /// Definition validation without prospective inputs.
    Authored,
    /// Static validation with prospective inputs.
    Bound,
    /// Complete birth-known readiness evaluation.
    Readiness,
}

/// One distinct actionable diagnostic, with bounded examples in encounter order.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowCheckFinding {
    /// Stable code; unknown codes are retained verbatim.
    pub code: String,
    /// Non-passing outcome.
    pub status: ValidationStatus,
    /// Complete diagnostic message.
    pub message: String,
    /// Complete actionable remediation, when available.
    pub remediation: Option<String>,
    /// Number of associated occurrences, including repeated paths.
    pub occurrences: usize,
    /// At most three unique root-relative paths, in encounter order.
    pub example_step_paths: Vec<String>,
}

/// Compact, read-only evaluation; contains neither input values nor full diagnostics.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowCheckSummary {
    /// Authored, static bound, or live readiness evaluation.
    pub mode: WorkflowCheckMode,
    /// Whether live adapter readiness was evaluated.
    pub evaluation_mode: WorkflowEvaluationMode,
    /// Required validation outcome; omitted live probes are not incomplete validation.
    pub validation_status: ValidationStatus,
    /// Exact evaluated workflow identity.
    pub workflow_ref: String,
    /// Canonical bound workspace, if binding completed.
    pub workspace_dir: Option<PathBuf>,
    /// Aggregate validation outcome (Failed > Warning > NotEvaluated > Passed).
    pub status: ValidationStatus,
    /// Prospective-run outcome; present for complete validation or live readiness evaluation.
    pub readiness_status: Option<WorkflowReadinessStatus>,
    /// Number of passing occurrences; their records are not retained.
    pub passed_checks: usize,
    /// Distinct non-passing diagnostics in first-encounter order.
    pub findings: Vec<WorkflowCheckFinding>,
    /// Required input names only, never supplied or default values.
    pub required_input_names: Vec<String>,
    /// Number of exact missing normalized capability requests.
    pub missing_capability_count: usize,
    /// Exact compact missing-grant fragment, if bound validation completed.
    pub policy_suggestion: Option<crate::CapabilityPolicyHint>,
    /// Missing authority exists and every failed check has an accepted authority code.
    pub authorization_eligible: bool,
}

impl WorkflowCheckSummary {
    /// Returns whether this readiness evaluation permits fresh run birth.
    pub fn is_ready(&self) -> bool {
        self.evaluation_mode == WorkflowEvaluationMode::LiveReadiness
            && self.readiness_status == Some(WorkflowReadinessStatus::Ready)
    }

    /// Returns whether every required validation check passed, excluding omitted live probes.
    pub fn is_validation_passed(&self) -> bool {
        self.validation_status == ValidationStatus::Passed
    }

    pub(crate) fn invalid_definition(workflow: &str, error: &anyhow::Error) -> Self {
        let mut checks = CheckCollector::new(ReportDetail::Summary);
        checks.push(validation_check(
            "bound_validation",
            ValidationStatus::Failed,
            None,
            error.to_string(),
            None,
        ));
        let mut summary = checks.summary(WorkflowCheckMode::Readiness, workflow.to_string());
        summary.readiness_status = Some(WorkflowReadinessStatus::InvalidDefinition);
        if let Some(binding) = error.downcast_ref::<WorkflowInputBindingError>() {
            let name = match &binding.kind {
                WorkflowInputBindingErrorKind::MissingRequiredInput { input_name }
                | WorkflowInputBindingErrorKind::TypeMismatch { input_name, .. }
                | WorkflowInputBindingErrorKind::UnsupportedDeclaredType { input_name, .. } => {
                    Some(input_name)
                }
                WorkflowInputBindingErrorKind::UndeclaredInput { .. } => None,
            };
            summary.required_input_names.extend(name.cloned());
        }
        summary
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReportDetail {
    Summary,
    Full,
}

type FindingKey = (u8, String, String, Option<String>);

pub(super) struct CheckCollector {
    detail: ReportDetail,
    pub(super) checks: Vec<ValidationCheck>,
    pub(super) status: ValidationStatus,
    passed: usize,
    findings: Vec<WorkflowCheckFinding>,
    groups: BTreeMap<FindingKey, usize>,
    only_authority_failures: bool,
}

fn rank(status: ValidationStatus) -> u8 {
    match status {
        ValidationStatus::Passed | ValidationStatus::DeferredToDispatch => 0,
        ValidationStatus::NotEvaluated => 1,
        ValidationStatus::Warning => 2,
        ValidationStatus::Failed => 3,
    }
}

pub(super) fn readiness_status(status: ValidationStatus) -> WorkflowReadinessStatus {
    match status {
        ValidationStatus::Passed | ValidationStatus::DeferredToDispatch => {
            WorkflowReadinessStatus::Ready
        }
        ValidationStatus::Failed => WorkflowReadinessStatus::NotDeliverable,
        ValidationStatus::Warning | ValidationStatus::NotEvaluated => {
            WorkflowReadinessStatus::NotReady
        }
    }
}

pub(super) fn validation_readiness_status(status: ValidationStatus) -> WorkflowReadinessStatus {
    match status {
        ValidationStatus::Failed => WorkflowReadinessStatus::NotDeliverable,
        _ => WorkflowReadinessStatus::NotReady,
    }
}

/// The exact authority-only rule used by authorization planning.
pub(super) fn authority_failure_code(code: &str) -> bool {
    matches!(
        code,
        "run_policy" | "step_policy" | "capability_not_granted"
    )
}

impl CheckCollector {
    pub(super) fn new(detail: ReportDetail) -> Self {
        Self {
            detail,
            checks: Vec::new(),
            status: ValidationStatus::Passed,
            passed: 0,
            findings: Vec::new(),
            groups: BTreeMap::new(),
            only_authority_failures: true,
        }
    }

    pub(super) fn push(&mut self, check: ValidationCheck) {
        self.record(
            &check.code,
            check.status,
            &check.message,
            check.remediation.as_deref(),
            1,
            check.step_path.iter().map(String::as_str),
        );
        if self.detail == ReportDetail::Full {
            self.checks.push(check);
        }
    }

    fn record<'a>(
        &mut self,
        code: &str,
        status: ValidationStatus,
        message: &str,
        remediation: Option<&str>,
        occurrences: usize,
        paths: impl IntoIterator<Item = &'a str>,
    ) {
        if rank(status) > rank(self.status) {
            self.status = status;
        }
        if status == ValidationStatus::Passed {
            self.passed += occurrences;
            return;
        }
        if status != ValidationStatus::DeferredToDispatch
            && (status != ValidationStatus::Failed || !authority_failure_code(code))
        {
            self.only_authority_failures = false;
        }
        let key = (
            rank(status),
            code.to_string(),
            message.to_string(),
            remediation.map(str::to_string),
        );
        let index = *self.groups.entry(key).or_insert_with(|| {
            let index = self.findings.len();
            self.findings.push(WorkflowCheckFinding {
                code: code.to_string(),
                status,
                message: message.to_string(),
                remediation: remediation.map(str::to_string),
                occurrences: 0,
                example_step_paths: Vec::new(),
            });
            index
        });
        let finding = &mut self.findings[index];
        finding.occurrences += occurrences;
        for path in paths {
            if finding.example_step_paths.len() == 3 {
                break;
            }
            if !finding
                .example_step_paths
                .iter()
                .any(|example| example == path)
            {
                finding.example_step_paths.push(path.to_string());
            }
        }
    }

    pub(super) fn preflight(&mut self, check: transist_core::PreflightCheck) {
        let status = match check.status {
            transist_core::PreflightStatus::Passed => ValidationStatus::Passed,
            transist_core::PreflightStatus::DeferredToDispatch => {
                ValidationStatus::DeferredToDispatch
            }
            transist_core::PreflightStatus::Warning => ValidationStatus::Warning,
            transist_core::PreflightStatus::Failed => ValidationStatus::Failed,
        };
        // Consume shared evidence directly: Summary never expands per-step ValidationChecks.
        self.record(
            &check.kind,
            status,
            &check.message,
            None,
            check.step_ids.len().max(1),
            check.step_ids.iter().map(String::as_str),
        );
        if self.detail == ReportDetail::Full {
            self.checks.extend(validation_checks_from_preflight(check));
        }
    }

    pub(super) fn summary(
        &self,
        mode: WorkflowCheckMode,
        workflow_ref: String,
    ) -> WorkflowCheckSummary {
        WorkflowCheckSummary {
            mode,
            evaluation_mode: if mode == WorkflowCheckMode::Readiness {
                WorkflowEvaluationMode::LiveReadiness
            } else {
                WorkflowEvaluationMode::Validation
            },
            validation_status: self.status,
            workflow_ref,
            workspace_dir: None,
            status: self.status,
            readiness_status: match mode {
                WorkflowCheckMode::Readiness => Some(readiness_status(self.status)),
                _ => None,
            },
            passed_checks: self.passed,
            findings: self.findings.clone(),
            required_input_names: Vec::new(),
            missing_capability_count: 0,
            policy_suggestion: None,
            authorization_eligible: false,
        }
    }

    pub(super) fn eligible(&self, missing: usize) -> bool {
        missing > 0 && self.only_authority_failures
    }
}

pub(super) struct BoundEvaluation {
    pub(super) summary: WorkflowCheckSummary,
    pub(super) full: Option<BoundWorkflowValidation>,
    // Transient ordered authority for authorization identity, never part of a summary report.
    pub(super) missing: Vec<CapabilityRequest>,
}

pub(super) struct AuthoredEvaluation {
    pub(super) summary: WorkflowCheckSummary,
    pub(super) full: Option<AuthoredWorkflowValidation>,
}

/// Validates one captured definition with the selected retention level, without writes.
pub(super) fn evaluate_authored_workflow(
    home_dir: &Path,
    flow_path: &Path,
    detail: ReportDetail,
) -> Result<AuthoredEvaluation> {
    let snapshot = transist_compiler::capture_workflow_definition(flow_path)?;
    transist_compiler::validate_workflow_snapshot_in_home(home_dir, &snapshot)?;
    let flow = snapshot.top_level_flow()?;
    let workflow_ref = format!("{}@{}", flow.id, flow.version);
    let mut checks = CheckCollector::new(detail);
    for step in &flow.steps {
        let mut values = step.with.clone();
        if let Some(call) = &step.subflow {
            values.extend(call.inputs.clone());
        }
        if let Some(prompt) = &step.prompt {
            values.insert("prompt".into(), yaml_serde::Value::String(prompt.clone()));
        }
        let value = KernelValue::from_json(serde_json::to_value(values)?);
        if !transist_protocol::collect_deferred_bindings(&value, "")
            .map_err(anyhow::Error::msg)?
            .is_empty()
        {
            checks.push(validation_check(
                "deferred_to_dispatch",
                ValidationStatus::DeferredToDispatch,
                Some(step.id.clone()),
                "declared action data is materialized and validated before consumer dispatch",
                None,
            ));
        }
    }
    checks.push(validation_check(
        "authored_definition",
        ValidationStatus::Passed,
        None,
        "captured workflow definition passed authored validation",
        None,
    ));
    let required_input_names = flow
        .inputs
        .iter()
        .filter(|(_, spec)| spec.required && spec.default.is_none())
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for name in &required_input_names {
        checks.push(validation_check(
            "required_input",
            ValidationStatus::NotEvaluated,
            None,
            format!("required workflow input '{name}' is not evaluated in authored mode"),
            Some(format!(
                "provide '{name}' with --input or --inputs-file for bound validation"
            )),
        ));
    }
    let mut summary = checks.summary(WorkflowCheckMode::Authored, workflow_ref.clone());
    summary.required_input_names = required_input_names;
    let full = if detail == ReportDetail::Full {
        #[cfg(test)]
        FULL_RESULTS.with(|count| count.set(count.get() + 1));
        Some(AuthoredWorkflowValidation {
            workflow_ref,
            status: checks.status,
            checks: checks.checks,
        })
    } else {
        None
    };
    Ok(AuthoredEvaluation { summary, full })
}

#[allow(clippy::too_many_arguments)]
/// Composes availability and optional live ports, evaluating inputs with the selected retention level.
pub(super) fn evaluate_workflow_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    modes: runs::execution::RunModes,
    evaluation_mode: WorkflowEvaluationMode,
    detail: ReportDetail,
) -> Result<BoundEvaluation> {
    #[cfg(test)]
    if evaluation_mode == WorkflowEvaluationMode::LiveReadiness {
        LIVE_EVALUATIONS.with(|count| count.set(count.get() + 1));
    }
    let router = if evaluation_mode == WorkflowEvaluationMode::LiveReadiness {
        Some(router_cache.get_or_insert(home_dir)?)
    } else {
        None
    };
    let (_, secrets) = crate::composition::runtime_policy_and_secrets_with_modes(
        home_dir,
        crate::runtime_owner_identity()?,
        None,
        modes.local_only,
    )?;
    validate_bound_workflow_in_home_inner(
        repo_root,
        home_dir,
        workflow_ref,
        workflow_inputs,
        modes.trusted_execution,
        modes.local_only,
        router.as_deref().map(|router| router as &dyn PreflightPort),
        Some(secrets.as_ref()),
        detail,
    )
}

#[allow(clippy::too_many_arguments)]
/// Shared static/live evaluator; only Full constructs complete bound-validation evidence.
pub(super) fn validate_bound_workflow_in_home_inner(
    repo_root: &Path,
    home_dir: &Path,
    workflow_ref: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    trusted_execution: bool,
    local_only: bool,
    preflight: Option<&dyn PreflightPort>,
    secrets: Option<&dyn SecretProviderPort>,
    detail: ReportDetail,
) -> Result<BoundEvaluation> {
    let measurement_scope = transist_core::PreflightMeasurementScope::ApplicationReadiness;
    let _total = transist_core::phase_span(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::Total,
    );
    let (recipe, recipe_path, recipe_digest) = transist_core::measure_preflight_phase(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::RecipeLoad,
        || load_compiled_recipe_with_digest(home_dir, workflow_ref),
    )?;
    let catalog = validation_reuse::catalog(home_dir, &recipe, &recipe_digest)?;
    let plugin_metadata =
        LocalPluginMetadata::with_catalog(home_dir.to_path_buf(), std::sync::Arc::clone(&catalog));
    let bound = transist_core::measure_preflight_phase(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::Binding,
        || -> Result<_> {
            transist_plugins::record_validation_work(
                transist_plugins::ValidationWork::InputBinding,
            );
            let inputs = bind_workflow_inputs(&recipe, &recipe_path, workflow_inputs)?;
            bind_and_materialize_recipe(
                home_dir,
                repo_root,
                &recipe,
                inputs,
                local_only,
                &LocalHomeSettings::new(),
                &plugin_metadata,
            )
        },
    )?;
    let prompt_files = LocalPromptFiles::new();
    let mut invocations = Vec::new();
    let mut checks = CheckCollector::new(detail);
    let effective_trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, trusted_execution)?;
    let trust = transist_core::measure_preflight_phase(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::Trust,
        || -> Result<_> {
            if effective_trusted_execution {
                let trust = crate::trusted_execution::resolve_trusted_execution_with_catalog(
                    repo_root,
                    home_dir,
                    "static-validation",
                    &bound.recipe,
                    effective_trusted_execution,
                    Some(&catalog),
                )?;
                let decision = trust.decision.as_ref();
                checks.push(validation_check(
                    "trusted_execution",
                    if decision.is_some_and(|decision| decision.denied()) {
                        ValidationStatus::Failed
                    } else {
                        ValidationStatus::Passed
                    },
                    None,
                    decision.map_or_else(
                        || "trusted execution is enabled".to_string(),
                        |decision| {
                            format!(
                                "trusted execution decision '{}' (code={})",
                                decision.decision,
                                decision.reason_code.as_deref().unwrap_or("allowed")
                            )
                        },
                    ),
                    None,
                ));
                Ok(trust)
            } else {
                Ok(crate::trusted_execution::disabled_resolution())
            }
        },
    )?;
    let policy_measurement = transist_core::phase_span(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::Policy,
    );
    let policy = crate::composition::runtime_policy_with_modes(
        home_dir,
        trust.decision.clone().filter(|decision| !decision.denied()),
        local_only,
    )?;
    let root_workflow_ref = format!(
        "{}@{}",
        bound.recipe.workflow.id, bound.recipe.workflow.version
    );
    match policy.validate_run_start(&bound.workspace_dir, &root_workflow_ref) {
        Ok(run_start) => checks.push(validation_check(
            "run_policy",
            if run_start.allowed {
                ValidationStatus::Passed
            } else {
                ValidationStatus::Failed
            },
            None,
            if run_start.allowed {
                "workflow start is allowed by policy".to_string()
            } else {
                format!(
                    "workflow start denied by policy (code={}, message={})",
                    run_start.reason_code.as_deref().unwrap_or("policy_denied"),
                    run_start.message.as_deref().unwrap_or("denied")
                )
            },
            None,
        )),
        Err(error) => checks.push(policy_error_check("run_policy", None, error)),
    }
    let policy_snapshot = crate::runtime_policy::load_capability_policy_snapshot(home_dir)?;
    transist_plugins::record_validation_work(transist_plugins::ValidationWork::PolicyEvaluation);
    let (exact_grants, plugin_grants) =
        transist_policy::effective_grants_for_workflow(&policy_snapshot.allow, &root_workflow_ref)?;
    let mut required = BTreeSet::new();
    let mut granted = BTreeSet::new();
    let mut descriptor_missing = BTreeSet::new();
    collect_static_recipe_validation(
        &bound.recipe,
        &bound.inputs,
        &BTreeSet::new(),
        &bound.workspace_dir,
        home_dir,
        local_only,
        &plugin_metadata,
        &prompt_files,
        &root_workflow_ref,
        "",
        policy.as_ref(),
        &exact_grants,
        &plugin_grants,
        &mut invocations,
        &mut checks,
        &mut required,
        &mut granted,
        &mut descriptor_missing,
        &mut vec![root_workflow_ref.clone()],
        secrets,
        preflight.is_some(),
    )?;
    drop(policy_measurement);
    let adapter_checks = transist_core::measure_preflight_phase(
        measurement_scope,
        transist_core::PreflightMeasurementPhase::Adapter,
        || {
            transist_core::with_preflight_measurement_scope(measurement_scope, || {
                if let Some(preflight) =
                    preflight.filter(|_| checks.status != ValidationStatus::Failed)
                {
                    preflight.preflight_steps(&invocations)
                } else {
                    transist_adapters::validate_static_preflight_steps_with_catalog(
                        &invocations,
                        &catalog,
                    )
                }
            })
        },
    )?;
    for check in adapter_checks {
        checks.preflight(check);
    }
    let mut projected_required = required;
    let mut projected_granted = granted;
    let mut workflow_stack = vec![bound.recipe.workflow.clone()];
    collect_projected_capability_requirements(
        &bound.recipe,
        &bound.inputs,
        &BTreeSet::new(),
        &bound.workspace_dir,
        &exact_grants,
        &plugin_grants,
        &mut projected_required,
        &mut projected_granted,
        0,
        &mut workflow_stack,
    )?;
    let missing_capabilities = if policy_snapshot.enforcement == "enforced" {
        projected_required
            .iter()
            .filter(|request| !projected_granted.contains(request))
            .cloned()
            .chain(descriptor_missing)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let policy_suggestion = runs::capability_policy_hint(
        &root_workflow_ref,
        &bound.recipe.workflow.id,
        &missing_capabilities,
        &plugin_grants,
    );
    let mode = if preflight.is_some() {
        WorkflowCheckMode::Readiness
    } else {
        WorkflowCheckMode::Bound
    };
    let mut summary = checks.summary(mode, root_workflow_ref.clone());
    if preflight.is_none() && secrets.is_some() {
        summary.readiness_status = Some(validation_readiness_status(checks.status));
    }
    summary.workspace_dir = Some(bound.workspace_dir.clone());
    summary.required_input_names = recipe
        .inputs
        .iter()
        .filter(|(_, spec)| spec.required && spec.default.is_none())
        .map(|(name, _)| name.clone())
        .collect();
    summary.missing_capability_count = missing_capabilities.len();
    summary.authorization_eligible = checks.eligible(missing_capabilities.len());
    summary.policy_suggestion = Some(policy_suggestion.clone());
    // These constructors and capability report copies must remain inside the Full branch.
    let full = if detail == ReportDetail::Full {
        #[cfg(test)]
        FULL_RESULTS.with(|count| count.set(count.get() + 1));
        Some(BoundWorkflowValidation {
            workflow_ref: root_workflow_ref,
            workspace_dir: bound.workspace_dir,
            status: checks.status,
            checks: checks.checks,
            required_capabilities: projected_required.into_iter().collect(),
            granted_capabilities: projected_granted.into_iter().collect(),
            missing_capabilities: missing_capabilities.clone(),
            policy_suggestion,
        })
    } else {
        None
    };
    Ok(BoundEvaluation {
        summary,
        full,
        missing: missing_capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_validation_does_not_compose_live_ports_or_full_summary_evidence() {
        let fixture = Fixture::new("", "  - id: finish\n    control: control.finish\n", false);
        fixture.compile();
        LIVE_EVALUATIONS.with(|count| count.set(0));
        FULL_RESULTS.with(|count| count.set(0));
        let summary = fixture
            .context
            .workflow_validation_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert!(summary.is_validation_passed());
        LIVE_EVALUATIONS.with(|count| assert_eq!(count.get(), 0));
        FULL_RESULTS.with(|count| assert_eq!(count.get(), 0));
        let (full, compact) = fixture
            .context
            .workflow_validation_report_with_summary(
                "report.demo@v1",
                BTreeMap::new(),
                false,
                false,
            )
            .unwrap();
        assert_eq!(compact, summary);
        assert!(full.is_validation_passed());
        assert!(!full.is_ready());
        LIVE_EVALUATIONS.with(|count| assert_eq!(count.get(), 0));
        FULL_RESULTS.with(|count| assert_eq!(count.get(), 1));
        fixture
            .context
            .workflow_readiness_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        LIVE_EVALUATIONS.with(|count| assert_eq!(count.get(), 1));
    }

    #[test]
    fn authorization_validation_zero_live_evaluations_and_no_full_compact_results() {
        let fixture = authorization_fixture();
        LIVE_EVALUATIONS.with(|count| count.set(0));
        FULL_RESULTS.with(|count| count.set(0));
        let reviewed = fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, false)
            .unwrap();
        fixture
            .context
            .apply_reviewed_workflow_authorization_summary(BTreeMap::new(), &reviewed)
            .unwrap();
        let before = fs::read_dir(fixture.home.join("policy/workflow-authorizations"))
            .unwrap()
            .count();
        for apply in [false, true] {
            let summary = fixture
                .context
                .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, apply)
                .unwrap();
            assert_eq!(
                summary.status,
                WorkflowAuthorizationStatus::AlreadyAuthorized
            );
        }
        FULL_RESULTS.with(|count| assert_eq!(count.get(), 0));
        for apply in [false, true] {
            let full = fixture
                .context
                .authorize_workflow("report.demo@v1", BTreeMap::new(), None, apply)
                .unwrap();
            assert_eq!(full.status, WorkflowAuthorizationStatus::AlreadyAuthorized);
        }
        LIVE_EVALUATIONS.with(|count| assert_eq!(count.get(), 0));
        assert_eq!(
            fs::read_dir(fixture.home.join("policy/workflow-authorizations"))
                .unwrap()
                .count(),
            before
        );
    }

    #[test]
    fn readiness_structural_counters_cover_summary_and_incremental_full_boundaries() {
        let fixture = Fixture::new(
            "",
            "  - id: gate\n    control: control.gate.manual\n    with:\n      gate_id: review\n",
            true,
        );
        fixture.compile();
        fs::create_dir_all(fixture.home.join("policy/workflow-authorizations")).unwrap();
        fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
        FULL_RESULTS.with(|count| count.set(0));
        READINESS_RESULTS.with(|count| count.set(0));
        AUTHORIZATION_RESULTS.with(|count| count.set(0));
        fixture
            .context
            .validate_flow_file_summary(&fixture.workflow)
            .unwrap();
        fixture
            .context
            .validate_bound_workflow_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        fixture
            .context
            .workflow_readiness_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        fixture
            .context
            .workflow_readiness_summary("absent@v1", BTreeMap::new(), false, false)
            .unwrap();
        fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, false)
            .unwrap();
        fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, true)
            .unwrap();
        fixture
            .context
            .authorize_workflow_summary("absent@v1", BTreeMap::new(), None, false)
            .unwrap();
        FULL_RESULTS.with(|count| assert_eq!(count.get(), 0));
        READINESS_RESULTS.with(|count| assert_eq!(count.get(), 0));
        AUTHORIZATION_RESULTS.with(|count| assert_eq!(count.get(), 0));
        LIVE_EVALUATIONS.with(|count| count.set(0));
        let (full, summary) = fixture
            .context
            .workflow_readiness_report_with_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert_eq!(full.status, summary.readiness_status.unwrap());
        FULL_RESULTS.with(|count| assert_eq!(count.get(), 1));
        READINESS_RESULTS.with(|count| assert_eq!(count.get(), 1));
        LIVE_EVALUATIONS.with(|count| assert_eq!(count.get(), 1));
    }

    fn check(code: &str, status: ValidationStatus, path: &str) -> ValidationCheck {
        validation_check(
            code,
            status,
            Some(path.to_string()),
            "diagnostic",
            Some("remedy".to_string()),
        )
    }

    fn authorization_fixture() -> Fixture {
        let fixture = Fixture::new(
            "",
            "  - id: review\n    control: control.gate.manual\n    with:\n      gate_id: review\n",
            true,
        );
        fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflows, workflow-authorizations]\n").unwrap();
        fs::create_dir_all(fixture.home.join("policy/workflows")).unwrap();
        fs::create_dir_all(fixture.home.join("policy/workflow-authorizations")).unwrap();
        fixture.compile();
        fixture
    }

    #[test]
    fn authorization_facades_preserve_plan_bytes_destination_digest_and_application() {
        let fixture = authorization_fixture();
        let full = fixture
            .context
            .authorize_workflow("report.demo@v1", BTreeMap::new(), None, false)
            .unwrap();
        let summary = fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, false)
            .unwrap();
        assert_eq!(summary.status, WorkflowAuthorizationStatus::Planned);
        assert_eq!(summary.status, full.status);
        assert_eq!(summary.policy, full.policy);
        assert_eq!(summary.policy_output, full.policy_output);
        assert_eq!(
            yaml_serde::to_string(&summary.policy).unwrap(),
            yaml_serde::to_string(&full.policy).unwrap()
        );
        let missing = &full
            .readiness
            .validation
            .as_ref()
            .unwrap()
            .missing_capabilities;
        let digest: String =
            Sha256::digest(serde_json::to_vec(&(&full.workflow, missing)).unwrap())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
        assert!(
            summary
                .policy_output
                .as_ref()
                .unwrap()
                .ends_with(format!("report.demo-v1-{digest}.yaml"))
        );
        assert!(
            summary.policy.as_ref().unwrap()["grant_sets"]
                .get(format!("authorization-{}", &digest[..16]))
                .is_some()
        );
        let evaluation = validate_bound_workflow_in_home_inner(
            &fixture.workspace,
            &fixture.home,
            "report.demo@v1",
            BTreeMap::new(),
            false,
            false,
            None,
            None,
            ReportDetail::Summary,
        )
        .unwrap();
        assert!(evaluation.full.is_none());
        assert_eq!(&evaluation.missing, missing);
        // Full and compact publication produce identical receipts from the same plan.
        let full_applied =
            apply_workflow_authorization_in_home(&fixture.home, full.clone()).unwrap();
        fs::remove_file(full_applied.policy_output.as_ref().unwrap()).unwrap();
        let applied = fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, true)
            .unwrap();
        assert_eq!(applied.status, WorkflowAuthorizationStatus::Applied);
        assert_eq!(applied.status, full_applied.status);
        assert_eq!(applied.policy, full_applied.policy);
        assert_eq!(applied.policy_output, full_applied.policy_output);
        assert_eq!(applied.policy, summary.policy);
        assert_eq!(
            fs::read(applied.policy_output.unwrap()).unwrap(),
            yaml_serde::to_string(&summary.policy.unwrap())
                .unwrap()
                .as_bytes()
        );
        assert!(fixture.context.runs_list().unwrap().is_empty());
        for apply in [false, true] {
            let summary = fixture
                .context
                .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), None, apply)
                .unwrap();
            let full = fixture
                .context
                .authorize_workflow("report.demo@v1", BTreeMap::new(), None, apply)
                .unwrap();
            assert_eq!(
                summary.status,
                WorkflowAuthorizationStatus::AlreadyAuthorized
            );
            assert_eq!(summary.status, full.status);
            assert_eq!(summary.policy, full.policy);
            assert_eq!(summary.policy_output, full.policy_output);
        }
    }

    #[test]
    fn authorization_blockers_and_unavailable_destinations_match_full() {
        let fixture = authorization_fixture();
        for output in [
            Path::new("uncovered/proposal.yaml"),
            Path::new("../escape.yaml"),
            Path::new("bad.json"),
        ] {
            let full = fixture
                .context
                .authorize_workflow("report.demo@v1", BTreeMap::new(), Some(output), false)
                .unwrap_err();
            let summary = fixture
                .context
                .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), Some(output), false)
                .unwrap_err();
            assert_eq!(summary.to_string(), full.to_string());
        }
        let output = fixture
            .home
            .join("policy/workflow-authorizations/conflict.yaml");
        fs::write(&output, b"operator-owned bytes").unwrap();
        let full = fixture
            .context
            .authorize_workflow("report.demo@v1", BTreeMap::new(), Some(&output), true)
            .unwrap();
        let summary = fixture
            .context
            .authorize_workflow_summary("report.demo@v1", BTreeMap::new(), Some(&output), true)
            .unwrap();
        assert_eq!(summary.status, WorkflowAuthorizationStatus::Blocked);
        assert_eq!(summary.status, full.status);
        assert_eq!(summary.policy, full.policy);
        assert_eq!(summary.policy_output, full.policy_output);
        assert_eq!(fs::read(&output).unwrap(), b"operator-owned bytes");
        for workflow in ["missing@v1", "invalid"] {
            let full = fixture
                .context
                .authorize_workflow(workflow, BTreeMap::new(), None, true)
                .unwrap();
            let summary = fixture
                .context
                .authorize_workflow_summary(workflow, BTreeMap::new(), None, true)
                .unwrap();
            assert_eq!(summary.status, WorkflowAuthorizationStatus::Blocked);
            assert_eq!(summary.status, full.status);
            assert!(summary.policy.is_none());
            assert!(summary.policy_output.is_none());
        }
        assert!(fixture.context.runs_list().unwrap().is_empty());
    }

    #[test]
    fn authorization_mixed_partial_authority_preserves_order_and_exact_fragment() {
        let fixture = authorization_fixture();
        let required = [
            (CapabilityName::Plugin, "demo.plugin@v123", "load"),
            (CapabilityName::Action, "demo.plugin@v123/read", "invoke"),
            (CapabilityName::Action, "demo.plugin@v123/write", "invoke"),
            (CapabilityName::Filesystem, "file:///exact/path", "read"),
            (
                CapabilityName::Network,
                "https://example.test:443",
                "connect",
            ),
            (CapabilityName::Secret, "secret://exact-token", "read"),
            (CapabilityName::Approval, "review", "request"),
        ]
        .map(|(capability, resource, operation)| CapabilityRequest {
            capability,
            resource: resource.into(),
            operations: vec![operation.into()],
        });
        let configured = BTreeSet::from([transist_policy::PluginCapabilityGrant::new(
            "demo.plugin@v123",
            ["read".to_string()],
        )
        .unwrap()]);
        // Existing selected-action authority is excluded; preserve the evaluator's ordered remainder.
        let missing = required
            .iter()
            .filter(|r| {
                r.resource != "demo.plugin@v123/read" && r.capability != CapabilityName::Plugin
            })
            .cloned()
            .collect::<Vec<_>>();
        let suggestion =
            runs::capability_policy_hint("report.demo@v1", "report.demo", &missing, &configured);
        let mut collector = CheckCollector::new(ReportDetail::Summary);
        collector.push(check(
            "capability_not_granted",
            ValidationStatus::Failed,
            "nested/step",
        ));
        let mut summary = collector.summary(WorkflowCheckMode::Readiness, "report.demo@v1".into());
        summary.missing_capability_count = missing.len();
        summary.authorization_eligible = true;
        summary.policy_suggestion = Some(suggestion.clone());
        let readiness = WorkflowReadinessReport::from_validation(BoundWorkflowValidation {
            workflow_ref: summary.workflow_ref.clone(),
            workspace_dir: fixture.workspace.clone(),
            status: summary.status,
            checks: vec![check(
                "capability_not_granted",
                ValidationStatus::Failed,
                "nested/step",
            )],
            required_capabilities: required.to_vec(),
            granted_capabilities: vec![required[1].clone()],
            missing_capabilities: missing.clone(),
            policy_suggestion: suggestion,
        });
        let full = plan_workflow_authorization_in_home(&fixture.home, readiness, None).unwrap();
        let compact =
            authorize_summary_in_home(&fixture.home, summary.clone(), &missing, None, false)
                .unwrap();
        assert_eq!(compact.status, full.status);
        assert_eq!(compact.policy, full.policy);
        assert_eq!(compact.policy_output, full.policy_output);
        let fragment = compact.policy.unwrap();
        let grants = fragment["grant_sets"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(
            grants["plugin_grants"],
            serde_json::json!([{"plugin":"demo.plugin@v123", "actions":["write"]}])
        );
        assert_eq!(
            grants["capabilities"],
            serde_json::to_value(&missing[1..]).unwrap()
        );
        assert_eq!(fragment["workflow_grants"][0]["workflow"], "report.demo@v1");
        let mut blocked = summary;
        blocked.authorization_eligible = false;
        assert_eq!(
            authorize_summary_in_home(&fixture.home, blocked, &missing, None, true)
                .unwrap()
                .status,
            WorkflowAuthorizationStatus::Blocked
        );
    }

    #[test]
    fn authorization_publication_is_no_clobber_and_rolls_back_invalid_policy() {
        let fixture = authorization_fixture();
        let mut report = fixture
            .context
            .authorize_workflow("report.demo@v1", BTreeMap::new(), None, false)
            .unwrap();
        let output = report.policy_output.clone().unwrap();
        // Declining is a caller-owned decision; shared application must never publish it.
        report.status = WorkflowAuthorizationStatus::Declined;
        assert_eq!(
            apply_workflow_authorization_in_home(&fixture.home, report.clone())
                .unwrap()
                .status,
            WorkflowAuthorizationStatus::Declined
        );
        assert!(!output.exists());
        report.status = WorkflowAuthorizationStatus::Planned;
        fs::write(&output, b"racing operator").unwrap();
        assert!(apply_workflow_authorization_in_home(&fixture.home, report.clone()).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"racing operator");
        fs::remove_file(&output).unwrap();
        report.policy = Some(serde_json::json!({"schema":"invalid"}));
        assert!(apply_workflow_authorization_in_home(&fixture.home, report).is_err());
        assert!(!output.exists());
        assert!(
            publish_authorization(
                &fixture.home,
                Some(&serde_json::json!({"schema":"invalid"})),
                Some(&output)
            )
            .is_err()
        );
        assert!(!output.exists());
    }

    #[test]
    fn synthetic_streams_bound_retention_and_preserve_full_order() {
        for count in [10, 1_000, 10_000] {
            let mut summary = CheckCollector::new(ReportDetail::Summary);
            let mut full = CheckCollector::new(ReportDetail::Full);
            for index in 0..count {
                let path = format!("parent/child/step-{index}");
                for finding in [
                    check("passing", ValidationStatus::Passed, &path),
                    check("unknown.future", ValidationStatus::Failed, &path),
                ] {
                    summary.push(finding.clone());
                    full.push(finding);
                }
            }
            assert!(summary.checks.is_empty());
            assert_eq!(summary.checks.capacity(), 0);
            assert_eq!(summary.passed, count);
            assert_eq!(summary.findings.len(), 1);
            assert_eq!(summary.findings[0].occurrences, count);
            assert_eq!(
                summary.findings[0].example_step_paths,
                [
                    "parent/child/step-0",
                    "parent/child/step-1",
                    "parent/child/step-2"
                ]
            );
            assert_eq!(
                summary.summary(WorkflowCheckMode::Bound, "demo@v1".into()),
                full.summary(WorkflowCheckMode::Bound, "demo@v1".into())
            );
            assert_eq!(full.checks.len(), 2 * count);
            assert_eq!(
                full.checks[2 * count - 1].step_path.as_deref(),
                Some(format!("parent/child/step-{}", count - 1).as_str())
            );
        }
    }

    #[test]
    fn status_precedence_and_exact_eligibility_rule() {
        for (stream, expected) in [
            (vec![ValidationStatus::Passed], ValidationStatus::Passed),
            (
                vec![ValidationStatus::NotEvaluated, ValidationStatus::Passed],
                ValidationStatus::NotEvaluated,
            ),
            (
                vec![
                    ValidationStatus::NotEvaluated,
                    ValidationStatus::Warning,
                    ValidationStatus::Passed,
                ],
                ValidationStatus::Warning,
            ),
            (
                vec![
                    ValidationStatus::Warning,
                    ValidationStatus::Failed,
                    ValidationStatus::NotEvaluated,
                ],
                ValidationStatus::Failed,
            ),
        ] {
            let mut summary = CheckCollector::new(ReportDetail::Summary);
            let mut full = CheckCollector::new(ReportDetail::Full);
            for status in stream {
                summary.push(check("step_policy", status, "nested/step"));
                full.push(check("step_policy", status, "nested/step"));
            }
            assert_eq!(summary.status, expected);
            assert_eq!(summary.status, full.status);
            assert_eq!(
                readiness_status(summary.status),
                WorkflowReadinessReport::from_validation(BoundWorkflowValidation {
                    workflow_ref: "demo@v1".into(),
                    workspace_dir: PathBuf::new(),
                    status: full.status,
                    checks: full.checks.clone(),
                    required_capabilities: Vec::new(),
                    granted_capabilities: Vec::new(),
                    missing_capabilities: Vec::new(),
                    policy_suggestion: runs::capability_policy_hint(
                        "demo@v1",
                        "demo",
                        &[],
                        &BTreeSet::new()
                    )
                })
                .status
            );
        }
        for code in [
            "run_policy",
            "step_policy",
            "capability_not_granted",
            "approval_policy",
            "secret_resolution",
            "unknown.future",
        ] {
            let mut summary = CheckCollector::new(ReportDetail::Summary);
            summary.push(check(code, ValidationStatus::Failed, "nested/step"));
            let expected = matches!(
                code,
                "run_policy" | "step_policy" | "capability_not_granted"
            );
            assert_eq!(summary.eligible(1), expected, "{code}");
            assert!(!summary.eligible(0));
        }
    }

    #[test]
    fn grouping_uses_all_four_fields_and_unique_encounter_order() {
        let mut collector = CheckCollector::new(ReportDetail::Summary);
        for path in ["nested/z", "nested/z", "nested/a", "nested/q", "nested/b"] {
            collector.push(check("unknown", ValidationStatus::Warning, path));
        }
        let mut different = check("unknown", ValidationStatus::Warning, "nested/z");
        different.message = "different".into();
        collector.push(different);
        let mut different = check("unknown", ValidationStatus::Warning, "nested/z");
        different.remediation = None;
        collector.push(different);
        collector.push(check("unknown", ValidationStatus::NotEvaluated, "nested/z"));
        collector.push(check("another", ValidationStatus::Warning, "nested/z"));
        assert_eq!(collector.findings.len(), 5);
        assert_eq!(collector.findings[0].occurrences, 5);
        assert_eq!(
            collector.findings[0].example_step_paths,
            ["nested/z", "nested/a", "nested/q"]
        );
    }

    fn shared(
        status: transist_core::PreflightStatus,
        paths: Vec<String>,
    ) -> transist_core::PreflightCheck {
        transist_core::PreflightCheck {
            kind: "adapter_handshake".into(),
            status,
            step_ids: paths,
            adapter_kind: Some("process".into()),
            plugin_id: Some("test".into()),
            action: Some("run".into()),
            message: "shared diagnostic".into(),
        }
    }

    #[test]
    fn shared_adapter_results_are_sampled_without_expansion() {
        for count in [10, 1_000, 10_000] {
            let paths = (0..count)
                .map(|i| format!("nested/step-{i}"))
                .collect::<Vec<_>>();
            let mut summary = CheckCollector::new(ReportDetail::Summary);
            let mut full = CheckCollector::new(ReportDetail::Full);
            for status in [
                transist_core::PreflightStatus::Passed,
                transist_core::PreflightStatus::Warning,
                transist_core::PreflightStatus::Failed,
            ] {
                summary.preflight(shared(status, paths.clone()));
                full.preflight(shared(status, paths.clone()));
            }
            summary.preflight(shared(transist_core::PreflightStatus::Warning, Vec::new()));
            full.preflight(shared(transist_core::PreflightStatus::Warning, Vec::new()));
            assert_eq!(summary.checks.capacity(), 0);
            assert_eq!(summary.passed, count);
            assert_eq!(summary.findings.len(), 2);
            assert_eq!(summary.findings[0].occurrences, count + 1);
            assert_eq!(summary.findings[0].example_step_paths.len(), 3);
            assert_eq!(
                summary.summary(WorkflowCheckMode::Readiness, "demo@v1".into()),
                full.summary(WorkflowCheckMode::Readiness, "demo@v1".into())
            );
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        context: crate::TransistContext,
        workflow: PathBuf,
        home: PathBuf,
        workspace: PathBuf,
    }

    impl Fixture {
        fn new(extra: &str, steps: &str, allowlist: bool) -> Self {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            // Anchored storage rejects symlinked ancestors (/tmp is a symlink on macOS).
            let root = fs::canonicalize(temp.path()).unwrap();
            let home = root.join("home");
            let workspace = root.join("workspace");
            fs::create_dir(&workspace).unwrap();
            crate::init_home_dir_with_mode(&home, crate::RuntimeOperatingMode::Standalone).unwrap();
            if allowlist {
                fs::write(
                    home.join("config.yaml"),
                    "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n",
                )
                .unwrap();
            }
            let context = crate::TransistContext::with_home(&workspace, &home);
            let workflow = root.join("workflow.yaml");
            fs::write(&workflow, format!("schema: transist.flow.v1\nid: report.demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\n{extra}steps:\n{steps}", workspace.display())).unwrap();
            Self {
                _temp: temp,
                context,
                workflow,
                home,
                workspace,
            }
        }

        fn compile(&self) {
            self.context.compile_flow_file(&self.workflow).unwrap();
        }

        fn parity(&self, inputs: BTreeMap<String, KernelValue>) -> WorkflowCheckSummary {
            let summary = self
                .context
                .validate_bound_workflow_summary("report.demo@v1", inputs.clone(), false, false)
                .unwrap();
            let full = self
                .context
                .validate_bound_workflow("report.demo@v1", inputs.clone(), false, false)
                .unwrap();
            assert_eq!(summary.status, full.status);
            assert_eq!(
                summary.missing_capability_count,
                full.missing_capabilities.len()
            );
            assert_eq!(
                summary.policy_suggestion.as_ref(),
                Some(&full.policy_suggestion)
            );
            assert_eq!(
                summary.authorization_eligible,
                !full.missing_capabilities.is_empty()
                    && full
                        .checks
                        .iter()
                        .filter(|check| check.status == ValidationStatus::Failed)
                        .all(|check| matches!(
                            check.code.as_str(),
                            "run_policy" | "step_policy" | "capability_not_granted"
                        ))
            );
            let mut expected = CheckCollector::new(ReportDetail::Summary);
            for check in full.checks {
                expected.push(check);
            }
            assert_eq!(summary.findings, expected.findings);
            assert_eq!(summary.passed_checks, expected.passed);
            let (report, incremental) = self
                .context
                .workflow_readiness_report_with_summary(
                    "report.demo@v1",
                    inputs.clone(),
                    false,
                    false,
                )
                .unwrap();
            let live = self
                .context
                .workflow_readiness_summary("report.demo@v1", inputs, false, false)
                .unwrap();
            assert_eq!(live, incremental);
            assert_eq!(live.readiness_status, Some(report.status));
            assert!(self.context.runs_list().unwrap().is_empty());
            summary
        }
    }

    #[test]
    fn evaluator_parity_for_passing_warning_failed_and_nested_prerequisites() {
        for (prerequisites, status) in [
            ("", ValidationStatus::Passed),
            (
                "prerequisites:\n  paths:\n    - path: absent\n      kind: file\n      required: false\n",
                ValidationStatus::Warning,
            ),
            (
                "prerequisites:\n  paths:\n    - path: absent\n      kind: file\n      required: true\n",
                ValidationStatus::Failed,
            ),
        ] {
            let fixture = Fixture::new(
                prerequisites,
                "  - id: finish\n    control: control.finish\n",
                false,
            );
            fixture.compile();
            assert_eq!(fixture.parity(BTreeMap::new()).status, status);
        }
        let fixture = Fixture::new(
            "modules:\n  child:\n    path: child.yaml\n",
            "  - id: nested\n    module: child\n",
            true,
        );
        fs::write(fixture.workflow.parent().unwrap().join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: review\n    control: control.gate.manual\n    with:\n      gate_id: review\n").unwrap();
        fixture.compile();
        let summary = fixture.parity(BTreeMap::new());
        assert!(summary.authorization_eligible);
        assert!(summary.missing_capability_count > 0);
        assert!(
            summary
                .findings
                .iter()
                .any(|finding| finding.example_step_paths == ["nested/review"])
        );
        // A child prerequisite blocker must suppress its actions and authorization alike.
        let blocked = Fixture::new(
            "modules:\n  child:\n    path: child.yaml\n",
            "  - id: nested\n    module: child\n",
            true,
        );
        fs::write(blocked.workflow.parent().unwrap().join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: v1\nprerequisites:\n  paths:\n    - path: absent\n      kind: file\n      required: true\nsteps:\n  - id: review\n    control: control.gate.manual\n").unwrap();
        blocked.compile();
        let summary = blocked.parity(BTreeMap::new());
        assert_eq!(summary.status, ValidationStatus::Failed);
        assert!(!summary.authorization_eligible);
        assert!(
            summary
                .findings
                .iter()
                .any(|finding| finding.code == "workflow_prerequisite.path"
                    && finding.example_step_paths == ["nested"])
        );
        assert!(summary.findings.iter().all(|finding| {
            !finding
                .example_step_paths
                .iter()
                .any(|path| path == "nested/review")
        }));
    }

    #[test]
    fn summary_evaluators_never_construct_full_results() {
        let fixture = Fixture::new("", "  - id: finish\n    control: control.finish\n", false);
        let authored =
            evaluate_authored_workflow(&fixture.home, &fixture.workflow, ReportDetail::Summary)
                .unwrap();
        assert!(authored.full.is_none());
        fixture.compile();
        let bound = validate_bound_workflow_in_home_inner(
            &fixture.workspace,
            &fixture.home,
            "report.demo@v1",
            BTreeMap::new(),
            false,
            false,
            None,
            None,
            ReportDetail::Summary,
        )
        .unwrap();
        assert!(bound.full.is_none());
        struct Preflight(std::sync::atomic::AtomicUsize);
        impl PreflightPort for Preflight {
            fn preflight_steps(
                &self,
                _: &[PreflightInvocation],
            ) -> Result<Vec<transist_core::PreflightCheck>> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(vec![shared(
                    transist_core::PreflightStatus::Warning,
                    (0..10_000)
                        .map(|index| format!("parent/child/{index}"))
                        .collect(),
                )])
            }
        }
        struct Secrets;
        impl SecretProviderPort for Secrets {
            fn get_secret(&self, _: &str) -> Result<Option<String>> {
                panic!("readiness must not resolve secret values")
            }
            fn has_secret(&self, _: &str) -> Result<bool> {
                Ok(false)
            }
        }
        let preflight = Preflight(std::sync::atomic::AtomicUsize::new(0));
        let summary = validate_bound_workflow_in_home_inner(
            &fixture.workspace,
            &fixture.home,
            "report.demo@v1",
            BTreeMap::new(),
            false,
            false,
            Some(&preflight),
            Some(&Secrets),
            ReportDetail::Summary,
        )
        .unwrap();
        assert!(summary.full.is_none());
        assert_eq!(preflight.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(summary.summary.findings[0].occurrences, 10_000);
        assert_eq!(summary.summary.findings[0].example_step_paths.len(), 3);
        let full = validate_bound_workflow_in_home_inner(
            &fixture.workspace,
            &fixture.home,
            "report.demo@v1",
            BTreeMap::new(),
            false,
            false,
            Some(&preflight),
            Some(&Secrets),
            ReportDetail::Full,
        )
        .unwrap();
        assert_eq!(
            preflight.0.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "one preflight per evaluation, including Full plus its summary"
        );
        assert_eq!(summary.summary, full.summary);
        assert_eq!(full.full.unwrap().checks.len(), 10_001);
    }

    #[test]
    fn runtime_dependent_findings_and_external_state_have_parity() {
        let fixture = Fixture::new(
            "plugins:\n  shell: 0.1.0\n",
            "  - id: produce\n    uses: process:shell/echo\n    with:\n      args: [seed]\n    set:\n      dynamic: '{{steps.produce.out.stdout}}'\n  - id: consume\n    uses: process:shell/echo\n    with:\n      args: ['{{vars.dynamic}}']\n",
            false,
        );
        fixture
            .context
            .install_plugin_file(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
            )
            .unwrap();
        fixture.compile();
        let summary = fixture.parity(BTreeMap::new());
        assert_eq!(summary.status, ValidationStatus::NotEvaluated);
        assert!(
            summary
                .findings
                .iter()
                .any(|finding| finding.code == "mutable_variable"
                    && finding.example_step_paths == ["consume"])
        );
        assert!(
            summary
                .findings
                .iter()
                .any(|finding| finding.code == "external_state")
        );
    }

    #[test]
    fn required_input_names_and_invalid_definitions_never_retain_values() {
        let fixture = Fixture::new(
            "inputs:\n  zebra:\n    type: string\n    required: true\n  alpha:\n    type: string\n    required: true\n  optional:\n    type: string\n    default: private-default\n",
            "  - id: finish\n    control: control.finish\n",
            false,
        );
        let summary = fixture
            .context
            .validate_flow_file_summary(&fixture.workflow)
            .unwrap();
        let full = fixture
            .context
            .validate_flow_file(&fixture.workflow)
            .unwrap();
        assert_eq!(summary.status, full.status);
        assert_eq!(summary.required_input_names, ["alpha", "zebra"]);
        assert_eq!(summary.passed_checks, 1);
        fixture.compile();
        let invalid = fixture
            .context
            .workflow_readiness_summary("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        let full = fixture
            .context
            .workflow_readiness_report("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert_eq!(invalid.readiness_status, Some(full.status));
        assert_eq!(invalid.required_input_names, ["alpha"]);
        assert!(!invalid.authorization_eligible);
        let inputs = BTreeMap::from([
            ("alpha".into(), KernelValue::String(String::new())),
            (
                "zebra".into(),
                KernelValue::String("private-supplied".into()),
            ),
        ]);
        let summary = fixture.parity(inputs);
        assert_eq!(summary.required_input_names, ["alpha", "zebra"]);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("private-supplied"));
        assert!(!json.contains("private-default"));
        let unknown = fixture
            .context
            .workflow_readiness_summary("unknown@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert_eq!(
            unknown.readiness_status,
            Some(WorkflowReadinessStatus::InvalidDefinition)
        );
        assert_eq!(unknown.findings[0].code, "bound_validation");
        fs::write(&fixture.workflow, "invalid: definition\n").unwrap();
        assert!(
            fixture
                .context
                .validate_flow_file_summary(&fixture.workflow)
                .is_err()
        );
        assert!(
            fixture
                .context
                .validate_flow_file(&fixture.workflow)
                .is_err()
        );
    }

    #[test]
    fn ordered_full_json_contract_fixture() {
        let fixture = Fixture::new(
            "inputs:\n  zebra:\n    type: string\n    required: true\n  alpha:\n    type: string\n    required: true\n",
            "  - id: finish\n    control: control.finish\n",
            false,
        );
        let full = fixture
            .context
            .validate_flow_file(&fixture.workflow)
            .unwrap();
        assert_eq!(
            serde_json::to_value(full).unwrap(),
            serde_json::json!({
                "workflow_ref": "report.demo@v1", "status": "not_evaluated", "checks": [
                    {"code": "authored_definition", "status": "passed", "message": "captured workflow definition passed authored validation"},
                    {"code": "required_input", "status": "not_evaluated", "message": "required workflow input 'alpha' is not evaluated in authored mode", "remediation": "provide 'alpha' with --input or --inputs-file for bound validation"},
                    {"code": "required_input", "status": "not_evaluated", "message": "required workflow input 'zebra' is not evaluated in authored mode", "remediation": "provide 'zebra' with --input or --inputs-file for bound validation"}
                ]
            })
        );
        fixture.compile();
        let full = fixture
            .context
            .workflow_readiness_report("report.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert_eq!(
            serde_json::to_value(full).unwrap(),
            serde_json::json!({ "schema": "transist.workflow_readiness.v1", "workflow": "report.demo@v1", "status": "invalid_definition", "evaluation_mode": "live_readiness", "validation_status": "failed", "run_created": false, "checks": [{ "code": "bound_validation", "status": "failed", "message": "missing required workflow input 'alpha'" }] })
        );
    }
}
