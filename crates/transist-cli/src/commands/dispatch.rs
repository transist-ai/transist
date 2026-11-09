// SPDX-License-Identifier: MPL-2.0
//! CLI dispatcher mapping parsed `Cli` commands onto `TransistContext` calls.
//!
//! The dispatcher centralises the giant top-level command match so that
//! `main.rs` only has to parse arguments, install signal handlers, and
//! forward them here.

use anyhow::bail;
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::IsTerminal;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::cli::{
    Cli, CliOutputFormat, CliUiMode, Commands, EvidenceSubcommand, GateSubcommand, HomeSubcommand,
    InspectionSyncMode, PluginSubcommand, PolicySubcommand, RunsLeaseSubcommand, RunsSubcommand,
    SnapshotSubcommand, TrustSubcommand, WorkflowSubcommand,
};
use crate::commands::workflow_promotion::{serialized_validation, stage_bound_validation};
use crate::commands::workflow_reports::{render_authorization_summary, render_check_summary};
use crate::display::summary::{
    RunsShowStepFilter, compact_run_status_lines, emit_operator_summary, format_runs_show_command,
};
use crate::execution::RecipeCacheOwner;
use crate::input::merge_workflow_inputs;
use crate::managed_exit_codes::{wrap_usage_error, wrap_workflow_validation_error};
use crate::pager::{self, RenderedTerminalBlock};
use crate::prompt::{PromptUi, run_with_loading_feedback};
use crate::runner::spawn_and_attach;
use crate::runner::{RunnerAction, run_runner_loop};

#[allow(clippy::too_many_arguments)]
fn export_run_snapshot(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    run_id: String,
    source_id: String,
    workspace_id: String,
    operator_label: Option<String>,
    tree_version: u64,
    output: std::path::PathBuf,
    max_payload_bytes: Option<usize>,
    sync_mode: InspectionSyncMode,
    include_workflow_input_values: bool,
    include_event_payloads: bool,
    include_run_detail_full: bool,
    include_stderr_tail: bool,
    include_gate_request_summary: bool,
) -> Result<()> {
    let request = transist::InspectionSnapshotRequest {
        run_id: run_id.clone(),
        source_id,
        workspace_id,
        operator_label,
        produced_at_unix_ms: None,
        tree_version,
        event_after_by_run: Default::default(),
        max_payload_bytes,
        sync_policy: Some(transist::SyncPolicyConfig {
            mode: sync_mode.as_runtime(),
            include_workflow_input_values,
            include_event_payloads,
            include_run_detail_full,
            include_stderr_tail,
            include_gate_request_summary,
        }),
        extensions: Default::default(),
        provider_facts: Default::default(),
        policy_facts: Default::default(),
        runtime_id: None,
        organization_id: None,
        policy_revision: None,
        trusted_execution_mode: None,
        workflow_digest: None,
        workflow_signature_status: None,
        workflow_signer_key_id: None,
        plugin_trust: None,
        trust_decision: None,
        external_transfer_summary: None,
        approver_identity: None,
        locality: None,
        evidence_export: None,
    };
    let snapshot = context
        .inspection_snapshot(&request)?
        .with_context(|| format!("run '{run_id}' was not found"))?;
    transist::validate_snapshot_contract(&snapshot)?;

    if output == Path::new("-") {
        emit_json_output(
            prompt,
            &format!("transist runs snapshot export {run_id}"),
            &snapshot,
        )?;
    } else {
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating export dir: {}", parent.display()))?;
        }
        let payload = serde_json::to_vec_pretty(&snapshot)
            .context("failed serializing inspection snapshot")?;
        fs::write(&output, payload).with_context(|| {
            format!("failed writing snapshot export file: {}", output.display())
        })?;
        emit_json_output(
            prompt,
            &format!("transist runs snapshot export {run_id}"),
            &serde_json::json!({
                "run_id": run_id,
                "root_run_id": snapshot.root_run_id,
                "output": output.display().to_string(),
                "schema_version": snapshot.schema_version,
                "kind": snapshot.kind,
            }),
        )?;
    }
    Ok(())
}

/// Run the parsed CLI command and return a process exit code.
pub(crate) fn dispatch(
    cli: Cli,
    ui_mode: CliUiMode,
    repo_root: &Path,
    cwd: &Path,
    interrupt_flag: &Arc<AtomicBool>,
    prompt: &mut dyn PromptUi,
) -> Result<i32> {
    let output_format = cli.format;
    if matches!(output_format, Some(CliOutputFormat::Yaml))
        && !matches!(
            &cli.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Capabilities { .. }
            }) | Some(Commands::Policy {
                command: PolicySubcommand::Show { effective: true }
            })
        )
    {
        bail!(
            "--format yaml is only supported by 'transist runs capabilities' and 'transist policy show --effective'"
        );
    }
    if matches!(output_format, Some(CliOutputFormat::Plain))
        && matches!(
            &cli.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Capabilities { .. }
            })
        )
    {
        bail!("'transist runs capabilities' supports only --format yaml or --format json");
    }

    // Phase 1: commands that tolerate an unresolved home and do not need TransistContext.
    // These return early before resolve_home is called so that `transist home
    // init|set|unset` work cleanly in a fresh repo.
    if let Some(Commands::Home { command }) = &cli.command {
        match command {
            HomeSubcommand::Init { mode } => {
                let local_dir = match cli.home.as_deref() {
                    Some(path) => path.to_path_buf(),
                    None => {
                        let default_home = transist::per_user_default_home().ok_or_else(|| {
                            anyhow::anyhow!(
                                "cannot determine a per-user default home: set HOME or \
                                 XDG_STATE_HOME, or pass --home explicitly"
                            )
                        })?;
                        transist::record_user_home_path(cwd, &default_home)?
                    }
                };
                transist::init_home_dir_with_mode(&local_dir, mode.runtime_mode())?;
                println!("initialized {}", local_dir.display());
                return Ok(0);
            }
            HomeSubcommand::Set { path } => {
                let path = path.clone();
                transist::save_user_home_path(cwd, &path)?;
                let payload = transist::resolve_settings_payload(repo_root, cwd, None)?;
                emit_json_output(prompt, "transist home set", &payload)?;
                return Ok(0);
            }
            HomeSubcommand::Unset => {
                transist::clear_user_home_path()?;
                let payload = transist::resolve_settings_payload(repo_root, cwd, None);
                match payload {
                    Ok(p) => emit_json_output(prompt, "transist home unset", &p)?,
                    Err(_) => emit_json_output(
                        prompt,
                        "transist home unset",
                        &serde_json::json!({"persisted_user_home": null, "home_resolved": false}),
                    )?,
                }
                return Ok(0);
            }
            HomeSubcommand::Get => {} // needs resolved home — falls through to Phase 2
        }
    }

    if let Some(Commands::Workflow {
        command:
            WorkflowSubcommand::Explain {
                path,
                inputs_file,
                compare_to,
            },
    }) = &cli.command
    {
        // This branch must precede configuration, installation validation and
        // context construction: even an absent/uninitialized home is supported.
        let home = transist::resolve_workflow_explanation_home(cwd, cli.home.as_deref())?;
        let inputs = merge_workflow_inputs(inputs_file.as_deref(), &[])?;
        let explanation =
            transist::explain_workflow_file(home.as_deref(), path, &inputs, compare_to.as_deref())?;
        if matches!(output_format, Some(CliOutputFormat::Json)) {
            // A canonical sorted JSON object has no terminal framing or clocks.
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::to_value(&explanation)?)?
            );
        } else {
            print!("{}", transist::render_workflow_explanation(&explanation)?);
        }
        return Ok(0);
    }

    // Phase 2: all other commands require a resolved home.
    let resolved = transist::resolve_home(repo_root, cwd, cli.home.as_deref())?;
    transist::validate_runtime_configuration(&resolved.effective_home)?;
    let context = transist::TransistContext::with_home(repo_root, resolved.effective_home.clone());
    let read_only_home = matches!(
        &cli.command,
        Some(Commands::Workflow {
            command: WorkflowSubcommand::Check { .. } | WorkflowSubcommand::Install { .. }
        })
    );
    if read_only_home {
        context.validate_installation()?;
    } else {
        context.ensure_installation()?;
    }
    let recipe_cache = RecipeCacheOwner::default();

    match cli.command {
        None => unreachable!("bare command should be handled before dispatch"),
        Some(Commands::Home { command }) => match command {
            HomeSubcommand::Init { .. } => unreachable!("handled in Phase 1"),
            HomeSubcommand::Get => {
                emit_json_output(prompt, "transist home get", &resolved)?;
            }
            HomeSubcommand::Set { .. } => unreachable!("handled in Phase 1"),
            HomeSubcommand::Unset => unreachable!("handled in Phase 1"),
        },
        Some(Commands::Policy { command }) => match command {
            PolicySubcommand::Validate => emit_json_output(
                prompt,
                "transist policy validate",
                &serde_json::json!({"valid": true, "effective_home": resolved.effective_home}),
            )?,
            PolicySubcommand::Show { effective: true } => {
                let yaml =
                    transist::effective_runtime_configuration_yaml(&resolved.effective_home)?;
                if matches!(output_format, Some(CliOutputFormat::Json)) {
                    let value: serde_json::Value = yaml_serde::from_str(&yaml)?;
                    emit_json_output(prompt, "transist policy show --effective", &value)?;
                } else {
                    emit_text_output(prompt, "transist policy show --effective", &yaml)?;
                }
            }
            PolicySubcommand::Show { effective: false } => {
                bail!("'transist policy show' requires --effective")
            }
        },
        Some(Commands::Trust { command }) => match command {
            TrustSubcommand::Import { trust_root_file } => {
                let imported = context.import_trust_root(&trust_root_file)?;
                emit_json_output(
                    prompt,
                    &format!("transist trust import {}", trust_root_file.display()),
                    &imported,
                )?;
            }
            TrustSubcommand::List => {
                let trusted = context.list_trust_roots()?;
                emit_json_output(prompt, "transist trust list", &trusted)?;
            }
            TrustSubcommand::Inspect { key_id } => {
                let trust_root = context.inspect_trust_root(&key_id)?;
                emit_json_output(
                    prompt,
                    &format!("transist trust inspect {key_id}"),
                    &trust_root,
                )?;
            }
            TrustSubcommand::Export { key_id, output } => {
                let trust_root = context.inspect_trust_root(&key_id)?;
                if output == Path::new("-") {
                    emit_json_output(
                        prompt,
                        &format!("transist trust export {key_id} --output -"),
                        &trust_root,
                    )?;
                } else {
                    if let Some(parent) = output
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                    {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("failed creating export dir: {}", parent.display())
                        })?;
                    }
                    let payload = serde_json::to_vec_pretty(&trust_root)
                        .context("failed serializing trust root")?;
                    fs::write(&output, payload).with_context(|| {
                        format!("failed writing export file: {}", output.display())
                    })?;
                    emit_json_output(
                        prompt,
                        &format!("transist trust export {key_id}"),
                        &serde_json::json!({
                            "name": trust_root.name,
                            "key_id": trust_root.key_id,
                            "output": output.display().to_string(),
                        }),
                    )?;
                }
            }
            TrustSubcommand::Remove { key_id, yes } => {
                if !yes {
                    bail!("transist trust remove requires --yes");
                }
                let removed = context.remove_trust_root(&key_id)?;
                emit_json_output(prompt, &format!("transist trust remove {key_id}"), &removed)?;
            }
        },
        Some(Commands::Workflow { command }) => match command {
            WorkflowSubcommand::Explain { .. } => {
                unreachable!("handled before runtime initialization")
            }
            WorkflowSubcommand::Check {
                path,
                workflow_ref: workflow,
                live,
                inputs,
                inputs_file,
                trusted_execution,
                local_only,
                output,
            } => match (path, workflow) {
                (Some(path), None) => {
                    if live || output.is_some() {
                        bail!("--live and --output require an installed workflow reference");
                    }
                    let full = matches!(output_format, Some(CliOutputFormat::Json));
                    if inputs.is_empty() && inputs_file.is_none() {
                        if trusted_execution || local_only {
                            bail!(
                                "--trusted-execution and --local-only require bound validation inputs"
                            );
                        }
                        if !full {
                            let summary = context
                                .validate_flow_file_summary(&path)
                                .map_err(wrap_workflow_validation_error)?;
                            emit_text_output(
                                prompt,
                                "transist workflows check",
                                &render_check_summary(&summary),
                            )?;
                            return Ok(0);
                        }
                        let validation = context
                            .validate_flow_file(&path)
                            .map_err(wrap_workflow_validation_error)?;
                        let report = serialized_validation(&validation)?;
                        emit_json_output(
                            prompt,
                            &format!("transist workflows check --path {}", path.display()),
                            &structured_validation_output(
                                serde_json::to_value(path)?,
                                "authored",
                                report,
                            ),
                        )?;
                    } else {
                        let workflow_inputs = merge_workflow_inputs(
                            inputs_file.as_deref(),
                            &inputs,
                        )
                        .map_err(|source| {
                            wrap_usage_error("invalid workflow input arguments for check", source)
                        })?;
                        let package = transist::capture_workflow_package(&path)?;
                        let validation = stage_bound_validation(
                            repo_root,
                            context.home_dir(),
                            &package,
                            workflow_inputs,
                            trusted_execution,
                            local_only,
                            full,
                        )?;
                        if let Some(summary) = validation.summary {
                            let valid = summary.status != transist::ValidationStatus::Failed;
                            emit_text_output(
                                prompt,
                                "transist workflows check",
                                &render_check_summary(&summary),
                            )?;
                            return Ok(i32::from(!valid));
                        }
                        let report = validation.report.expect("full staged validation");
                        let valid =
                            report
                                .get("valid")
                                .and_then(Value::as_bool)
                                .unwrap_or_else(|| {
                                    report.get("checks").and_then(Value::as_array).is_none_or(
                                        |checks| {
                                            checks.iter().all(|check| {
                                                check.get("status").and_then(Value::as_str)
                                                    != Some("failed")
                                            })
                                        },
                                    )
                                });
                        emit_json_output(
                            prompt,
                            "transist workflows check",
                            &structured_bound_validation_output(
                                serde_json::to_value(path)?,
                                report,
                            ),
                        )?;
                        if !valid {
                            return Ok(1);
                        }
                    }
                }
                (None, Some(workflow)) => {
                    let workflow_inputs = merge_workflow_inputs(inputs_file.as_deref(), &inputs)
                        .map_err(|source| {
                            wrap_usage_error("invalid workflow input arguments for check", source)
                        })?;
                    let full = matches!(output_format, Some(CliOutputFormat::Json));
                    let live = live || output.is_some();
                    if !full && output.is_none() {
                        let summary = if live {
                            context.workflow_readiness_summary(
                                &workflow,
                                workflow_inputs,
                                trusted_execution,
                                local_only,
                            )?
                        } else {
                            context.workflow_validation_summary(
                                &workflow,
                                workflow_inputs,
                                trusted_execution,
                                local_only,
                            )?
                        };
                        emit_text_output(
                            prompt,
                            "transist workflows check",
                            &render_check_summary(&summary),
                        )?;
                        return Ok(i32::from(!if live {
                            summary.is_ready()
                        } else {
                            summary.is_validation_passed()
                        }));
                    }
                    let (report, summary) = if live {
                        context.workflow_readiness_report_with_summary(
                            &workflow,
                            workflow_inputs.clone(),
                            trusted_execution,
                            local_only,
                        )?
                    } else {
                        context.workflow_validation_report_with_summary(
                            &workflow,
                            workflow_inputs.clone(),
                            trusted_execution,
                            local_only,
                        )?
                    };
                    #[cfg(test)]
                    FULL_READINESS_EVALUATIONS.with(|count| count.set(count.get() + 1));
                    let ready = if live {
                        summary.is_ready()
                    } else {
                        summary.is_validation_passed()
                    };
                    let report = if live {
                        readiness_evidence_from_report(
                            &context,
                            &workflow,
                            &workflow_inputs,
                            report,
                        )?
                    } else {
                        let mut value = serde_json::to_value(report)?;
                        add_readiness_diagnostics(&mut value);
                        value
                    };
                    if let Some(output) = output {
                        write_readiness_file(&output, &report)?;
                    }
                    if full {
                        emit_json_output(prompt, "transist workflows check", &report)?;
                    } else {
                        emit_text_output(
                            prompt,
                            "transist workflows check",
                            &render_check_summary(&summary),
                        )?;
                    }
                    if !ready {
                        return Ok(1);
                    }
                }
                _ => bail!(
                    "workflows check requires exactly one of --path or <workflow-id>@<version>"
                ),
            },
            WorkflowSubcommand::Install {
                path,
                package,
                inputs,
                inputs_file,
                trusted_execution,
                local_only,
                signature,
                policy_output,
                dry_run,
                approve,
            } => {
                return dispatch_workflow_install(
                    &context,
                    prompt,
                    output_format,
                    path,
                    package,
                    inputs,
                    inputs_file,
                    trusted_execution,
                    local_only,
                    signature,
                    policy_output,
                    dry_run,
                    approve,
                );
            }
            WorkflowSubcommand::Authorize {
                workflow_ref: workflow,
                inputs,
                inputs_file,
                policy_output,
                dry_run,
                approve,
            } => {
                let inputs =
                    merge_workflow_inputs(inputs_file.as_deref(), &inputs).map_err(|source| {
                        wrap_usage_error("invalid workflow input arguments for authorize", source)
                    })?;
                if !matches!(output_format, Some(CliOutputFormat::Json)) {
                    let mut summary = context.authorize_workflow_summary(
                        &workflow,
                        inputs.clone(),
                        policy_output.as_deref(),
                        false,
                    )?;
                    use transist::WorkflowAuthorizationStatus as Status;
                    if summary.status == Status::Blocked
                        || summary.status == Status::AlreadyAuthorized
                        || dry_run
                    {
                        emit_text_output(
                            prompt,
                            "transist workflows authorize",
                            &render_authorization_summary(&summary),
                        )?;
                        return Ok(i32::from(summary.status == Status::Blocked));
                    }
                    if !approve {
                        emit_text_output(
                            prompt,
                            "transist workflows authorize",
                            &render_authorization_summary(&summary),
                        )?;
                        if !prompt.confirmation_available() {
                            return Ok(1);
                        }
                        if !prompt.confirm_suggested_action(
                            "Apply these workflow grants?",
                            "The exact authority and destination above will be written. Continue?",
                        )? {
                            summary.status = Status::Declined;
                            emit_text_output(
                                prompt,
                                "transist workflows authorize",
                                &render_authorization_summary(&summary),
                            )?;
                            return Ok(0);
                        }
                    }
                    summary =
                        context.apply_reviewed_workflow_authorization_summary(inputs, &summary)?;
                    emit_text_output(
                        prompt,
                        "transist workflows authorize",
                        &render_authorization_summary(&summary),
                    )?;
                    return Ok(i32::from(summary.status == Status::Blocked));
                }
                let required_input_hints = context
                    .inspect_workflow(&workflow)
                    .ok()
                    .map(|inspection| {
                        (
                            missing_required_workflow_inputs(&inspection.summary.inputs, &inputs),
                            empty_required_workflow_inputs(&inspection.summary.inputs, &inputs),
                        )
                    })
                    .unwrap_or_default();
                let mut report = context.authorize_workflow(
                    &workflow,
                    inputs.clone(),
                    policy_output.as_deref(),
                    false,
                )?;
                if report.status == transist::WorkflowAuthorizationStatus::Blocked {
                    emit_authorization_output(
                        prompt,
                        output_format,
                        &report,
                        &required_input_hints.0,
                        &required_input_hints.1,
                    )?;
                    return Ok(1);
                }
                if report.status == transist::WorkflowAuthorizationStatus::AlreadyAuthorized
                    || dry_run
                {
                    emit_authorization_output(
                        prompt,
                        output_format,
                        &report,
                        &required_input_hints.0,
                        &required_input_hints.1,
                    )?;
                    return Ok(0);
                }
                if !approve {
                    let interactive = !matches!(output_format, Some(CliOutputFormat::Json))
                        && prompt.confirmation_available();
                    emit_authorization_output(
                        prompt,
                        output_format,
                        &report,
                        &required_input_hints.0,
                        &required_input_hints.1,
                    )?;
                    if !interactive {
                        return Ok(1);
                    }
                    if !prompt.confirm_suggested_action(
                        "Apply these workflow grants?",
                        "The exact authority and destination above will be written. Continue?",
                    )? {
                        report.status = transist::WorkflowAuthorizationStatus::Declined;
                        emit_authorization_output(
                            prompt,
                            output_format,
                            &report,
                            &required_input_hints.0,
                            &required_input_hints.1,
                        )?;
                        return Ok(0);
                    }
                }
                report = context.apply_reviewed_workflow_authorization(inputs, &report)?;
                emit_authorization_output(
                    prompt,
                    output_format,
                    &report,
                    &required_input_hints.0,
                    &required_input_hints.1,
                )?;
                if report.status == transist::WorkflowAuthorizationStatus::Blocked {
                    return Ok(1);
                }
            }
            WorkflowSubcommand::List => {
                let workflows = context.list_workflows()?;
                emit_json_output(prompt, "transist workflows list", &workflows)?;
            }
            WorkflowSubcommand::Inspect { workflow_ref } => {
                let workflow = context.inspect_workflow(&workflow_ref)?;
                emit_json_output(
                    prompt,
                    &format!("transist workflows inspect {}", workflow_ref),
                    &workflow,
                )?;
            }
            WorkflowSubcommand::Uninstall { workflow_ref } => {
                let removed = context.uninstall_workflow(&workflow_ref, false)?;
                emit_json_output(
                    prompt,
                    "transist workflows uninstall",
                    &serde_json::json!({
                        "uninstalled": removed,
                    }),
                )?;
            }
            WorkflowSubcommand::Prepare {
                run_ref,
                workflow_ref: workflow,
                inputs,
                inputs_file,
                trusted_execution,
                local_only,
                readiness_file,
                max_age,
            } => {
                let workflow_inputs = merge_workflow_inputs(inputs_file.as_deref(), &inputs)
                    .map_err(|source| {
                        wrap_usage_error("invalid workflow input arguments for prepare", source)
                    })?;
                if let Some(readiness) = pre_dispatch_readiness(
                    &context,
                    &workflow,
                    workflow_inputs.clone(),
                    trusted_execution,
                    local_only,
                    readiness_file.as_deref(),
                    max_age,
                )? {
                    emit_readiness_output(prompt, output_format, &readiness)?;
                    return Ok(1);
                }
                reap_orphan_leases_best_effort(&context);
                let report = context.prepare_workflow_with_trust(
                    &workflow,
                    run_ref.as_deref(),
                    workflow_inputs,
                    trusted_execution,
                    local_only,
                )?;
                emit_test_run_warning_by_id(&context, &report.run_id)?;
                emit_json_output(prompt, "transist workflows prepare", &report)?;
                if report.status == "failed" {
                    emit_operator_summary(
                        &context,
                        &report.run_id,
                        prompt,
                        interrupt_flag,
                        true,
                        ui_mode,
                        &RenderedTerminalBlock::default(),
                    )?;
                    return Ok(1);
                }
            }
            WorkflowSubcommand::Run {
                run_ref,
                workflow_ref: workflow,
                inputs,
                inputs_file,
                trusted_execution,
                local_only,
                readiness_file,
                max_age,
            } => {
                let tracked_run_id = run_ref.unwrap_or_else(|| Uuid::new_v4().to_string());
                let workflow_inputs = merge_workflow_inputs(inputs_file.as_deref(), &inputs)
                    .map_err(|source| {
                        wrap_usage_error("invalid workflow input arguments for run", source)
                    })?;
                if let Some(readiness) = pre_dispatch_readiness(
                    &context,
                    &workflow,
                    workflow_inputs.clone(),
                    trusted_execution,
                    local_only,
                    readiness_file.as_deref(),
                    max_age,
                )? {
                    emit_readiness_output(prompt, output_format, &readiness)?;
                    return Ok(1);
                }
                reap_orphan_leases_best_effort(&context);
                let prepared = context.prepare_workflow_with_trust(
                    &workflow,
                    Some(&tracked_run_id),
                    workflow_inputs,
                    trusted_execution,
                    local_only,
                )?;
                if prepared.status == "failed" {
                    emit_operator_summary(
                        &context,
                        &tracked_run_id,
                        prompt,
                        interrupt_flag,
                        true,
                        ui_mode,
                        &RenderedTerminalBlock::default(),
                    )?;
                    return Ok(1);
                }
                if let Some(warning) = &prepared.lease_release_warning {
                    anyhow::bail!(
                        "run '{}' prepared successfully but automatic execution was withheld because the mutation lease could not be released: {}; inspect or repair the lease, then run 'transist runs start {}'",
                        tracked_run_id,
                        warning.message,
                        tracked_run_id
                    );
                }
                emit_test_run_warning_by_id(&context, &tracked_run_id)?;
                execute_runner_action(
                    &context,
                    prompt,
                    &RunnerAction::Run {
                        run_id: tracked_run_id.clone(),
                        trusted_execution,
                        local_only,
                    },
                    ui_mode,
                )?;
                return Ok(0);
            }
        },
        Some(Commands::Runs { command }) => {
            reap_orphan_leases_best_effort(&context);
            match command {
                RunsSubcommand::Run {
                    run_id,
                    runner,
                    trusted_execution,
                    local_only,
                } => {
                    if runner {
                        run_runner_loop(
                            &context,
                            &RunnerAction::Run {
                                run_id: run_id.clone(),
                                trusted_execution,
                                local_only,
                            },
                        )?;
                        return Ok(0);
                    }
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    execute_runner_action(
                        &context,
                        prompt,
                        &RunnerAction::Run {
                            run_id: run_id.clone(),
                            trusted_execution,
                            local_only,
                        },
                        ui_mode,
                    )?;
                    return Ok(0);
                }
                RunsSubcommand::Gate { command } => {
                    let (run_id, gate_id, actor, reason, decision) = match command {
                        GateSubcommand::Approve {
                            run_id,
                            gate_id,
                            actor,
                            reason,
                        } => (
                            run_id,
                            gate_id,
                            actor,
                            reason,
                            transist::GateDecision::Approved,
                        ),
                        GateSubcommand::Reject {
                            run_id,
                            gate_id,
                            actor,
                            reason,
                        } => (
                            run_id,
                            gate_id,
                            actor,
                            reason,
                            transist::GateDecision::Rejected,
                        ),
                        GateSubcommand::RequestChanges {
                            run_id,
                            gate_id,
                            actor,
                            reason,
                        } => (
                            run_id,
                            gate_id,
                            actor,
                            reason,
                            transist::GateDecision::RequestChanges,
                        ),
                    };
                    let gate_result = context.record_gate_decision_with_input(
                        &run_id,
                        gate_id.as_deref(),
                        decision,
                        resolve_gate_decision_input(&context, actor.as_deref(), reason.as_deref())?,
                    )?;
                    if let Some(warning) = &gate_result.lease_release_warning {
                        eprintln!(
                            "warning: gate decision committed with a lease release warning: {}",
                            warning.message
                        );
                    }
                    let runner_action =
                        reentry_runner_action_for_run(&context, &gate_result.root_run_id)?;
                    execute_runner_action(&context, prompt, &runner_action, ui_mode)?;
                    return Ok(0);
                }
                RunsSubcommand::Evidence { command } => match command {
                    EvidenceSubcommand::Export { run_id } => {
                        let exported = context.run_evidence_export(&run_id)?;
                        if matches!(output_format, Some(CliOutputFormat::Json)) {
                            emit_json_output(
                                prompt,
                                &format!("transist runs evidence export {run_id}"),
                                &exported,
                            )?;
                        } else {
                            emit_text_output(
                                prompt,
                                &format!("transist runs evidence export {run_id}"),
                                &exported.markdown,
                            )?;
                        }
                    }
                },
                RunsSubcommand::Snapshot { command } => match command {
                    SnapshotSubcommand::Export {
                        run_id,
                        source_id,
                        workspace_id,
                        operator_label,
                        tree_version,
                        output,
                        max_payload_bytes,
                        sync_mode,
                        include_workflow_input_values,
                        include_event_payloads,
                        include_run_detail_full,
                        include_stderr_tail,
                        include_gate_request_summary,
                    } => export_run_snapshot(
                        &context,
                        prompt,
                        run_id,
                        source_id,
                        workspace_id,
                        operator_label,
                        tree_version,
                        output,
                        max_payload_bytes,
                        sync_mode,
                        include_workflow_input_values,
                        include_event_payloads,
                        include_run_detail_full,
                        include_stderr_tail,
                        include_gate_request_summary,
                    )?,
                },
                RunsSubcommand::List { all, ids_only } => {
                    let runs = listed_run_summaries(&context, all)?;
                    let command = format_runs_list_command(all, ids_only, output_format);
                    if ids_only {
                        emit_json_output(prompt, &command, &run_ids_from_summaries(&runs))?;
                    } else if matches!(output_format, Some(CliOutputFormat::Json)) {
                        emit_json_output(prompt, &command, &runs)?;
                    } else {
                        emit_text_output(prompt, &command, &format_runs_list_body(&runs, all))?;
                    }
                }
                RunsSubcommand::Watch { run_id } => {
                    crate::ui::watch::run_runs_watch(&context, &run_id)?;
                }
                RunsSubcommand::Cancel { run_id } => {
                    let cancellation =
                        crate::execution::settle_active_run_cancellation(&context, &run_id)?;
                    emit_json_output(prompt, "transist runs cancel", &cancellation)?;
                    if !cancellation.settled {
                        bail!(
                            "{}",
                            cancellation
                                .error
                                .as_deref()
                                .unwrap_or("cancellation did not settle")
                        );
                    }
                }
                RunsSubcommand::Resume {
                    run_id,
                    trusted_execution,
                    runner,
                    local_only,
                } => {
                    if runner {
                        run_runner_loop(
                            &context,
                            &RunnerAction::Resume {
                                run_id: run_id.clone(),
                                trusted_execution,
                                local_only,
                            },
                        )?;
                        return Ok(0);
                    }
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    execute_runner_action(
                        &context,
                        prompt,
                        &RunnerAction::Resume {
                            run_id: run_id.clone(),
                            trusted_execution,
                            local_only,
                        },
                        ui_mode,
                    )?;
                    return Ok(0);
                }
                RunsSubcommand::Retry {
                    run_id,
                    trusted_execution,
                    runner,
                    local_only,
                } => {
                    if runner {
                        run_runner_loop(
                            &context,
                            &RunnerAction::Resume {
                                run_id: run_id.clone(),
                                trusted_execution,
                                local_only,
                            },
                        )?;
                        return Ok(0);
                    }
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    execute_runner_action(
                        &context,
                        prompt,
                        &RunnerAction::Resume {
                            run_id: run_id.clone(),
                            trusted_execution,
                            local_only,
                        },
                        ui_mode,
                    )?;
                    return Ok(0);
                }
                RunsSubcommand::Cleanup {
                    run_id,
                    trusted_execution,
                    runner,
                    local_only,
                } => {
                    if runner {
                        run_runner_loop(
                            &context,
                            &RunnerAction::Cleanup {
                                run_id: run_id.clone(),
                                trusted_execution,
                                local_only,
                            },
                        )?;
                        return Ok(0);
                    }
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    crate::execution::ensure_run_tree_lease_free(
                        &context,
                        &run_id,
                        crate::execution::CLEANUP_START_LEASE_TIMEOUT,
                    )?;
                    let pid = spawn_and_attach(
                        &context,
                        prompt,
                        &RunnerAction::Cleanup {
                            run_id: run_id.clone(),
                            trusted_execution,
                            local_only,
                        },
                        ui_mode,
                    )?;
                    if ui_mode == CliUiMode::Plain {
                        wait_cleanup_settlement(&context, &run_id, pid, interrupt_flag);
                    }
                    return Ok(0);
                }
                RunsSubcommand::Replay {
                    run_id,
                    trusted_execution,
                    run_ref,
                    runner,
                    local_only,
                } => {
                    let replay_run_id = run_ref
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    if runner {
                        run_runner_loop(
                            &context,
                            &RunnerAction::Replay {
                                source_run_id: run_id.clone(),
                                replay_run_id: replay_run_id.clone(),
                                trusted_execution,
                                local_only,
                            },
                        )?;
                        return Ok(0);
                    }
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    execute_runner_action(
                        &context,
                        prompt,
                        &RunnerAction::Replay {
                            source_run_id: run_id.clone(),
                            replay_run_id: replay_run_id.clone(),
                            trusted_execution,
                            local_only,
                        },
                        ui_mode,
                    )?;
                    return Ok(0);
                }
                RunsSubcommand::SyncRecipe { run_id } => {
                    let report = context.runs_sync_recipe(&run_id)?;
                    emit_test_run_warning(&report.workflow_id, &report.workflow_version, &run_id);
                    emit_json_output(
                        prompt,
                        &format!("transist runs sync-recipe {}", run_id),
                        &report,
                    )?;
                }
                RunsSubcommand::Status { run_id } => {
                    for line in compact_run_status_lines(&context, &run_id, &recipe_cache)? {
                        println!("{}", line);
                    }
                    std::io::stdout().flush()?;
                }
                RunsSubcommand::Preflight {
                    run_id,
                    trusted_execution,
                    local_only,
                } => {
                    let load_context = context.clone();
                    let load_run_id = run_id.clone();
                    let report =
                        run_with_loading_feedback(prompt, "Running preflight", move || {
                            load_context.runs_preflight_with_trust(
                                &load_run_id,
                                trusted_execution,
                                local_only,
                            )
                        })?;
                    emit_test_run_warning_by_id(&context, &run_id)?;
                    emit_json_output(
                        prompt,
                        &format!("transist runs preflight {}", run_id),
                        &report,
                    )?;
                    if preflight_has_capability_denial(&report) {
                        eprintln!(
                            "Capability allowlist hint:\n  {}",
                            capability_hint_command(&run_id)
                        );
                    }
                }
                RunsSubcommand::Capabilities { run_id, exact } => {
                    let report = context.run_capabilities(&run_id)?;
                    let format = output_format.unwrap_or(CliOutputFormat::Yaml);
                    let command = format!(
                        "transist runs capabilities {} --format {}",
                        run_id,
                        capability_format_name(format)
                    );
                    match format {
                        CliOutputFormat::Json => {
                            if exact {
                                emit_json_output(prompt, &command, &report.missing_capabilities)?
                            } else {
                                emit_json_output(prompt, &command, &report.policy)?
                            }
                        }
                        CliOutputFormat::Yaml => {
                            let body = if exact {
                                yaml_serde::to_string(&report.missing_capabilities)?
                            } else {
                                yaml_serde::to_string(&report.policy)?
                            };
                            emit_text_output(prompt, &command, &body)?;
                        }
                        CliOutputFormat::Plain => unreachable!("plain output is rejected above"),
                    }
                }
                RunsSubcommand::Show {
                    run_id,
                    failed,
                    success,
                    all,
                    tui,
                } => {
                    if tui {
                        crate::ui::watch::run_runs_watch(&context, &run_id)?;
                    } else {
                        let filter = resolve_runs_show_step_filter(failed, success);
                        let load_context = context.clone();
                        let load_run_id = run_id.clone();
                        let run =
                            run_with_loading_feedback(prompt, "Loading run details", move || {
                                load_context.runs_show(&load_run_id)
                            })?;
                        let run_value = Value::from(run.clone());
                        emit_test_run_warning(
                            run_value["summary"]["workflow_id"]
                                .as_str()
                                .unwrap_or_default(),
                            run_value["summary"]["workflow_version"]
                                .as_str()
                                .unwrap_or_default(),
                            &run_id,
                        );
                        let run = filter_runs_show_steps(run_value, filter);
                        emit_json_output(
                            prompt,
                            &render_runs_show_request_command(&run_id, filter, all),
                            &run,
                        )?;
                    }
                }
                RunsSubcommand::Tail { run_id, limit } => {
                    let load_context = context.clone();
                    let load_run_id = run_id.clone();
                    let events =
                        run_with_loading_feedback(prompt, "Loading run events", move || {
                            load_context.runs_tail(&load_run_id, limit)
                        })?;
                    emit_json_output(
                        prompt,
                        &format!("transist runs tail {} --limit {}", run_id, limit),
                        &events,
                    )?;
                }
                RunsSubcommand::Lease { command } => match command {
                    RunsLeaseSubcommand::Inspect { run_id } => {
                        let load_context = context.clone();
                        let load_run_id = run_id.clone();
                        let lease = run_with_loading_feedback(
                            prompt,
                            "Loading lease details",
                            move || load_context.runs_lease_inspect(&load_run_id),
                        )?;
                        emit_json_output(
                            prompt,
                            &format!("transist runs lease inspect {}", run_id),
                            &lease,
                        )?;
                    }
                    RunsLeaseSubcommand::Break {
                        run_id,
                        yes_i_understand,
                        force_live_owner,
                    } => {
                        if !yes_i_understand {
                            return Err(anyhow::anyhow!(
                                "refusing to break the lease without --yes-i-understand"
                            ));
                        }
                        let lease = context.runs_lease_break(&run_id, force_live_owner)?;
                        emit_json_output(
                            prompt,
                            &format!("transist runs lease break {}", run_id),
                            &lease,
                        )?;
                    }
                },
            }
        }
        Some(Commands::Inspection { command }) => {
            let response = crate::handle_inspection_command_extension(
                command,
                repo_root,
                &resolved,
                interrupt_flag,
            )?;
            let exit_code = response.exit_code;
            emit_inspection_command_response(prompt, response)?;
            if exit_code != 0 {
                return Ok(exit_code);
            }
        }
        Some(Commands::Plugin { command }) => match command {
            PluginSubcommand::Install {
                path,
                trusted_execution,
                signature,
            } => {
                if !path.exists() {
                    bail!("plugins install path does not exist: {}", path.display());
                }
                let installed = if path.is_dir() {
                    context.install_plugin_dir_with_trust_and_signature(
                        &path,
                        trusted_execution,
                        signature.as_deref(),
                    )?
                } else {
                    context.install_plugin_file_with_trust_and_signature(
                        &path,
                        trusted_execution,
                        signature.as_deref(),
                    )?
                };
                emit_json_output(
                    prompt,
                    "transist plugins install",
                    &serde_json::json!({
                        "id": installed.installed.id,
                        "version": installed.installed.version,
                        "dir": installed.installed.dir,
                        "prerequisites": installed.prerequisites,
                    }),
                )?;
            }
            PluginSubcommand::List => {
                let plugins = context.list_plugins()?;
                emit_json_output(prompt, "transist plugins list", &plugins)?;
            }
            PluginSubcommand::Inspect { plugin_id } => {
                let manifest = context.inspect_plugin(&plugin_id)?;
                emit_json_output(
                    prompt,
                    &format!("transist plugins inspect {}", plugin_id),
                    &manifest,
                )?;
            }
            PluginSubcommand::Uninstall { plugin_id, version } => match version {
                Some(version) => {
                    context.remove_plugin(&plugin_id, &version)?;
                    emit_json_output(
                        prompt,
                        "transist plugins uninstall",
                        &serde_json::json!({
                            "uninstalled": plugin_id,
                            "version": version,
                        }),
                    )?;
                }
                None => {
                    let versions = context.uninstall_plugin(&plugin_id)?;
                    emit_json_output(
                        prompt,
                        "transist plugins uninstall",
                        &serde_json::json!({
                            "uninstalled": plugin_id,
                            "versions": versions,
                        }),
                    )?;
                }
            },
        },
    }

    Ok(0)
}

pub(crate) fn structured_bound_validation_output(validated: Value, report: Value) -> Value {
    structured_validation_output(validated, "bound", report)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_workflow_install(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    output_format: Option<CliOutputFormat>,
    path: Option<std::path::PathBuf>,
    package: Option<std::path::PathBuf>,
    inputs: Vec<String>,
    inputs_file: Option<std::path::PathBuf>,
    trusted_execution: bool,
    local_only: bool,
    signature: Option<std::path::PathBuf>,
    policy_output: Option<std::path::PathBuf>,
    dry_run: bool,
    approve: bool,
) -> Result<i32> {
    reject_removed_workflow_install_options(
        &inputs,
        inputs_file.as_deref(),
        local_only,
        signature.as_deref(),
        policy_output.as_deref(),
    )?;
    let governed = workflow_install_is_governed(context.home_dir())?;
    let path = resolve_workflow_install_source(path, package, governed, trusted_execution)?;
    let requires_trust = governed || trusted_execution;
    let package = transist::capture_workflow_package_with_signature(&path, None)?;
    let workflow_ref = package.workflow_ref()?;
    let signature = package.verify_signature(context.home_dir());
    let signature_verified = signature.as_ref().is_ok_and(|signature| signature.verified);
    if requires_trust && !signature_verified {
        bail!(
            "package signature verification failed (code={})",
            signature
                .ok()
                .and_then(|verification| verification.reason_code)
                .unwrap_or_else(|| "trust_denied".to_string())
        );
    }
    let validation = serde_json::to_value(context.validate_flow_file(&path)?)?;
    let already_installed = workflow_install_is_unchanged(
        context.home_dir(),
        &workflow_ref,
        package.definition_sha256(),
        requires_trust,
    )?;
    let mut report = workflow_install_output(
        if already_installed {
            "already_installed"
        } else {
            "planned"
        },
        &workflow_ref,
        &package,
        &path,
        governed,
        requires_trust,
        signature_verified,
        &validation,
    );
    report["schema"] = Value::String("transist.workflow_install.v1".to_string());
    if dry_run || already_installed {
        emit_json_output(prompt, "transist workflows install", &report)?;
        return Ok(0);
    }

    if !approve {
        let interactive = !matches!(output_format, Some(CliOutputFormat::Json))
            && prompt.confirmation_available();
        emit_json_output(prompt, "transist workflows install", &report)?;
        if !interactive {
            return Ok(1);
        }
        if !prompt.confirm_suggested_action(
            "Apply workflow installation?",
            "The reviewed immutable package will be written. Continue?",
        )? {
            report["status"] = Value::String("declined".to_string());
            emit_json_output(prompt, "transist workflows install", &report)?;
            return Ok(0);
        }
    }

    let lock = transist::lock_workflow_installation(context.home_dir())?;
    let install =
        context.install_workflow_package_snapshot_with_trust(&package, requires_trust, &lock)?;
    emit_test_workflow_install_warning(&install.workflow_id, &install.workflow_version);
    report["status"] = Value::String("applied".to_string());
    report["install"] = serde_json::to_value(install)?;
    emit_json_output(prompt, "transist workflows install", &report)?;
    Ok(0)
}

/// Rejects unsupported install options with operation-specific guidance.
fn reject_removed_workflow_install_options(
    inputs: &[String],
    inputs_file: Option<&Path>,
    local_only: bool,
    signature: Option<&Path>,
    policy_output: Option<&Path>,
) -> Result<()> {
    let option = if !inputs.is_empty() {
        Some("--input")
    } else if inputs_file.is_some() {
        Some("--inputs-file")
    } else if local_only {
        Some("--local-only")
    } else if signature.is_some() {
        Some("--signature")
    } else if policy_output.is_some() {
        Some("--policy-output")
    } else {
        None
    };
    if let Some(option) = option {
        bail!(
            "{option} is no longer supported by workflows install; install code first, then use workflows check with prospective inputs and workflows authorize for exact missing authority"
        );
    }
    Ok(())
}

fn workflow_install_is_governed(home_dir: &Path) -> Result<bool> {
    let raw = fs::read_to_string(home_dir.join("config.yaml"))?;
    let config: yaml_serde::Value = yaml_serde::from_str(&raw)?;
    Ok(config
        .get("runtime")
        .and_then(|runtime| runtime.get("mode"))
        .and_then(yaml_serde::Value::as_str)
        == Some("governed"))
}

/// Resolves the only accepted workflow artifact for installation.
fn resolve_workflow_install_source(
    path: Option<PathBuf>,
    package: Option<PathBuf>,
    governed: bool,
    trusted_execution: bool,
) -> Result<PathBuf> {
    match (path, package) {
        (Some(_), Some(_)) | (None, None) => {
            bail!("workflows install requires exactly one of --package or --path")
        }
        (Some(_), None) if governed || trusted_execution => {
            bail!(
                "governed or trusted workflow installation requires --package; --path is limited to unsigned standalone/development installation"
            )
        }
        (Some(path), None) => Ok(path),
        (None, Some(package)) => {
            let mut workflows = fs::read_dir(&package)
                .with_context(|| format!("failed reading workflow package: {}", package.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("yaml"))
                .collect::<Vec<_>>();
            workflows.sort();
            match workflows.as_slice() {
                [workflow] => Ok(workflow.clone()),
                [] => bail!(
                    "workflow package contains no top-level .yaml workflow: {}",
                    package.display()
                ),
                _ => bail!(
                    "workflow package must contain exactly one top-level .yaml workflow: {}",
                    package.display()
                ),
            }
        }
    }
}

fn workflow_install_is_unchanged(
    home_dir: &Path,
    workflow_ref: &str,
    digest: &str,
    trusted_execution: bool,
) -> Result<bool> {
    let Some((workflow_id, workflow_version)) = workflow_ref.split_once('@') else {
        return Ok(false);
    };
    let index_path = home_dir.join("registry/workflow-install-index.json");
    let Ok(raw) = fs::read(&index_path) else {
        return Ok(false);
    };
    let index: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid workflow install index: {}", index_path.display()))?;
    let Some(entry) = index.pointer(&format!("/entries/{workflow_ref}")) else {
        return Ok(false);
    };
    let recipe_path = home_dir
        .join("registry/recipes")
        .join(format!("{workflow_id}-{workflow_version}.yaml"));
    let recipe_matches = entry["recipe_sha256"].as_str().is_some_and(|expected| {
        fs::read(&recipe_path)
            .map(|bytes| {
                Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    == expected
            })
            .unwrap_or(false)
    });
    Ok(entry["workflow_definition_sha256"] == digest
        && entry["trusted_execution"] == trusted_execution
        && recipe_matches
        && (!trusted_execution
            || home_dir
                .join("registry/trust")
                .join(format!("{workflow_id}-{workflow_version}.json"))
                .is_file()))
}

#[allow(clippy::too_many_arguments)]
fn workflow_install_output(
    status: &str,
    workflow_ref: &str,
    package: &transist::WorkflowPackageSnapshot,
    package_path: &Path,
    governed: bool,
    trusted_execution: bool,
    signature_verified: bool,
    validation: &Value,
) -> Value {
    serde_json::json!({
        "schema": "transist.workflow_install.v1",
        "status": status,
        "run_created": false,
        "workflow": workflow_ref,
        "package": {
            "definition_sha256": package.definition_sha256(),
            "source_path": package_path.display().to_string(),
        },
        "governed": governed,
        "trusted_execution": trusted_execution,
        "signature_verified": signature_verified,
        "validation": validation,
    })
}

/// Builds the stable, read-only readiness report from bound validation evidence.
#[cfg(test)]
pub(crate) fn workflow_readiness_output(workflow: &str, validation: Result<Value>) -> Value {
    match validation {
        Ok(validation) => {
            let validation_status = validation
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            let status = match validation_status {
                "passed" => "ready",
                "failed" => "not_deliverable",
                _ => "not_ready",
            };
            let checks = validation
                .get("checks")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            serde_json::json!({
                "schema": "transist.workflow_readiness.v1",
                "workflow": workflow,
                "status": status,
                "run_created": false,
                "checks": checks,
                "validation": validation,
            })
        }
        Err(error) => serde_json::json!({
            "schema": "transist.workflow_readiness.v1",
            "workflow": workflow,
            "status": "invalid_definition",
            "run_created": false,
            "checks": [{
                "code": "bound_validation",
                "status": "failed",
                "message": error.to_string(),
            }],
        }),
    }
}

/// Produces canonical, input-redacted readiness evidence for an installed workflow.
pub(crate) fn workflow_readiness_evidence(
    context: &transist::TransistContext,
    workflow: &str,
    workflow_inputs: BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
    local_only: bool,
) -> Result<Value> {
    let report = context.workflow_readiness_report(
        workflow,
        workflow_inputs.clone(),
        trusted_execution,
        local_only,
    )?;
    readiness_evidence_from_report(context, workflow, &workflow_inputs, report)
}

/// Adds canonical evidence fingerprints to an already evaluated full report.
fn readiness_evidence_from_report(
    context: &transist::TransistContext,
    workflow: &str,
    workflow_inputs: &BTreeMap<String, transist::KernelValue>,
    report: transist::WorkflowReadinessReport,
) -> Result<Value> {
    #[cfg(test)]
    EVIDENCE_GENERATIONS.with(|count| count.set(count.get() + 1));
    let mut readiness = serde_json::to_value(report)?;
    add_readiness_diagnostics(&mut readiness);
    let inspection = context.inspect_workflow(workflow).ok();
    if let Some(inspection) = inspection.as_ref() {
        add_required_workflow_input_diagnostics(
            &mut readiness,
            &inspection.summary.inputs,
            workflow_inputs,
        );
    }
    let recipe = inspection
        .map(|inspection| serde_json::to_value(inspection.recipe))
        .transpose()?;
    let workspace_dir = readiness
        .get("validation")
        .and_then(|validation| validation.get("workspace_dir"))
        .cloned()
        .unwrap_or(Value::Null);
    let fingerprints = serde_json::json!({
        "workflow": canonical_fingerprint(&serde_json::json!({
            "workflow": workflow,
            "recipe": recipe,
        }))?,
        // Bound input values are deliberately hashed, never persisted in readiness evidence.
        "inputs": canonical_fingerprint(&serde_json::to_value(workflow_inputs)? )?,
        "workspace": canonical_fingerprint(&workspace_dir)?,
        "dependencies": filesystem_fingerprint(&context.home_dir().join("plugins"))?,
        "trust": canonical_fingerprint(&serde_json::json!({
            "roots": filesystem_fingerprint(&context.home_dir().join("trust"))?,
            "workflow_metadata": filesystem_fingerprint(&context.home_dir().join("registry/trust"))?,
        }))?,
        "policy": filesystem_fingerprint(&context.home_dir().join("policy"))?,
        "settings": canonical_fingerprint(&serde_json::json!({
            "config": filesystem_snapshot(&context.home_dir().join("config.yaml"))?,
            "user_settings": filesystem_snapshot(&context.home_dir().join("settings.yaml"))?,
            "toolchains": filesystem_snapshot(&context.home_dir().join("toolchains.yaml"))?,
            "installation": filesystem_snapshot(&context.home_dir().join("installation.json"))?,
            "workflow_registry": filesystem_snapshot(&context.home_dir().join("registry/workflow-install-index.json"))?,
        }))?,
        "preflight": canonical_fingerprint(&readiness)?,
    });
    let issued_at_unix_seconds = unix_seconds()?;
    let object = readiness
        .as_object_mut()
        .expect("readiness output is always a JSON object");
    object.insert(
        "issued_at_unix_seconds".to_string(),
        Value::Number(issued_at_unix_seconds.into()),
    );
    object.insert("fingerprints".to_string(), fingerprints);
    let fingerprint = readiness_fingerprint(&readiness)?;
    readiness
        .as_object_mut()
        .expect("readiness output is always a JSON object")
        .insert("fingerprint".to_string(), Value::String(fingerprint));
    Ok(readiness)
}

/// Writes one canonical readiness-evidence document without adding input values.
pub(crate) fn write_readiness_file(path: &Path, evidence: &Value) -> Result<()> {
    let canonical = canonical_json(evidence)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed creating readiness output dir: {}", parent.display())
        })?;
    }
    fs::write(path, canonical)
        .with_context(|| format!("failed writing readiness file: {}", path.display()))
}

/// Revalidates required readiness evidence before a new run can be published.
fn readiness_file_rejection(
    context: &transist::TransistContext,
    workflow: &str,
    workflow_inputs: BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
    local_only: bool,
    path: &Path,
    max_age: u64,
) -> Result<Option<Value>> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) => {
            return Ok(Some(readiness_rejection(
                workflow,
                "readiness_file",
                format!(
                    "failed reading readiness evidence '{}': {error}",
                    path.display()
                ),
            )));
        }
    };
    let evidence: Value = match serde_json::from_slice(&raw) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Ok(Some(readiness_rejection(
                workflow,
                "readiness_modified",
                format!("readiness evidence is not valid JSON: {error}"),
            )));
        }
    };
    if canonical_json(&evidence)? != raw {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_modified",
            "readiness evidence is not canonical JSON".to_string(),
        )));
    }
    if evidence.get("schema").and_then(Value::as_str) != Some("transist.workflow_readiness.v1")
        || evidence.get("workflow").and_then(Value::as_str) != Some(workflow)
        || evidence.get("run_created").and_then(Value::as_bool) != Some(false)
    {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_modified",
            "readiness evidence has an unsupported schema or does not bind this workflow"
                .to_string(),
        )));
    }
    let expected_fingerprint = match readiness_fingerprint(&evidence) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            return Ok(Some(readiness_rejection(
                workflow,
                "readiness_modified",
                format!("readiness evidence cannot be fingerprinted: {error}"),
            )));
        }
    };
    if evidence.get("fingerprint").and_then(Value::as_str) != Some(expected_fingerprint.as_str()) {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_modified",
            "readiness evidence fingerprint does not match its contents".to_string(),
        )));
    }
    let issued_at = match evidence
        .get("issued_at_unix_seconds")
        .and_then(Value::as_i64)
    {
        Some(issued_at) => issued_at,
        None => {
            return Ok(Some(readiness_rejection(
                workflow,
                "readiness_modified",
                "readiness evidence has no valid issue timestamp".to_string(),
            )));
        }
    };
    let now = unix_seconds()?;
    if issued_at > now {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_future",
            "readiness evidence was issued in the future".to_string(),
        )));
    }
    if now.saturating_sub(issued_at) > i64::try_from(max_age).expect("max age is bounded") {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_stale",
            format!("readiness evidence is older than {max_age} seconds"),
        )));
    }
    let current = match workflow_readiness_evidence(
        context,
        workflow,
        workflow_inputs,
        trusted_execution,
        local_only,
    ) {
        Ok(current) => current,
        Err(error) => {
            return Ok(Some(readiness_rejection(
                workflow,
                "readiness_drift",
                format!("readiness evidence could not be revalidated: {error:#}"),
            )));
        }
    };
    if evidence.get("status").and_then(Value::as_str) != Some("ready")
        || current.get("status").and_then(Value::as_str) != Some("ready")
        || evidence.get("fingerprints") != current.get("fingerprints")
    {
        return Ok(Some(readiness_rejection(
            workflow,
            "readiness_drift",
            "readiness evidence no longer matches current workflow, inputs, workspace, dependencies, trust, policy, settings, or preflight state"
                .to_string(),
        )));
    }
    Ok(None)
}

fn readiness_rejection(workflow: &str, code: &str, message: String) -> Value {
    serde_json::json!({
        "schema": "transist.workflow_readiness.v1",
        "workflow": workflow,
        "status": "not_deliverable",
        "run_created": false,
        "checks": [{
            "code": code,
            "status": "failed",
            "message": message,
        }],
    })
}

/// Hashes all canonical readiness evidence fields except its self-fingerprint.
pub(crate) fn readiness_fingerprint(evidence: &Value) -> Result<String> {
    let mut unsigned = evidence.clone();
    unsigned
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("readiness evidence must be a JSON object"))?
        .remove("fingerprint");
    canonical_fingerprint(&unsigned)
}

fn canonical_fingerprint(value: &Value) -> Result<String> {
    Ok(Sha256::digest(canonical_json(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("failed serializing canonical JSON")
}

fn filesystem_fingerprint(path: &Path) -> Result<String> {
    canonical_fingerprint(&filesystem_snapshot(path)?)
}

fn filesystem_snapshot(path: &Path) -> Result<Value> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({"kind": "missing"}));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed reading {}", path.display()));
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_file() {
        return Ok(serde_json::json!({
            "kind": "file",
            "sha256": Sha256::digest(fs::read(path)?)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        }));
    }
    if file_type.is_symlink() {
        return Ok(serde_json::json!({
            "kind": "symlink",
            "target": fs::read_link(path)?.to_string_lossy(),
        }));
    }
    if !file_type.is_dir() {
        return Ok(serde_json::json!({"kind": "other"}));
    }
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        entries.insert(
            entry.file_name().to_string_lossy().into_owned(),
            filesystem_snapshot(&entry.path())?,
        );
    }
    Ok(serde_json::json!({"kind": "directory", "entries": entries}))
}

fn unix_seconds() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs()
        .try_into()
        .context("system clock is outside supported readiness timestamp range")
}

/// Returns a rejection report when supplied readiness evidence cannot authorize run birth.
pub(crate) fn pre_dispatch_readiness(
    context: &transist::TransistContext,
    workflow: &str,
    workflow_inputs: BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
    local_only: bool,
    readiness_file: Option<&Path>,
    max_age: u64,
) -> Result<Option<Value>> {
    if let Some(path) = readiness_file {
        return readiness_file_rejection(
            context,
            workflow,
            workflow_inputs,
            trusted_execution,
            local_only,
            path,
            max_age,
        );
    }
    deterministic_pre_dispatch_readiness(
        context,
        workflow,
        workflow_inputs,
        trusted_execution,
        local_only,
    )
}

/// Returns a read-only rejection report when deterministic checks block fresh run birth.
pub(crate) fn deterministic_pre_dispatch_readiness(
    context: &transist::TransistContext,
    workflow: &str,
    workflow_inputs: BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
    local_only: bool,
) -> Result<Option<Value>> {
    let readiness = serde_json::to_value(context.workflow_readiness_report(
        workflow,
        workflow_inputs,
        trusted_execution,
        local_only,
    )?)?;
    Ok((readiness.get("status").and_then(Value::as_str) != Some("ready")).then_some(readiness))
}

pub(crate) fn structured_validation_output(
    validated: Value,
    mode: &'static str,
    report: Value,
) -> Value {
    let workflow = report.get("workflow_ref").cloned().unwrap_or(Value::Null);
    let valid = report
        .get("status")
        .and_then(Value::as_str)
        .is_none_or(|status| status != "failed");
    let workspace_dir = report.get("workspace_dir").cloned().unwrap_or(Value::Null);
    let checks = report
        .get("checks")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::json!({
        "validated": validated,
        "schema": transist::FLOW_SCHEMA_V1,
        "report_schema": "transist.workflow_validation.v1",
        "mode": mode,
        "workflow": workflow,
        "valid": valid,
        "workspace_dir": workspace_dir,
        "checks": checks,
        "report": report,
    })
}

fn resolve_gate_decision_input(
    context: &transist::TransistContext,
    actor_flag: Option<&str>,
    reason_flag: Option<&str>,
) -> Result<transist::GateDecisionInput> {
    let actor_env = std::env::var("TRANSIST_ACTOR").ok();
    let reason_env = std::env::var("TRANSIST_REASON").ok();
    let config_actor = load_gate_actor_from_home_config(context.home_dir())?;
    let user_settings_actor = transist::load_user_gate_actor()?;
    let (actor, actor_source, reason) = resolve_gate_decision_input_sources(
        actor_flag,
        reason_flag,
        actor_env.as_deref(),
        reason_env.as_deref(),
        config_actor.as_deref(),
        user_settings_actor.as_deref(),
    );

    Ok(transist::GateDecisionInput {
        actor,
        actor_source,
        reason,
        decided_at_ms: None,
    })
}

fn resolve_gate_decision_input_sources(
    actor_flag: Option<&str>,
    reason_flag: Option<&str>,
    actor_env: Option<&str>,
    reason_env: Option<&str>,
    config_actor: Option<&str>,
    user_settings_actor: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let actor_flag = normalize_optional_string(actor_flag);
    let reason_flag = normalize_optional_string(reason_flag);
    let actor_env = normalize_optional_string(actor_env);
    let reason_env = normalize_optional_string(reason_env);
    let config_actor = normalize_optional_string(config_actor);
    let user_settings_actor = normalize_optional_string(user_settings_actor);

    if let Some(actor) = actor_flag {
        return (
            Some(actor),
            Some("flag".to_string()),
            reason_flag.or(reason_env),
        );
    }
    if let Some(actor) = actor_env {
        return (
            Some(actor),
            Some("env".to_string()),
            reason_flag.or(reason_env),
        );
    }
    if let Some(actor) = config_actor {
        return (
            Some(actor),
            Some("config".to_string()),
            reason_flag.or(reason_env),
        );
    }
    if let Some(actor) = user_settings_actor {
        return (
            Some(actor),
            Some("user-settings".to_string()),
            reason_flag.or(reason_env),
        );
    }

    (None, None, reason_flag.or(reason_env))
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn load_gate_actor_from_home_config(home_dir: &Path) -> Result<Option<String>> {
    let path = home_dir.join("config.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed reading gate decision config: {}", path.display()))?;
    let parsed: yaml_serde::Value = yaml_serde::from_str(&raw)
        .context("invalid runtime config yaml for manual gate decision identity")?;
    Ok(parsed
        .get("manual_gates")
        .and_then(|value| value.get("actor"))
        .and_then(yaml_serde::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

fn execute_runner_action(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    action: &RunnerAction,
    ui_mode: CliUiMode,
) -> Result<()> {
    let run_id = action.run_id().to_string();
    let log_offset = crate::runner::runner_log_offset(context.home_dir(), &run_id);
    let pid = spawn_and_attach(context, prompt, action, ui_mode)?;

    // If the fullscreen TUI viewer was attached, it already surfaces early
    // dispatch failures via its own `wait_for_runner_decision` handoff.
    // Otherwise (plain mode, non-TTY, or dumb terminal), wait briefly for
    // early dispatch failures — e.g. asset-lock drift or terminal-status
    // rejection — so the operator sees the error on stderr instead of a
    // bare "spawned detached runner" message that looks like success.
    let tui_attached = crate::ui::should_use_tui(ui_mode)
        && std::io::stdout().is_terminal()
        && crate::ui::fullscreen::fullscreen_ui_available();
    if tui_attached {
        return Ok(());
    }

    let verdict = crate::runner::wait_for_runner_decision(
        pid,
        context.home_dir(),
        &run_id,
        log_offset,
        std::time::Duration::from_millis(500),
    )?;
    match verdict {
        crate::runner::RunnerEarlyVerdict::Exited(code) if code != 0 => {
            let tail = crate::runner::tail_runner_log(context.home_dir(), &run_id, 5);
            let summary = if tail.is_empty() {
                format!("runner exited with code {code} (see runner.log)")
            } else {
                format!("runner exited with code {code}: {tail}")
            };
            anyhow::bail!("detached runner failed: {summary}");
        }
        crate::runner::RunnerEarlyVerdict::FailureSignal(tail) => {
            anyhow::bail!("detached runner reported failure: {tail}");
        }
        _ => {}
    }
    Ok(())
}

fn resolve_runs_show_step_filter(failed: bool, success: bool) -> RunsShowStepFilter {
    if failed {
        RunsShowStepFilter::Failed
    } else if success {
        RunsShowStepFilter::Success
    } else {
        RunsShowStepFilter::All
    }
}

pub(crate) fn render_runs_show_request_command(
    run_id: &str,
    filter: RunsShowStepFilter,
    explicit_all: bool,
) -> String {
    match filter {
        RunsShowStepFilter::All if explicit_all => {
            format!("{} --all", format_runs_show_command(run_id, filter))
        }
        _ => format_runs_show_command(run_id, filter),
    }
}

pub(crate) fn filter_runs_show_steps(mut run: Value, filter: RunsShowStepFilter) -> Value {
    if matches!(filter, RunsShowStepFilter::All) {
        return run;
    }

    let Some(run_object) = run.as_object_mut() else {
        return run;
    };

    if matches!(filter, RunsShowStepFilter::Failed) {
        if let Some(preflight_checks) = run_object
            .get_mut("preflight")
            .and_then(|preflight| preflight.as_object_mut())
            .and_then(|preflight| preflight.get_mut("checks"))
            .and_then(|checks| checks.as_array_mut())
        {
            preflight_checks.retain(|check| {
                check
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| status == "failed")
                    .unwrap_or(false)
            });
        }
    }

    let Some(steps) = run_object.get_mut("steps") else {
        return run;
    };
    let Some(steps_object) = steps.as_object_mut() else {
        return run;
    };

    steps_object.retain(|_, step| match step.get("status").and_then(Value::as_str) {
        Some("failed") => matches!(filter, RunsShowStepFilter::Failed),
        Some("success" | "succeeded") => matches!(filter, RunsShowStepFilter::Success),
        _ => false,
    });

    run
}

pub(crate) fn emit_json_output(
    prompt: &mut dyn PromptUi,
    title: &str,
    value: &(impl serde::Serialize + ?Sized),
) -> Result<()> {
    let body = serde_json::to_string_pretty(value)?;
    if prompt.prefers_pager() || pager::should_page_command_output(&body) {
        prompt.present_paged_text(title, &body)?;
    } else {
        println!("{}", body);
        std::io::stdout().flush()?;
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static EVIDENCE_GENERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FULL_READINESS_EVALUATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Names required string inputs supplied empty after CLI overrides, without exposing values.
pub(crate) fn empty_required_workflow_inputs(
    specs: &[transist::WorkflowInspectionInputSummary],
    inputs: &BTreeMap<String, transist::KernelValue>,
) -> Vec<String> {
    specs
        .iter()
        .filter(|spec| spec.required && spec.value_type == "string")
        .filter(|spec| {
            matches!(
                inputs.get(&spec.name),
                Some(transist::KernelValue::String(value)) if value.trim().is_empty()
            )
        })
        .map(|spec| spec.name.clone())
        .collect()
}

/// Names required inputs that have neither a supplied value nor a declared default.
pub(crate) fn missing_required_workflow_inputs(
    specs: &[transist::WorkflowInspectionInputSummary],
    inputs: &BTreeMap<String, transist::KernelValue>,
) -> Vec<String> {
    specs
        .iter()
        .filter(|spec| spec.required && !spec.has_default && !inputs.contains_key(&spec.name))
        .map(|spec| spec.name.clone())
        .collect()
}

/// Adds missing input names to the shared diagnostic index without recording values.
pub(crate) fn add_missing_required_input_diagnostics(report: &mut Value, names: &[String]) {
    report["diagnostics"]["inputs"]["missing_required"] = serde_json::json!(names);
}

/// Adds input names, never values, to the diagnostic index shared by CLI reports.
pub(crate) fn add_empty_required_input_diagnostics(report: &mut Value, names: &[String]) {
    report["diagnostics"]["inputs"]["empty_required"] = serde_json::json!(names);
}

/// Enriches a readiness display with required input names using the same rule for CLI and TUI.
pub(crate) fn add_required_workflow_input_diagnostics(
    report: &mut Value,
    specs: &[transist::WorkflowInspectionInputSummary],
    inputs: &BTreeMap<String, transist::KernelValue>,
) {
    add_missing_required_input_diagnostics(
        report,
        &missing_required_workflow_inputs(specs, inputs),
    );
    add_empty_required_input_diagnostics(report, &empty_required_workflow_inputs(specs, inputs));
}

/// Returns whether a readiness report can be remediated by publishing only its
/// exact missing authority.
///
/// Readiness failures outside `run_policy` and `step_policy` require operator
/// action other than authorization, so callers must not offer an authority
/// prompt for them.
pub(crate) fn readiness_authorization_available(report: &Value) -> bool {
    let missing = report
        .pointer("/validation/missing_capabilities")
        .and_then(Value::as_array)
        .is_some_and(|capabilities| !capabilities.is_empty());
    let only_authority_blockers = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| {
            !matches!(
                check["status"].as_str(),
                Some("passed" | "deferred_to_dispatch")
            )
        })
        .all(|check| {
            check["status"].as_str() == Some("failed")
                && matches!(
                    check["code"].as_str(),
                    Some("run_policy" | "step_policy" | "capability_not_granted")
                )
        });

    report["status"] != "ready" && missing && only_authority_blockers
}

/// Renders only actionable findings, grouping repeated checks by code and message.
/// Inputs and secrets are never read or interpolated into this presentation.
pub(crate) fn render_readiness_text(report: &Value) -> String {
    let can_authorize = readiness_authorization_available(report);
    let mut groups =
        BTreeMap::<&str, BTreeMap<(String, String, String, Option<String>), Vec<String>>>::new();
    let mut failed = BTreeMap::<&str, usize>::new();
    for check in report["checks"].as_array().into_iter().flatten() {
        let status = check["status"].as_str().unwrap_or("unknown");
        if status == "passed" {
            continue;
        }
        let code = check["code"].as_str().unwrap_or("unknown");
        let group = readiness_check_group(code);
        if status == "failed" {
            *failed.entry(group).or_default() += 1;
        }
        let key = (
            status.to_string(),
            code.to_string(),
            check["message"].as_str().unwrap_or("no detail").to_string(),
            check["remediation"].as_str().map(str::to_string),
        );
        let paths = groups.entry(group).or_default().entry(key).or_default();
        if let Some(path) = check["step_path"].as_str() {
            paths.push(path.to_string());
        }
    }
    let mut body = format!(
        "Workflow: {}\nStatus: {}\nrun_created: false\n",
        report["workflow"].as_str().unwrap_or("unknown"),
        report["status"].as_str().unwrap_or("unknown")
    );
    if failed.is_empty() {
        body.push_str("Blocking checks: none\n");
    } else {
        body.push_str("Blocking checks:");
        for group in READINESS_GROUP_ORDER {
            if let Some(count) = failed.get(group) {
                body.push_str(&format!(" {group}={count}"));
            }
        }
        body.push('\n');
    }
    let input_failed = report["checks"].as_array().is_some_and(|checks| {
        checks.iter().any(|check| {
            check["status"] == "failed"
                && matches!(
                    check["code"].as_str(),
                    Some("input_schema" | "input_missing" | "input_binding" | "bound_validation")
                )
        })
    });
    if input_failed {
        if let Some(names) = report
            .pointer("/diagnostics/inputs/missing_required")
            .and_then(Value::as_array)
            .filter(|names| !names.is_empty())
        {
            body.push_str("\nMissing required workflow inputs:\n");
            for name in names.iter().filter_map(Value::as_str) {
                body.push_str(&format!(
                    "- {}: provide a value in the inputs file",
                    serde_json::to_string(name).expect("string serializes")
                ));
                if name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    body.push_str(&format!(" or pass --input {name}=<value>"));
                }
                body.push('\n');
            }
        }
        if let Some(names) = report
            .pointer("/diagnostics/inputs/empty_required")
            .and_then(Value::as_array)
            .filter(|names| !names.is_empty())
        {
            body.push_str("\nRequired workflow inputs supplied empty (possible cause):\n");
            for name in names.iter().filter_map(Value::as_str) {
                body.push_str(&format!(
                    "- {}: set a non-empty value in the inputs file",
                    serde_json::to_string(name).expect("string serializes")
                ));
                if name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    body.push_str(&format!(" or pass --input {name}=<value>"));
                }
                body.push('\n');
            }
        }
    }
    if let Some(step_path) = runtime_dependency_launch_blocker(report) {
        body.push_str(&format!(
            "\nLaunch blocked: {step_path} depends on values produced at runtime, so its request cannot be materialized before run birth. This workflow cannot start until that dependency is removed or made input-bound. No run was created.\n"
        ));
    }
    for group in READINESS_GROUP_ORDER {
        if let Some(findings) = groups.get(group) {
            body.push_str(&format!("\n{group}:\n"));
            for ((status, code, message, remediation), paths) in findings {
                body.push_str(&format!("- [{status}] {code}: {message}"));
                if !paths.is_empty() {
                    let shown = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                    body.push_str(&format!(" (steps: {shown}"));
                    if paths.len() > 3 {
                        body.push_str(&format!(", +{} more", paths.len() - 3));
                    }
                    body.push(')');
                }
                body.push('\n');
                if let Some(remediation) = remediation
                    .as_deref()
                    .filter(|_| group != &"Authority" || can_authorize)
                {
                    body.push_str(&format!("  Next: {remediation}\n"));
                }
            }
        }
    }
    if let Some(missing) = report
        .pointer("/validation/missing_capabilities")
        .and_then(Value::as_array)
        .filter(|missing| !missing.is_empty())
    {
        body.push_str(&format!(
            "\nMissing authority: {} capabilities.\n",
            missing.len()
        ));
    }
    if can_authorize {
        body.push_str("\nNext: review exact missing authority with `transist workflows authorize <id>@<version> --input <key=value>` or `--inputs-file <inputs.yaml>`, then rerun `workflows check`.\n");
    } else if failed.keys().any(|group| *group != "Authority") {
        body.push_str("\nNext: resolve non-authority blockers before reviewing grants.\n");
    }
    body
}

const READINESS_GROUP_ORDER: &[&str] = &[
    "Inputs / action schemas",
    "Workspace / definition",
    "Trust",
    "Dependencies",
    "Secrets",
    "External readiness",
    "Authority",
    "Other",
];

/// Classifies stable check codes for display; unknown codes remain visible.
pub(crate) fn readiness_check_group(code: &str) -> &'static str {
    match readiness_check_category(code) {
        "inputs" => "Inputs / action schemas",
        "workspace" => "Workspace / definition",
        "authority" => "Authority",
        "trust" => "Trust",
        "dependencies" => "Dependencies",
        "secrets" => "Secrets",
        "external" => "External readiness",
        _ => "Other",
    }
}

fn readiness_check_category(code: &str) -> &'static str {
    match code {
        "input_schema" | "input_missing" | "input_binding" => "inputs",
        "bound_validation" | "workspace" | "workspace_boundary" => "workspace",
        "run_policy" | "step_policy" | "capability_not_granted" => "authority",
        "trust" | "signature" | "workflow_trust" | "plugin_trust" => "trust",
        "plugin_contract" | "plugin_prerequisite" | "adapter_handshake" => "dependencies",
        "secret_value" | "secret_availability" | "secret_delivery" => "secrets",
        "network_egress_boundary" | "filesystem_workspace_boundary" => "external",
        _ => "other",
    }
}

/// Adds navigable, input-redacted check indices without changing the underlying checks.
pub(crate) fn add_readiness_diagnostics(report: &mut Value) {
    let mut groups = serde_json::Map::new();
    let mut blocking_codes = std::collections::BTreeSet::new();
    for (index, check) in report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let status = check["status"].as_str().unwrap_or("unknown");
        if status == "passed" {
            continue;
        }
        let code = check["code"].as_str().unwrap_or("unknown");
        let category = readiness_check_category(code);
        let group = groups
            .entry(category.to_string())
            .or_insert_with(|| serde_json::json!({"failed": 0, "warning": 0, "not_evaluated": 0, "deferred_to_dispatch": 0, "check_indices": []}));
        if let Some(count) = group.get_mut(status) {
            if let Some(value) = count.as_u64() {
                *count = serde_json::json!(value + 1);
            }
        }
        group["check_indices"]
            .as_array_mut()
            .expect("indices are an array")
            .push(serde_json::json!(index));
        if status == "failed" && category != "authority" {
            blocking_codes.insert(code.to_string());
        }
    }
    let missing_count = report
        .pointer("/validation/missing_capabilities")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let eligible = readiness_authorization_available(report);
    report["diagnostics"] = serde_json::json!({
        "groups": groups,
        "authorization": {
            "eligible": eligible,
            "missing_capabilities": missing_count,
            "blocking_codes": blocking_codes,
        }
    });
}

/// Adds the same readiness navigation to authorization JSON without changing the policy plan.
pub(crate) fn add_authorization_diagnostics(report: &mut Value) {
    add_readiness_diagnostics(&mut report["readiness"]);
    let mut diagnostics = report["readiness"]["diagnostics"].clone();
    if report["status"] == "blocked" && report["policy_output"].is_string() {
        diagnostics["authorization"]["plan_blocker"] =
            serde_json::json!("policy_destination_unavailable");
    }
    report["diagnostics"] = diagnostics;
}

/// Shows why authorization is blocked, or the exact grant plan before consent.
pub(crate) fn render_authorization_text(report: &Value) -> String {
    let status = report["status"].as_str().unwrap_or("unknown");
    let mut body = format!(
        "Workflow authorization: {status}\nWorkflow: {}\nrun_created: false\n",
        report["workflow"].as_str().unwrap_or("unknown")
    );
    if status == "blocked" {
        body.push_str("No grants written.\n\n");
        let readiness = &report["readiness"];
        body.push_str(&render_readiness_text(readiness));
        let pending = readiness
            .pointer("/validation/missing_capabilities")
            .and_then(Value::as_array)
            .filter(|missing| !missing.is_empty());
        if let Some(path) = report["policy_output"].as_str() {
            body.push_str(&format!(
                "Policy destination is unavailable: {path}. Choose an unused loaded policy path.\n"
            ));
        } else if pending.is_some() {
            body.push_str(&format!(
                "\nTo add missing grants after resolving the non-authority blockers, rerun:\n  transist workflows authorize {} --inputs-file <corrected-inputs-file>\nReuse any --input overrides from this attempt. In an interactive terminal, this command will show the exact grants and policy destination, then ask for confirmation. Use --dry-run to preview without writing. Grants are added to the effective home's policy, not to the workflow definition.\n",
                report["workflow"].as_str().unwrap_or("<id>@<version>")
            ));
        }
    } else if let Some(path) = report["policy_output"].as_str() {
        body.push_str(&format!("Policy destination: {path}\n"));
        if let Some(policy) = report.get("policy") {
            body.push_str("Exact proposed grants:\n");
            body.push_str(&serde_json::to_string_pretty(policy).expect("JSON value serializes"));
            body.push('\n');
        }
    }
    body
}

fn emit_authorization_output(
    prompt: &mut dyn PromptUi,
    output_format: Option<CliOutputFormat>,
    report: &impl serde::Serialize,
    missing_required_inputs: &[String],
    empty_required_inputs: &[String],
) -> Result<()> {
    let mut value = serde_json::to_value(report)?;
    add_authorization_diagnostics(&mut value);
    add_missing_required_input_diagnostics(&mut value["readiness"], missing_required_inputs);
    add_empty_required_input_diagnostics(&mut value["readiness"], empty_required_inputs);
    add_missing_required_input_diagnostics(&mut value, missing_required_inputs);
    add_empty_required_input_diagnostics(&mut value, empty_required_inputs);
    if !matches!(output_format, Some(CliOutputFormat::Plain)) {
        return emit_json_output(prompt, "transist workflows authorize", &value);
    }
    emit_text_output(
        prompt,
        "transist workflows authorize",
        &render_authorization_text(&value),
    )
}

/// Returns the step whose runtime-produced input prevents static run-birth
/// materialization, when that is the report's current readiness blocker.
fn runtime_dependency_launch_blocker(report: &Value) -> Option<&str> {
    (report["status"].as_str() == Some("not_ready"))
        .then(|| report["checks"].as_array())
        .flatten()
        .into_iter()
        .flatten()
        .find(|check| {
            check["status"].as_str() == Some("not_evaluated")
                && matches!(
                    check["code"].as_str(),
                    Some("mutable_variable" | "step_output")
                )
        })
        .and_then(|check| check["step_path"].as_str())
}

/// Renders prospective readiness for terminal users while retaining the exact
/// machine-readable report for JSON output.
fn emit_readiness_output(
    prompt: &mut dyn PromptUi,
    output_format: Option<CliOutputFormat>,
    report: &Value,
) -> Result<()> {
    if !matches!(output_format, Some(CliOutputFormat::Plain)) {
        return emit_json_output(prompt, "transist workflows check", report);
    }
    emit_text_output(
        prompt,
        "transist workflows check",
        &render_readiness_text(report),
    )
}

fn emit_text_output(prompt: &mut dyn PromptUi, title: &str, body: &str) -> Result<()> {
    if prompt.prefers_pager() || pager::should_page_command_output(body) {
        prompt.present_paged_text(title, body)?;
    } else if !body.is_empty() {
        println!("{}", body);
        std::io::stdout().flush()?;
    }
    Ok(())
}

fn emit_inspection_command_response(
    prompt: &mut dyn PromptUi,
    response: crate::InspectionCommandResponse,
) -> Result<()> {
    if let (Some(title), Some(value)) =
        (response.json_title.as_deref(), response.json_value.as_ref())
    {
        emit_json_output(prompt, title, value)?;
    }
    if !response.stderr_lines.is_empty() {
        let mut stderr = std::io::stderr().lock();
        for line in &response.stderr_lines {
            writeln!(stderr, "{line}")?;
        }
        stderr.flush()?;
    }
    Ok(())
}

fn preflight_has_capability_denial(report: &transist::PreflightReport) -> bool {
    report.checks.iter().any(|check| {
        check.message.contains("code=capability_not_granted")
            || check.message.contains("capability_not_granted")
            || check.message.contains("policy.allow.capabilities")
    })
}

fn capability_hint_command(run_id: &str) -> String {
    format!("transist runs capabilities {run_id} --format yaml")
}

fn capability_format_name(format: CliOutputFormat) -> &'static str {
    match format {
        CliOutputFormat::Yaml => "yaml",
        CliOutputFormat::Json => "json",
        CliOutputFormat::Plain => unreachable!("plain output is rejected above"),
    }
}

/// Best-effort passive reap of orphaned run-tree leases.
///
/// Called from every command path that either inspects run state or is about
/// to spawn a detached runner, so that a crashed prior runner cannot block a
/// fresh spawn or hide stale state from `runs list`. Failures are swallowed;
/// reaping is opportunistic and must never break the user's command.
fn reap_orphan_leases_best_effort(context: &transist::TransistContext) {
    let _ = context.release_orphan_leases();
}

pub(crate) fn reentry_runner_action_for_run(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<RunnerAction> {
    let run = Value::from(context.runs_show(run_id)?);
    let status = run["status"].as_str().unwrap_or_default();
    Ok(runner_action_for_status(run_id, status))
}

fn runner_action_for_status(run_id: &str, status: &str) -> RunnerAction {
    if matches!(status, "failed" | "canceled") {
        return RunnerAction::Resume {
            run_id: run_id.to_string(),
            trusted_execution: false,
            local_only: false,
        };
    }
    RunnerAction::Run {
        run_id: run_id.to_string(),
        trusted_execution: false,
        local_only: false,
    }
}

/// Poll the run state until cleanup reaches a terminal state, the runner
/// exits, or the operator interrupts. Prints a result summary when cleanup
/// settles. Does not cancel the detached runner on interrupt — the runner
/// continues independently.
#[allow(unsafe_code)]
fn wait_cleanup_settlement(
    context: &transist::TransistContext,
    run_id: &str,
    runner_pid: u32,
    interrupt_flag: &Arc<AtomicBool>,
) {
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    eprintln!(
        "transist: waiting for cleanup of run {} to settle (runner pid {})...",
        run_id, runner_pid
    );

    let deadline = Instant::now() + Duration::from_secs(300);
    let interval = Duration::from_millis(250);

    loop {
        if interrupt_flag.load(Ordering::SeqCst) {
            eprintln!(
                "transist: cleanup continues detached (runner pid {runner_pid}); \
                 use `transist runs watch {run_id}` or `transist runs status {run_id}` \
                 for results"
            );
            return;
        }

        if let Ok(run) = context.runs_show(run_id) {
            let run_val = Value::from(run);
            if let Some(cleanup) = run_val.get("cleanup") {
                let state = cleanup["state"].as_str().unwrap_or("");
                if matches!(state, "succeeded" | "failed" | "skipped") {
                    emit_cleanup_result_summary(context, run_id);
                    return;
                }
            }
        }

        #[cfg(unix)]
        {
            // SAFETY: `kill(pid, 0)` checks process liveness without sending
            // a signal; the pid is a validated runner pid from an active record.
            if unsafe { libc::kill(runner_pid as i32, 0) } != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    let log_tail = crate::runner::tail_runner_log(context.home_dir(), run_id, 5);
                    if !log_tail.is_empty() {
                        eprintln!("transist: cleanup runner exited; runner.log: {}", log_tail);
                    }
                    emit_cleanup_result_summary(context, run_id);
                    return;
                }
            }
        }

        if Instant::now() >= deadline {
            eprintln!(
                "transist: cleanup is still in progress after 300s; \
                 use `transist runs watch {run_id}` or `transist runs status {run_id}`"
            );
            return;
        }
        sleep(interval);
    }
}

/// Build a compact cleanup result summary from run state JSON.
///
/// Returns one line per summary entry. Does not emit secrets beyond
/// existing persisted inspection surfaces.
fn format_cleanup_result_summary(run: &Value, run_id: &str) -> Vec<String> {
    let mut lines = Vec::new();

    let run_status = run["status"].as_str().unwrap_or("unknown");

    let Some(cleanup) = run.get("cleanup") else {
        lines.push(format!(
            "transist: no cleanup record found for run {run_id}"
        ));
        return lines;
    };

    let state = cleanup["state"].as_str().unwrap_or("unknown");
    let trigger = cleanup["trigger"]
        .as_str()
        .map(|t| t.to_string())
        .unwrap_or_else(|| {
            cleanup["trigger"]
                .as_object()
                .and_then(|obj| obj.keys().next())
                .map(|k| k.to_string())
                .unwrap_or_else(|| String::from("unknown"))
        });

    lines.push(format!("Run:         {run_id} (status: {run_status})"));
    lines.push(format!("Cleanup:     state={state} trigger={trigger}"));

    if let Some(steps) = cleanup["steps"].as_object() {
        for (step_id, step) in steps {
            let step_status = step["status"].as_str().unwrap_or("unknown");
            let mut line = format!("  {step_id}: {step_status}");
            if step_status == "failed" {
                if let Some(msg) = step
                    .get("last_result")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    line.push_str(&format!(" — {msg}"));
                }
            }
            lines.push(line);
        }
    }

    lines.push(format!("Inspect:     transist runs show {run_id} --failed"));
    lines.push(format!("Watch:       transist runs watch {run_id}"));

    lines
}

/// Emit a compact cleanup result summary to stderr from persisted run state.
fn emit_cleanup_result_summary(context: &transist::TransistContext, run_id: &str) {
    let run = match context.runs_show(run_id) {
        Ok(run) => Value::from(run),
        Err(e) => {
            eprintln!("transist: could not read run state for cleanup summary: {e}");
            return;
        }
    };
    for line in format_cleanup_result_summary(&run, run_id) {
        eprintln!("{line}");
    }
}

fn listed_run_summaries(
    context: &transist::TransistContext,
    all: bool,
) -> Result<Vec<transist::InspectionRunSummary>> {
    let inspection = transist::TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    inspection.list_all_runs(
        if all {
            transist::RunListScope::All
        } else {
            transist::RunListScope::Roots
        },
        None,
    )
}

fn format_runs_list_command(
    all: bool,
    ids_only: bool,
    output_format: Option<CliOutputFormat>,
) -> String {
    let mut parts = vec!["transist runs list".to_string()];
    if all {
        parts.push("--all".to_string());
    }
    if ids_only {
        parts.push("--ids-only".to_string());
    }
    if let Some(format) = output_format {
        parts.push(format!(
            "--format {}",
            match format {
                CliOutputFormat::Json => "json",
                CliOutputFormat::Plain => "plain",
                CliOutputFormat::Yaml => "yaml",
            }
        ));
    }
    parts.join(" ")
}

fn run_ids_from_summaries(summaries: &[transist::InspectionRunSummary]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| summary.run_id.clone())
        .collect()
}

fn emit_test_run_warning_by_id(context: &transist::TransistContext, run_id: &str) -> Result<()> {
    let run = Value::from(context.runs_show(run_id)?);
    emit_test_run_warning(
        run["summary"]["workflow_id"].as_str().unwrap_or_default(),
        run["summary"]["workflow_version"]
            .as_str()
            .unwrap_or_default(),
        run_id,
    );
    Ok(())
}

fn emit_test_run_warning(workflow_id: &str, workflow_version: &str, run_id: &str) {
    if let Some(message) = test_run_warning_message(workflow_id, workflow_version, run_id) {
        eprintln!("{message}");
    }
}

fn emit_test_workflow_install_warning(workflow_id: &str, workflow_version: &str) {
    if transist::is_test_workflow(workflow_version) {
        eprintln!(
            "warning: workflow {}@{} is a TEST workflow; registry recipe overwrites and per-run recipe snapshot patching are enabled. Do not use in production.",
            workflow_id, workflow_version
        );
    }
}

fn test_run_warning_message(
    workflow_id: &str,
    workflow_version: &str,
    run_id: &str,
) -> Option<String> {
    transist::is_test_workflow(workflow_version).then(|| {
        format!(
            "warning: run '{}' is a TEST run (workflow {}@{}); registry recipe overwrites and per-run recipe snapshot patching are enabled. Do not use in production.",
            run_id,
            workflow_id,
            workflow_version
        )
    })
}

fn format_runs_list_body(summaries: &[transist::InspectionRunSummary], all: bool) -> String {
    let lines = if summaries.is_empty() {
        vec!["No runs found.".to_string()]
    } else {
        let mut lines = Vec::with_capacity(summaries.len() + 1);
        lines.push(format_runs_list_header(all));
        lines.extend(
            summaries
                .iter()
                .map(|summary| format_runs_list_line(summary, all)),
        );
        lines
    };
    lines.join("\n")
}

fn format_runs_list_header(include_parent: bool) -> String {
    let mut parts = vec![
        "[status   ]".to_string(),
        "run id".to_string(),
        "workflow".to_string(),
    ];
    if include_parent {
        parts.push("parent".to_string());
    }
    parts.push("details".to_string());
    parts.join("  ")
}

fn format_runs_list_line(summary: &transist::InspectionRunSummary, include_parent: bool) -> String {
    let run_id = summary.run_id.clone();
    let mut parts = vec![
        format!("[{:<9}]", summary.status),
        run_id,
        format!("{}@{}", summary.workflow_id, summary.workflow_version),
    ];
    if include_parent && let Some(parent_run_id) = summary.parent_run_id.as_ref() {
        parts.push(format!("parent={parent_run_id}"));
    }
    if let Some(step_id) = summary.current_step_id.as_ref() {
        parts.push(format!("step={step_id}"));
    }
    if summary.has_blocked_gates {
        parts.push("blocked-gates".to_string());
    }
    parts.join("  ")
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn workflow_cli_positional_dispatch_prepare_binds_inputs_and_run_refs() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        let flow = root.join("workflow.yaml");
        std::fs::write(&flow, "schema: transist.flow.v1\nid: positional.demo\nversion: v1\ninputs:\n  subject:\n    type: string\n    required: true\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
        let context = transist::TransistContext::with_home(&root, &home);
        context.compile_flow_file(&flow).unwrap();
        let file = root.join("inputs.json");
        std::fs::write(&file, r#"{"subject":"file"}"#).unwrap();
        for explicit in [true, false] {
            let mut args = vec![
                "transist",
                "--home",
                home.to_str().unwrap(),
                "workflows",
                "prepare",
                "positional.demo@v1",
                "--inputs-file",
                file.to_str().unwrap(),
                "--input",
                "subject=inline",
                "--local-only",
            ];
            if explicit {
                args.extend(["--run-ref", "chosen-run"]);
            }
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            let code = super::dispatch(
                cli,
                crate::cli::CliUiMode::Plain,
                &root,
                &root,
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                &mut crate::prompt::scripted::ScriptedPrompt::default(),
            )
            .unwrap();
            assert_eq!(code, 0);
        }
        let runs = context.runs_list().unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().any(|id| id == "chosen-run"));
        for id in runs {
            if id != "chosen-run" {
                assert!(uuid::Uuid::parse_str(&id).is_ok());
            }
            let saved: serde_json::Value = serde_json::from_slice(
                &std::fs::read(home.join(format!("runs/run_{id}/context/run_inputs.json")))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(saved["values"]["subject"], "inline");
            assert_eq!(saved["workflow"]["id"], "positional.demo");
            assert_eq!(saved["workflow"]["version"], "v1");
            let state: serde_json::Value = serde_json::from_slice(
                &std::fs::read(home.join(format!("runs/run_{id}/state.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(state["local_only_mode"], true);
        }
    }

    #[test]
    fn workflow_validation_fast_path_output_preserves_live_evidence_and_authored_checks() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        let flow = root.join("workflow.yaml");
        std::fs::write(&flow, "schema: transist.flow.v1\nid: report.demo\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
        transist::TransistContext::with_home(&root, &home)
            .compile_flow_file(&flow)
            .unwrap();
        let inputs_file = root.join("inputs.json");
        for target in [
            vec!["--path", flow.to_str().unwrap()],
            vec![
                "--path",
                flow.to_str().unwrap(),
                "--inputs-file",
                inputs_file.to_str().unwrap(),
            ],
            vec!["report.demo@v1"],
        ] {
            std::fs::write(root.join("inputs.json"), "{}").unwrap();
            let mut args = vec![
                "transist",
                "--home",
                home.to_str().unwrap(),
                "workflows",
                "check",
            ];
            args.extend(target);
            super::EVIDENCE_GENERATIONS.with(|count| count.set(0));
            super::FULL_READINESS_EVALUATIONS.with(|count| count.set(0));
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            super::dispatch(
                cli,
                crate::cli::CliUiMode::Plain,
                &root,
                &root,
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                &mut crate::prompt::scripted::ScriptedPrompt::default(),
            )
            .unwrap();
            super::EVIDENCE_GENERATIONS.with(|count| assert_eq!(count.get(), 0));
            super::FULL_READINESS_EVALUATIONS.with(|count| assert_eq!(count.get(), 0));
        }
        let output = root.join("readiness.json");
        let cli = crate::cli::Cli::try_parse_from([
            "transist",
            "--home",
            home.to_str().unwrap(),
            "workflows",
            "check",
            "report.demo@v1",
            "--output",
            output.to_str().unwrap(),
        ])
        .unwrap();
        super::dispatch(
            cli,
            crate::cli::CliUiMode::Plain,
            &root,
            &root,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            &mut crate::prompt::scripted::ScriptedPrompt::default(),
        )
        .unwrap();
        super::EVIDENCE_GENERATIONS.with(|count| assert_eq!(count.get(), 1));
        super::FULL_READINESS_EVALUATIONS.with(|count| assert_eq!(count.get(), 1));
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(evidence["evaluation_mode"], "live_readiness");
        let context = transist::TransistContext::with_home(&root, &home);
        assert!(
            super::readiness_file_rejection(
                &context,
                "report.demo@v1",
                Default::default(),
                false,
                false,
                &output,
                300,
            )
            .unwrap()
            .is_none()
        );
        assert!(context.runs_list().unwrap().is_empty());
    }

    #[test]
    fn workflow_validation_fast_path_default_plain_json_live_and_output_dispatch() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let context = transist::TransistContext::with_home(&workspace, &home);
        let plugin = root.join("plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("input.json"), "{}").unwrap();
        std::fs::write(plugin.join("output.json"), "{}").unwrap();
        std::fs::write(
            plugin.join("probe.py"),
            "#!/usr/bin/env python3\nraise RuntimeError('live probe invoked')\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                plugin.join("probe.py"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(plugin.join("plugin.yaml"), "schema: transist.plugin.v1\nid: fast.plugin\nversion: 1.0.0\nadapter: process\ndefault_provider_kind: local\nentrypoint: {type: process, command: probe.py}\npackage:\n  files: [probe.py, input.json, output.json]\nactions:\n  - name: echo\n    input_schema: input.json\n    output_schema: output.json\n    secret_inputs: []\n    bindings: {}\ncapabilities:\n  supports_cancel: false\n  supports_health: false\n  supports_artifact_hints: false\n  supports_evidence_hints: false\n").unwrap();
        context
            .install_plugin_file(&plugin.join("plugin.yaml"))
            .unwrap();
        let flow = root.join("workflow.yaml");
        std::fs::write(&flow, format!("schema: transist.flow.v1\nid: fast.demo\nversion: v1\nworkspace: {{path: {}, access: read_only}}\nplugins: {{fast.plugin: 1.0.0}}\nsteps:\n  - id: echo\n    uses: process:fast.plugin/echo\n    with: {{}}\n", workspace.display())).unwrap();
        context.install_flow_file_with_trust(&flow, false).unwrap();
        let output = root.join("readiness.json");
        for flags in [
            vec![],
            vec!["--format", "plain"],
            vec!["--format", "json"],
            vec!["--live"],
            vec!["--live", "--format", "json"],
            vec!["--output", output.to_str().unwrap()],
        ] {
            let live = flags.contains(&"--live") || flags.contains(&"--output");
            let evidence_expected =
                live && (flags.contains(&"json") || flags.contains(&"--output"));
            let mut args = vec![
                "transist",
                "--home",
                home.to_str().unwrap(),
                "workflows",
                "check",
                "fast.demo@v1",
            ];
            args.extend(flags);
            super::EVIDENCE_GENERATIONS.with(|count| count.set(0));
            let code = super::dispatch(
                crate::cli::Cli::try_parse_from(args).unwrap(),
                crate::cli::CliUiMode::Plain,
                &root,
                &root,
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                &mut crate::prompt::scripted::ScriptedPrompt::default(),
            )
            .unwrap();
            assert_eq!(code, i32::from(live));
            super::EVIDENCE_GENERATIONS
                .with(|count| assert_eq!(count.get(), usize::from(evidence_expected)));
            assert!(context.runs_list().unwrap().is_empty());
        }
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(evidence["evaluation_mode"], "live_readiness");
        assert!(evidence["fingerprint"].is_string());
        assert_ne!(evidence["status"], "ready");
        assert!(
            crate::cli::Cli::try_parse_from([
                "transist",
                "workflows",
                "check",
                "--path",
                flow.to_str().unwrap(),
                "--live"
            ])
            .is_err()
        );
        let (full, compact) = context
            .workflow_validation_report_with_summary(
                "fast.demo@v1",
                Default::default(),
                false,
                false,
            )
            .unwrap();
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["evaluation_mode"], "validation");
        assert_eq!(json["validation_status"], "passed");
        assert_eq!(json["status"], "not_ready");
        assert!(compact.is_validation_passed());
        assert!(!compact.is_ready());
        assert!(
            crate::commands::workflow_reports::render_check_summary(&compact)
                .contains("Validation passed; live readiness not checked.")
        );
    }

    #[test]
    fn workflow_cli_positional_dispatch_authorization_decline_and_json_preserve_no_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        std::fs::create_dir_all(home.join("policy/workflow-authorizations")).unwrap();
        std::fs::write(
            home.join("config.yaml"),
            "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n",
        )
        .unwrap();
        let flow = root.join("workflow.yaml");
        std::fs::write(&flow, "schema: transist.flow.v1\nid: report.demo\nversion: v1\nsteps:\n  - id: gate\n    control: control.gate.manual\n    with:\n      gate_id: review\n").unwrap();
        let context = transist::TransistContext::with_home(&root, &home);
        context.compile_flow_file(&flow).unwrap();
        for json in [false, true] {
            let mut args = vec![
                "transist",
                "--home",
                home.to_str().unwrap(),
                "workflows",
                "authorize",
                "report.demo@v1",
            ];
            if json {
                args.extend(["--format", "json"]);
            }
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            let mut prompt = crate::prompt::scripted::ScriptedPrompt::from_script(
                true,
                vec![],
                vec![],
                vec![],
                vec![],
                vec![false],
            );
            super::EVIDENCE_GENERATIONS.with(|count| count.set(0));
            let exit = super::dispatch(
                cli,
                crate::cli::CliUiMode::Plain,
                &root,
                &root,
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                &mut prompt,
            )
            .unwrap();
            assert_eq!(exit, i32::from(json));
            assert_eq!(prompt.action_confirmations.len(), usize::from(json));
            super::EVIDENCE_GENERATIONS.with(|count| assert_eq!(count.get(), 0));
            assert_eq!(
                std::fs::read_dir(home.join("policy/workflow-authorizations"))
                    .unwrap()
                    .count(),
                0
            );
            assert!(context.runs_list().unwrap().is_empty());
        }
    }
    use super::{
        RunnerAction, capability_hint_command, format_cleanup_result_summary,
        format_runs_list_body, format_runs_list_command, format_runs_list_header,
        format_runs_list_line, preflight_has_capability_denial,
        resolve_gate_decision_input_sources, run_ids_from_summaries, runner_action_for_status,
        test_run_warning_message,
    };
    use crate::cli::CliOutputFormat;

    #[test]
    fn runner_action_for_failed_status_uses_resume() {
        let action = runner_action_for_status("run-1", "failed");
        assert!(matches!(action, RunnerAction::Resume { .. }));
    }

    #[test]
    fn runner_action_for_blocked_status_uses_run() {
        let action = runner_action_for_status("run-1", "blocked");
        assert!(matches!(action, RunnerAction::Run { .. }));
    }

    #[test]
    fn format_runs_list_command_renders_requested_flags() {
        assert_eq!(
            format_runs_list_command(false, false, None),
            "transist runs list"
        );
        assert_eq!(
            format_runs_list_command(true, false, None),
            "transist runs list --all"
        );
        assert_eq!(
            format_runs_list_command(true, true, None),
            "transist runs list --all --ids-only"
        );
        assert_eq!(
            format_runs_list_command(true, false, Some(CliOutputFormat::Json)),
            "transist runs list --all --format json"
        );
        assert_eq!(
            format_runs_list_command(false, false, Some(CliOutputFormat::Plain)),
            "transist runs list --format plain"
        );
    }

    #[test]
    fn capability_hint_is_emitted_only_for_capability_denials() {
        let report = transist::PreflightReport {
            run_id: "run-1".to_string(),
            status: transist::PreflightStatus::Failed,
            checked_at_ms: 0,
            blocker_count: 1,
            warning_count: 0,
            checks: vec![transist::PreflightCheck {
                kind: "step_policy".to_string(),
                status: transist::PreflightStatus::Failed,
                step_ids: vec!["invoke".to_string()],
                adapter_kind: None,
                plugin_id: None,
                action: None,
                message: "policy.allow.capabilities is missing".to_string(),
            }],
            lease_release_warning: None,
        };

        assert!(preflight_has_capability_denial(&report));
        assert_eq!(
            capability_hint_command("run-1"),
            "transist runs capabilities run-1 --format yaml"
        );
        assert!(!preflight_has_capability_denial(
            &transist::PreflightReport {
                checks: vec![],
                ..report
            }
        ));
    }

    #[test]
    fn format_runs_list_line_renders_single_line_summary() {
        let root = sample_run_summary("root-run", "running", None, Some("review_plan"), false);
        let child = sample_run_summary(
            "root-run__child",
            "blocked",
            Some("root-run"),
            Some("review_changes"),
            true,
        );

        let root_line = format_runs_list_line(&root, false);
        let child_line = format_runs_list_line(&child, true);

        assert_eq!(
            root_line,
            "[running  ]  root-run  workflow@v1  step=review_plan"
        );
        assert_eq!(
            child_line,
            "[blocked  ]  root-run__child  workflow@v1  parent=root-run  step=review_changes  blocked-gates"
        );
        assert!(!root_line.contains('\n'));
        assert!(!child_line.contains('\n'));
    }

    #[test]
    fn format_runs_list_line_marks_test_runs() {
        let mut summary = sample_run_summary("test-run", "running", None, None, false);
        summary.workflow_version = transist::TEST_WORKFLOW_VERSION.to_string();

        assert_eq!(
            format_runs_list_line(&summary, false),
            "[running  ]  test-run  workflow@v0"
        );
    }

    #[test]
    fn test_run_warning_message_is_emitted_only_for_v0_runs() {
        let warning =
            test_run_warning_message("workflow", transist::TEST_WORKFLOW_VERSION, "run-1");
        #[cfg(feature = "dev-version-bypass")]
        {
            let warning = warning.expect("warning for test run");
            assert!(warning.contains("run 'run-1' is a TEST run"));
            assert!(warning.contains("workflow workflow@v0"));
        }
        #[cfg(not(feature = "dev-version-bypass"))]
        assert!(warning.is_none());
        assert!(test_run_warning_message("workflow", "1.0.0", "run-1").is_none());
    }

    #[test]
    fn format_runs_list_header_matches_requested_scope() {
        assert_eq!(
            format_runs_list_header(false),
            "[status   ]  run id  workflow  details"
        );
        assert_eq!(
            format_runs_list_header(true),
            "[status   ]  run id  workflow  parent  details"
        );
    }

    #[test]
    fn format_runs_list_body_uses_one_line_per_run() {
        let lines = format_runs_list_body(
            &[
                sample_run_summary("run-1", "success", None, None, false),
                sample_run_summary("run-2", "failed", None, Some("publish"), false),
            ],
            false,
        );
        let rendered_lines = lines.lines().collect::<Vec<_>>();
        assert_eq!(rendered_lines.len(), 3);
        assert_eq!(rendered_lines[0], "[status   ]  run id  workflow  details");
    }

    #[test]
    fn format_runs_list_body_handles_empty_runs() {
        assert_eq!(format_runs_list_body(&[], false), "No runs found.");
    }

    #[test]
    fn run_ids_from_summaries_preserves_display_order() {
        let summaries = vec![
            sample_run_summary("run-2", "running", None, Some("plan"), false),
            sample_run_summary("run-1", "success", None, None, false),
        ];
        assert_eq!(
            run_ids_from_summaries(&summaries),
            vec!["run-2".to_string(), "run-1".to_string()]
        );
    }

    #[test]
    fn gate_decision_input_prefers_flag_then_env_then_config() {
        let resolved = resolve_gate_decision_input_sources(
            Some("flag@example.com"),
            Some("because"),
            Some("env@example.com"),
            Some("env-reason"),
            Some("config@example.com"),
            Some("user@example.com"),
        );
        assert_eq!(resolved.0.as_deref(), Some("flag@example.com"));
        assert_eq!(resolved.1.as_deref(), Some("flag"));
        assert_eq!(resolved.2.as_deref(), Some("because"));

        let resolved = resolve_gate_decision_input_sources(
            None,
            None,
            Some("env@example.com"),
            Some("env-reason"),
            Some("config@example.com"),
            Some("user@example.com"),
        );
        assert_eq!(resolved.0.as_deref(), Some("env@example.com"));
        assert_eq!(resolved.1.as_deref(), Some("env"));
        assert_eq!(resolved.2.as_deref(), Some("env-reason"));

        let resolved = resolve_gate_decision_input_sources(
            None,
            Some("cli-reason"),
            None,
            Some("env-reason"),
            Some("config@example.com"),
            Some("user@example.com"),
        );
        assert_eq!(resolved.0.as_deref(), Some("config@example.com"));
        assert_eq!(resolved.1.as_deref(), Some("config"));
        assert_eq!(resolved.2.as_deref(), Some("cli-reason"));

        let resolved = resolve_gate_decision_input_sources(
            None,
            Some("cli-reason"),
            None,
            Some("env-reason"),
            None,
            Some("user@example.com"),
        );
        assert_eq!(resolved.0.as_deref(), Some("user@example.com"));
        assert_eq!(resolved.1.as_deref(), Some("user-settings"));
        assert_eq!(resolved.2.as_deref(), Some("cli-reason"));
    }

    fn sample_run_summary(
        run_id: &str,
        status: &str,
        parent_run_id: Option<&str>,
        current_step_id: Option<&str>,
        has_blocked_gates: bool,
    ) -> transist::InspectionRunSummary {
        transist::InspectionRunSummary {
            run_id: run_id.to_string(),
            workflow_id: "workflow".to_string(),
            workflow_version: "v1".to_string(),
            status: status.to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: current_step_id.map(str::to_string),
            has_blocked_gates,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: parent_run_id.unwrap_or(run_id).to_string(),
            parent_run_id: parent_run_id.map(str::to_string),
        }
    }

    #[test]
    fn format_cleanup_summary_succeeded() {
        let run = serde_json::json!({
            "status": "success",
            "cleanup": {
                "state": "succeeded",
                "trigger": "success",
                "steps": {
                    "on_success": {
                        "status": "succeeded"
                    }
                }
            }
        });
        let lines = format_cleanup_result_summary(&run, "demo-run");
        let text = lines.join("\n");
        assert!(text.contains("state=succeeded"), "text: {text}");
        assert!(text.contains("trigger=success"), "text: {text}");
        assert!(text.contains("on_success: succeeded"), "text: {text}");
        assert!(
            text.contains("transist runs show demo-run --failed"),
            "text: {text}"
        );
        assert!(
            text.contains("transist runs watch demo-run"),
            "text: {text}"
        );
    }

    #[test]
    fn format_cleanup_summary_failed_step_with_error() {
        let run = serde_json::json!({
            "status": "failed",
            "cleanup": {
                "state": "failed",
                "trigger": "failed",
                "steps": {
                    "cleanup_on_fail": {
                        "status": "failed",
                        "last_result": {
                            "error": {
                                "message": "permission denied"
                            }
                        }
                    }
                }
            }
        });
        let lines = format_cleanup_result_summary(&run, "demo-run");
        let text = lines.join("\n");
        assert!(text.contains("state=failed"), "text: {text}");
        assert!(text.contains("trigger=failed"), "text: {text}");
        assert!(
            text.contains("cleanup_on_fail: failed — permission denied"),
            "text: {text}"
        );
    }

    #[test]
    fn format_cleanup_summary_skipped() {
        let run = serde_json::json!({
            "status": "canceled",
            "cleanup": {
                "state": "skipped",
                "trigger": "canceled",
                "steps": {
                    "on_canceled": {
                        "status": "skipped"
                    }
                }
            }
        });
        let lines = format_cleanup_result_summary(&run, "demo-run");
        let text = lines.join("\n");
        assert!(text.contains("state=skipped"), "text: {text}");
        assert!(text.contains("trigger=canceled"), "text: {text}");
        assert!(text.contains("on_canceled: skipped"), "text: {text}");
    }

    #[test]
    fn format_cleanup_summary_no_cleanup_record() {
        let run = serde_json::json!({
            "status": "success"
        });
        let lines = format_cleanup_result_summary(&run, "demo-run");
        let text = lines.join("\n");
        assert!(text.contains("no cleanup record"), "text: {text}");
    }

    #[test]
    fn format_cleanup_summary_multiple_steps() {
        let run = serde_json::json!({
            "status": "success",
            "cleanup": {
                "state": "succeeded",
                "trigger": "success",
                "steps": {
                    "cleanup_a": {
                        "status": "succeeded"
                    },
                    "cleanup_b": {
                        "status": "skipped"
                    }
                }
            }
        });
        let lines = format_cleanup_result_summary(&run, "multi-run");
        let text = lines.join("\n");
        assert!(text.contains("cleanup_a: succeeded"), "text: {text}");
        assert!(text.contains("cleanup_b: skipped"), "text: {text}");
        assert!(
            text.contains("transist runs show multi-run --failed"),
            "text: {text}"
        );
    }
}
