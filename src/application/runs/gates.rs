// SPDX-License-Identifier: MPL-2.0
//! Manual gate decision machinery.
//!
//! Owns the typed [`GateDecision`] surface, the single core
//! [`decide_gate_in_home_with_cache`] entry point used by [`RunService`], and the helpers
//! that resolve currently blocked gates and persist gate decision records.
//!
//! Stringly-typed decision values (`"approved"`, `"rejected"`,
//! `"request_changes"`) are confined to the persistence boundary; orchestration
//! code in this module stays type-safe via [`GateDecision`].

use crate::composition::router_cache::RouterCache;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use transist_core::LeaseReleaseWarning;
use transist_protocol::{GateDecisionRecord, KernelValue, Recipe, RecipeStep};

use super::execution;
use super::helpers::{
    child_run_id_for_step_state, load_persisted_recipe, next_actionable_subflow_run,
    resume_or_reenter_run_with_cache,
};
use super::{RunReport, reenter_run_path_with_cache, resolve_actionable_failed_run_path};

/// Internal typed representation of a manual gate decision.
///
/// Keeps stringly-typed values (`"approved"`, `"rejected"`, `"request_changes"`)
/// confined to the persistence boundary while orchestration code stays type-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Gate approved; the run continues past the gate on resume.
    Approved,
    /// Gate rejected; the run is marked failed at the gate.
    Rejected,
    /// Reviewer requested changes; the run is marked failed pending edits.
    RequestChanges,
}

impl GateDecision {
    /// Canonical persisted form written to gate decision records.
    fn as_persisted_str(self) -> &'static str {
        match self {
            GateDecision::Approved => "approved",
            GateDecision::Rejected => "rejected",
            GateDecision::RequestChanges => "request_changes",
        }
    }
}

/// User-supplied metadata attached to a new manual-gate decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateDecisionInput {
    /// Optional actor identity for the decision record.
    pub actor: Option<String>,
    /// Optional source label that explains where the actor came from.
    pub actor_source: Option<String>,
    /// Optional human rationale. New decisions require a non-empty value.
    pub reason: Option<String>,
    /// Optional precomputed timestamp. When absent, the runtime stamps `now`.
    pub decided_at_ms: Option<u64>,
}

/// Resolves the effective manual-gate actor identity for the given home using
/// the same fallback order as gate decision validation.
pub fn resolve_gate_actor_identity_in_home(home_dir: &Path) -> Result<Option<String>> {
    let actor_env = std::env::var("TRANSIST_ACTOR").ok();
    let (actor, _, _) = resolve_gate_decision_input_sources(
        home_dir,
        None,
        None,
        None,
        actor_env.as_deref(),
        None,
    )?;
    Ok(actor)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockedGateResolution {
    pub(super) run_id: String,
    pub(super) step_id: String,
    pub(super) gate_id: String,
}

/// Result of recording a gate decision without re-entering its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDecisionReport {
    /// Root run id to target for detached re-entry.
    pub root_run_id: String,
    /// Non-fatal lease finalization warning after the decision was committed.
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

pub(crate) fn decide_gate_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    gate_id: Option<&str>,
    decision: GateDecision,
    input: GateDecisionInput,
) -> Result<RunReport> {
    let resolution = resolve_blocked_gate(home_dir, run_id)?;
    validate_and_match_gate_id(run_id, gate_id, &resolution.gate_id)?;
    let root_run_id = ensure_gate_decision_capability(
        repo_root,
        home_dir,
        &resolution.run_id,
        &resolution.step_id,
        &resolution.gate_id,
        router_cache,
    )?;
    let lease_release_warning = persist_gate_decision(
        repo_root,
        home_dir,
        &GateDecisionTarget {
            root_run_id: root_run_id.clone(),
            run_id: resolution.run_id.clone(),
            step_id: resolution.step_id.clone(),
            gate_id: resolution.gate_id.clone(),
        },
        decision.as_persisted_str(),
        input,
        router_cache,
    )?;
    // Re-enter the lineage root once and let subflow execution recurse into the
    // child tree. Re-entering every ancestor can immediately replay the newly
    // blocked descendant a second time, which inflates executions and produces
    // misleading gate summaries after a single decision.
    let mut report =
        resume_or_reenter_run_with_cache(repo_root, home_dir, router_cache, &root_run_id)?;
    if decision == GateDecision::Approved
        && report.status == "failed"
        && let Some(run_path) = auto_reblock_path_for_approved_gate_failure(home_dir, &root_run_id)?
    {
        let first_warning = report.lease_release_warning;
        report = reenter_run_path_with_cache(repo_root, home_dir, router_cache, &run_path)?;
        report.lease_release_warning = super::execution::combine_lease_release_warnings(
            report.lease_release_warning,
            first_warning,
        );
    }
    // The gate write is a separate committed mutation from re-entry; keep its
    // lease finalization warning visible unless re-entry produced one itself.
    report.lease_release_warning = super::execution::combine_lease_release_warnings(
        report.lease_release_warning,
        lease_release_warning,
    );
    Ok(report)
}

pub(crate) fn record_gate_decision_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    gate_id: Option<&str>,
    decision: GateDecision,
    input: GateDecisionInput,
) -> Result<GateDecisionReport> {
    let resolution = resolve_blocked_gate(home_dir, run_id)?;
    validate_and_match_gate_id(run_id, gate_id, &resolution.gate_id)?;
    let root_run_id = ensure_gate_decision_capability(
        repo_root,
        home_dir,
        &resolution.run_id,
        &resolution.step_id,
        &resolution.gate_id,
        router_cache,
    )?;
    let lease_release_warning = persist_gate_decision(
        repo_root,
        home_dir,
        &GateDecisionTarget {
            root_run_id: root_run_id.clone(),
            run_id: resolution.run_id.clone(),
            step_id: resolution.step_id.clone(),
            gate_id: resolution.gate_id.clone(),
        },
        decision.as_persisted_str(),
        input,
        router_cache,
    )?;
    Ok(GateDecisionReport {
        root_run_id,
        lease_release_warning,
    })
}

fn ensure_gate_decision_capability(
    repo_root: &Path,
    home_dir: &Path,
    run_id: &str,
    step_id: &str,
    gate_id: &str,
    router_cache: &RouterCache,
) -> Result<String> {
    let recipe = load_persisted_recipe(home_dir, run_id)?;
    // Select the persisted `approval:decide` checkpoint capability for this
    // gate step directly from the pinned recipe, mirroring the kernel's own
    // gate-dispatch selection (`persisted_capabilities_for_step`). This must
    // never be regenerated from live recipe/gate data: a missing, malformed,
    // or mismatched persisted authority denies the decision rather than
    // falling back to a freshly derived request.
    let capabilities =
        transist_core::persisted_capabilities_for_step(&recipe, step_id, Some("decide"))
            .with_context(|| format!("gate '{gate_id}' on step '{step_id}'"))?;
    let kernel = execution::kernel_for_run_with_local_only(
        home_dir,
        repo_root,
        router_cache,
        run_id,
        false,
    )?;
    let record = kernel
        .services()
        .run_store
        .load_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run '{run_id}' not found"))?;
    let root_run_id = record
        .lineage
        .as_ref()
        .map(|lineage| lineage.root_run_id.as_str())
        .filter(|root_run_id| !root_run_id.trim().is_empty())
        .unwrap_or(&record.run_id)
        .to_string();
    let policy_workflow_ref = kernel.policy_workflow_ref(&record)?;
    let descriptor = transist_core::build_gate_policy_descriptor_for_workflow(
        &policy_workflow_ref,
        &capabilities,
    )?;
    let decision =
        kernel
            .services()
            .policy
            .validate_step_execution(run_id, step_id, &descriptor)?;
    if !decision.allowed {
        bail!(
            "approval decision denied (code={}, message={})",
            decision.reason_code.as_deref().unwrap_or("policy_denied"),
            decision.message.as_deref().unwrap_or("denied")
        );
    }
    Ok(root_run_id)
}

pub(super) fn resolve_blocked_gate(home_dir: &Path, run_id: &str) -> Result<BlockedGateResolution> {
    let mut visited = BTreeSet::new();
    resolve_blocked_gate_recursive(home_dir, run_id, run_id, &mut visited)
}

pub(super) fn auto_reblock_path_for_approved_gate_failure(
    home_dir: &Path,
    root_run_id: &str,
) -> Result<Option<Vec<String>>> {
    let run_path = resolve_actionable_failed_run_path(home_dir, root_run_id)?;
    let Some(actionable_run_id) = run_path.last() else {
        return Ok(None);
    };
    if failed_run_can_reblock_at_approved_gate(home_dir, actionable_run_id)? {
        return Ok(Some(run_path));
    }
    Ok(None)
}

fn failed_run_can_reblock_at_approved_gate(home_dir: &Path, run_id: &str) -> Result<bool> {
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    if run["status"].as_str() != Some("failed") {
        return Ok(false);
    }
    let steps = run["steps"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("run '{}' state is missing steps", run_id))?;
    let recipe = load_persisted_recipe(home_dir, run_id)?;
    let child_loop_context = run_is_loop_body_subflow(home_dir, run_id)?;

    for step in recipe.steps.iter().filter(|step| {
        matches!(
            &step.execution,
            transist_protocol::StepExecutionDescriptor::Control { primitive }
                if primitive == "control.gate.manual"
        )
    }) {
        let Some(step_state) = steps.get(&step.id) else {
            continue;
        };
        if step_state["status"].as_str() != Some("succeeded") {
            continue;
        }
        if step_state["last_result"]["output"]["decision"].as_str() != Some("approved") {
            continue;
        }
        if !child_loop_context && !step_is_in_loop_body(&recipe, &step.id) {
            continue;
        }
        let has_safe_failed_descendant = non_loop_descendants_including_self(&recipe, &step.id)
            .into_iter()
            .filter(|step_id| step_id != &step.id)
            .any(|step_id| {
                steps
                    .get(&step_id)
                    .map(step_is_safe_condition_failure)
                    .unwrap_or(false)
            });
        if has_safe_failed_descendant {
            return Ok(true);
        }
    }

    Ok(false)
}

fn run_is_loop_body_subflow(home_dir: &Path, run_id: &str) -> Result<bool> {
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    let Some(parent_run_id) = run["lineage"]["parent_run_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(false);
    };
    let Some(parent_step_id) = run["lineage"]["parent_step_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(false);
    };
    let parent_recipe = load_persisted_recipe(home_dir, parent_run_id)?;
    Ok(step_is_in_loop_body(&parent_recipe, parent_step_id))
}

fn step_is_in_loop_body(recipe: &Recipe, step_id: &str) -> bool {
    recipe.steps.iter().any(|step| {
        loop_body_steps(step)
            .iter()
            .any(|body_step_id| body_step_id == step_id)
    })
}

fn loop_body_steps(step: &RecipeStep) -> Vec<String> {
    let Some(KernelValue::Object(loop_meta)) = step.with.get("__transist_loop") else {
        return Vec::new();
    };
    let Some(KernelValue::Array(entries)) = loop_meta.get("body_steps") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| match entry {
            KernelValue::String(value) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            _ => None,
        })
        .collect()
}

fn non_loop_descendants_including_self(recipe: &Recipe, root_step_id: &str) -> Vec<String> {
    let mut outgoing = BTreeMap::<String, Vec<String>>::new();
    if let Some(graph) = recipe.graph.as_ref() {
        for edge in &graph.edges {
            if edge.r#loop.is_some() {
                continue;
            }
            outgoing
                .entry(edge.from.clone())
                .or_default()
                .push(edge.to.clone());
        }
    } else {
        for pair in recipe.steps.windows(2) {
            outgoing
                .entry(pair[0].id.clone())
                .or_default()
                .push(pair[1].id.clone());
        }
    }

    let mut queue = VecDeque::from([root_step_id.to_string()]);
    let mut visited = BTreeSet::new();
    let mut descendants = Vec::new();

    while let Some(step_id) = queue.pop_front() {
        if !visited.insert(step_id.clone()) {
            continue;
        }
        descendants.push(step_id.clone());
        for next in outgoing.get(&step_id).into_iter().flatten() {
            queue.push_back(next.clone());
        }
    }

    descendants
}

fn step_is_safe_condition_failure(step: &serde_json::Value) -> bool {
    step["status"].as_str() == Some("failed")
        && step["last_result"]["error"]["retryability"].as_str() == Some("safe")
        && step["last_result"]["output"]["ok"] == serde_json::Value::Bool(false)
}

fn resolve_blocked_gate_recursive(
    home_dir: &Path,
    run_id: &str,
    requested_run_id: &str,
    visited: &mut BTreeSet<String>,
) -> Result<BlockedGateResolution> {
    if !visited.insert(run_id.to_string()) {
        bail!(
            "run '{}' contains a cycle while resolving the current blocked gate",
            requested_run_id
        );
    }
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    let steps = run["steps"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("run '{}' state is missing steps", run_id))?;
    let mut blocked_steps = steps.iter().filter_map(|(step_id, step)| {
        (step["status"].as_str() == Some("blocked")).then_some(step_id.clone())
    });
    let Some(step_id) = blocked_steps.next() else {
        if let Some(child_run_id) = next_actionable_subflow_run(home_dir, run_id)? {
            return resolve_blocked_gate_recursive(
                home_dir,
                &child_run_id,
                requested_run_id,
                visited,
            );
        }
        bail!(
            "run '{}' is not currently blocked on a gate",
            requested_run_id
        );
    };
    if blocked_steps.next().is_some() {
        bail!(
            "run '{}' is blocked on multiple steps; gate decision is ambiguous",
            run_id
        );
    }

    let recipe = load_persisted_recipe(home_dir, run_id)?;
    let step = recipe
        .steps
        .iter()
        .find(|step| step.id == step_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "blocked step '{}' is missing from persisted recipe",
                step_id
            )
        })?;

    if matches!(
        step.execution,
        transist_protocol::StepExecutionDescriptor::Subflow { .. }
    ) && let Some(child_run_id) = child_run_id_for_step_state(&steps[&step_id])
    {
        let child = serde_json::Value::from(execution::show_run(home_dir, &child_run_id)?);
        if child["status"].as_str() == Some("blocked") {
            return resolve_blocked_gate_recursive(
                home_dir,
                &child_run_id,
                requested_run_id,
                visited,
            );
        }
    }

    Ok(BlockedGateResolution {
        run_id: run_id.to_string(),
        step_id: step_id.clone(),
        gate_id: gate_id_for_step(step)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GateDecisionTarget {
    pub(super) root_run_id: String,
    pub(super) run_id: String,
    pub(super) step_id: String,
    pub(super) gate_id: String,
}

fn persist_gate_decision(
    repo_root: &Path,
    home_dir: &Path,
    target: &GateDecisionTarget,
    decision: &str,
    input: GateDecisionInput,
    router_cache: &RouterCache,
) -> Result<Option<LeaseReleaseWarning>> {
    let input = validate_gate_decision_input(home_dir, input)?;
    let decided_at_ms = input
        .decided_at_ms
        .ok_or_else(|| anyhow::anyhow!("validated gate decision is missing its timestamp"))?;
    let payload = GateDecisionRecord {
        review_request: None,
        schema: "transist.gate_decision.v1".to_string(),
        run_id: target.run_id.clone(),
        gate_id: target.gate_id.clone(),
        step_id: target.step_id.clone(),
        decision: decision.to_string(),
        actor: input.actor,
        actor_source: input.actor_source,
        reason: input.reason,
        decided_at_ms: Some(decided_at_ms),
    };
    let kernel = crate::composition::default_runtime_kernel(router_cache, repo_root, home_dir)?;
    let outcome = kernel.record_gate_decision(home_dir, &target.root_run_id, &payload)?;
    Ok(outcome.lease_release_warning)
}

fn validate_gate_decision_input(
    home_dir: &Path,
    input: GateDecisionInput,
) -> Result<GateDecisionInput> {
    let actor_env = std::env::var("TRANSIST_ACTOR").ok();
    let reason_env = std::env::var("TRANSIST_REASON").ok();
    let (actor, actor_source, reason) = resolve_gate_decision_input_sources(
        home_dir,
        input.actor.as_deref(),
        input.actor_source.as_deref(),
        input.reason.as_deref(),
        actor_env.as_deref(),
        reason_env.as_deref(),
    )?;

    let Some(actor) = actor else {
        bail!(
            "manual gate decisions require actor identity; provide --actor, set TRANSIST_ACTOR, configure manual_gates.actor in <home>/config.yaml, or set identity.actor in user settings"
        );
    };
    let Some(reason) = reason else {
        bail!(
            "manual gate decisions require a non-empty reason; provide --reason or set TRANSIST_REASON"
        );
    };

    Ok(GateDecisionInput {
        actor: Some(actor),
        actor_source,
        reason: Some(reason),
        decided_at_ms: Some(input.decided_at_ms.unwrap_or_else(current_unix_ms)),
    })
}

fn resolve_gate_decision_input_sources(
    home_dir: &Path,
    actor: Option<&str>,
    actor_source: Option<&str>,
    reason: Option<&str>,
    actor_env: Option<&str>,
    reason_env: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let actor = normalize_optional_string(actor);
    let actor_source = normalize_optional_string(actor_source);
    let reason = normalize_optional_string(reason);
    let actor_env = normalize_optional_string(actor_env);
    let reason_env = normalize_optional_string(reason_env);

    if let Some(actor) = actor {
        return Ok((Some(actor), actor_source, reason.or(reason_env)));
    }
    if let Some(actor) = actor_env {
        return Ok((Some(actor), Some("env".to_string()), reason.or(reason_env)));
    }
    if let Some(actor) = load_gate_actor_from_config(home_dir)? {
        return Ok((
            Some(actor),
            Some("config".to_string()),
            reason.or(reason_env),
        ));
    }
    if let Some(actor) = crate::load_user_gate_actor()? {
        return Ok((
            Some(actor),
            Some("user-settings".to_string()),
            reason.or(reason_env),
        ));
    }

    Ok((None, None, reason.or(reason_env)))
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Default, Deserialize)]
struct GateDecisionRuntimeConfig {
    #[serde(default)]
    manual_gates: GateDecisionManualGateConfig,
}

#[derive(Debug, Default, Deserialize)]
struct GateDecisionManualGateConfig {
    #[serde(default)]
    actor: Option<String>,
}

fn load_gate_actor_from_config(home_dir: &Path) -> Result<Option<String>> {
    let path = home_dir.join("config.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed reading gate decision config: {}", path.display()))?;
    let config: GateDecisionRuntimeConfig = yaml_serde::from_str(&raw)
        .context("invalid runtime config yaml for manual gate decision identity")?;
    Ok(config
        .manual_gates
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

fn gate_id_for_step(step: &RecipeStep) -> Result<String> {
    // Strictly reject an authored-but-invalid gate id: whitespace/empty or a
    // non-string value fails closed rather than silently falling back to the
    // step id, and the authored value is never trimmed.
    let raw = match step.with.get("gate_id") {
        None => step.id.clone(),
        Some(KernelValue::String(value)) => {
            if value.trim().is_empty() {
                bail!(
                    "gate step '{}' has an empty or whitespace-only gate_id",
                    step.id
                );
            }
            value.clone()
        }
        Some(_) => bail!(
            "gate step '{}' has a non-string gate_id (must be a string)",
            step.id
        ),
    };
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, &raw)
        .with_context(|| format!("gate step '{}' has invalid gate id", step.id))?;
    Ok(raw)
}

/// Validates an operator-supplied gate id against the resolved blocked gate.
///
/// The supplied id must satisfy the identifier rule and match the exact gate
/// the run is currently blocked on. An invalid or mismatched id fails closed.
fn validate_and_match_gate_id(
    run_id: &str,
    requested: Option<&str>,
    resolved_gate_id: &str,
) -> Result<()> {
    let Some(requested) = requested else {
        return Ok(());
    };
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, requested)
        .context("invalid gate id")?;
    if resolved_gate_id != requested {
        bail!(
            "run '{}' is blocked on gate '{}' not '{}'",
            run_id,
            resolved_gate_id,
            requested
        );
    }
    Ok(())
}

// NOTE: gate decision paths are constructed exclusively through
// `transist_storage_local::run_paths::gate_decision_path` and
// `transist_storage_local::run_paths::gate_decision_history_path`, which validate the
// exact authored gate id. No lossy slug normalization is performed here.

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn validate_gate_decision_input_rejects_missing_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let error = validate_gate_decision_input(
            tmp.path(),
            GateDecisionInput {
                actor: Some("alice".to_string()),
                actor_source: Some("flag".to_string()),
                reason: Some("   ".to_string()),
                decided_at_ms: None,
            },
        )
        .expect_err("missing reason must fail");

        assert!(error.to_string().contains("require a non-empty reason"));
    }

    #[test]
    fn validate_gate_decision_input_rejects_missing_actor_from_all_sources() {
        let _guard = env_lock().lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("config-home");
        let prior_home = std::env::var_os("HOME");
        let prior_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: `env_lock` is held, so no other thread races on the
        // process-global environment; prior values are restored below.
        unsafe {
            std::env::set_var("HOME", config_home.path());
            std::env::set_var("XDG_CONFIG_HOME", config_home.path());
        }

        let error = validate_gate_decision_input(
            tmp.path(),
            GateDecisionInput {
                actor: None,
                actor_source: None,
                reason: Some("approved".to_string()),
                decided_at_ms: None,
            },
        )
        .expect_err("missing actor must fail");

        assert!(error.to_string().contains("require actor identity"));

        // SAFETY: `env_lock` is held; restoring prior values under the same
        // guarded test section before the lock is released.
        unsafe {
            match prior_home {
                Some(path) => std::env::set_var("HOME", path),
                None => std::env::remove_var("HOME"),
            }
            match prior_xdg_config_home {
                Some(path) => std::env::set_var("XDG_CONFIG_HOME", path),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn validate_gate_decision_input_stamps_timestamp_and_loads_config_actor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("config.yaml"),
            "version: v1\nmanual_gates:\n  actor: reviewer@example.com\n",
        )
        .expect("write config");

        let validated = validate_gate_decision_input(
            tmp.path(),
            GateDecisionInput {
                actor: None,
                actor_source: Some("config".to_string()),
                reason: Some("approved".to_string()),
                decided_at_ms: None,
            },
        )
        .expect("validate gate decision");

        assert_eq!(validated.actor.as_deref(), Some("reviewer@example.com"));
        assert_eq!(validated.actor_source.as_deref(), Some("config"));
        assert!(validated.decided_at_ms.is_some());
    }

    #[test]
    fn validate_gate_decision_input_prefers_env_over_config_when_actor_missing() {
        let _guard = env_lock().lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("config.yaml"),
            "version: v1\nmanual_gates:\n  actor: config@example.com\n",
        )
        .expect("write config");

        let (actor, actor_source, reason) = resolve_gate_decision_input_sources(
            tmp.path(),
            None,
            Some("flag"),
            Some("approved"),
            Some("env@example.com"),
            None,
        )
        .expect("resolve decision sources");

        assert_eq!(actor.as_deref(), Some("env@example.com"));
        assert_eq!(actor_source.as_deref(), Some("env"));
        assert_eq!(reason.as_deref(), Some("approved"));
    }

    #[test]
    fn validate_gate_decision_input_loads_user_settings_actor_when_home_config_missing() {
        let _guard = env_lock().lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("config-home");

        // Build the platform-specific settings dir that platform_config_dir() resolves
        // to for the given HOME (and no XDG_CONFIG_HOME override):
        //   macOS  → $HOME/Library/Application Support/transist/
        //   Linux  → $HOME/.config/transist/
        //   Windows → $APPDATA/transist/ (not covered here)
        let settings_dir = if cfg!(target_os = "macos") {
            config_home
                .path()
                .join("Library")
                .join("Application Support")
                .join("transist")
        } else {
            config_home.path().join(".config").join("transist")
        };
        fs::create_dir_all(&settings_dir).expect("create user settings dir");
        fs::write(
            settings_dir.join("settings.yaml"),
            "schema: transist.user-settings.v1\nidentity:\n  actor: reviewer@example.com\n",
        )
        .expect("write user settings");
        let prior_home = std::env::var_os("HOME");
        let prior_xdg = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: `env_lock` is held; prior values are captured and restored
        // below before the guard drops.
        unsafe {
            std::env::set_var("HOME", config_home.path());
            // Unset XDG_CONFIG_HOME so platform_config_dir() falls through to $HOME/.config
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        let validated = validate_gate_decision_input(
            tmp.path(),
            GateDecisionInput {
                actor: None,
                actor_source: None,
                reason: Some("approved".to_string()),
                decided_at_ms: None,
            },
        )
        .expect("validate gate decision");

        assert_eq!(validated.actor.as_deref(), Some("reviewer@example.com"));
        assert_eq!(validated.actor_source.as_deref(), Some("user-settings"));

        // SAFETY: `env_lock` is held; restoring prior captured values.
        unsafe {
            match prior_home {
                Some(path) => std::env::set_var("HOME", path),
                None => std::env::remove_var("HOME"),
            }
            match prior_xdg {
                Some(path) => std::env::set_var("XDG_CONFIG_HOME", path),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn persist_gate_decision_writes_matching_active_and_history_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join("transist-home");
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let recipe: Recipe = yaml_serde::from_str(
            r#"
schema: transist.recipe.v1
workflow:
  id: gate-records
  version: v1
steps:
  - id: review
    execution:
      kind: control
      primitive: control.gate.manual
    with:
      gate_id: review-gate
"#,
        )
        .expect("manual gate recipe");
        let router_cache = RouterCache::local();
        // Persist a real waiting run and recipe; this ordinary gate has no
        // document-review request to attach to its decision.
        let kernel =
            crate::composition::default_runtime_kernel(&router_cache, tmp.path(), &home_dir)
                .expect("kernel");
        let report = kernel
            .execute_recipe(tmp.path(), &home_dir, &recipe, Some("demo-run"))
            .expect("run to manual gate");
        assert_eq!(report.status, transist_core::KernelRunStatus::Blocked);
        let blocked_gate = resolve_blocked_gate(&home_dir, "demo-run").expect("persisted gate");
        assert_eq!(blocked_gate.step_id, "review");
        assert_eq!(blocked_gate.gate_id, "review-gate");
        let run_root = home_dir.join("runs/run_demo-run");

        persist_gate_decision(
            tmp.path(),
            &home_dir,
            &GateDecisionTarget {
                root_run_id: "demo-run".to_string(),
                run_id: "demo-run".to_string(),
                step_id: "review".to_string(),
                gate_id: "review-gate".to_string(),
            },
            "approved",
            GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("flag".to_string()),
                reason: Some("looks good".to_string()),
                decided_at_ms: Some(42),
            },
            &router_cache,
        )
        .expect("persist gate decision");

        let active_path = transist_storage_local::run_paths::gate_decision_path(
            &home_dir,
            "demo-run",
            "review-gate",
        )
        .expect("active path");
        let active: GateDecisionRecord =
            serde_json::from_str(&fs::read_to_string(&active_path).expect("read active decision"))
                .expect("parse active decision");
        let history_dir = run_root.join("gates/history");
        let history_entry = fs::read_dir(&history_dir)
            .expect("read history dir")
            .next()
            .expect("history entry")
            .expect("history entry read");
        let history: GateDecisionRecord = serde_json::from_str(
            &fs::read_to_string(history_entry.path()).expect("read history decision"),
        )
        .expect("parse history decision");

        assert_eq!(active, history);
        assert_eq!(active.review_request, None);
        assert_eq!(active.actor.as_deref(), Some("reviewer@example.com"));
        assert_eq!(active.actor_source.as_deref(), Some("flag"));
        assert_eq!(active.reason.as_deref(), Some("looks good"));
        assert_eq!(active.decided_at_ms, Some(42));
    }
}
