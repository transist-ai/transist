// SPDX-License-Identifier: MPL-2.0
use anyhow::{Result, bail};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use crate::cli::{InspectionSubcommand, InspectionSyncMode};

/// Read-only summary of local inspection sync state shown in the fullscreen dashboard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectionSyncSummary {
    /// Whether a sync configuration file is present.
    pub configured: bool,
    /// Durable source identifier, if configured.
    pub source_id: Option<String>,
    /// Central registry URL, if configured.
    pub registry_url: Option<String>,
    /// Operator-supplied workspace label, if configured.
    pub workspace_id: Option<String>,
    /// Best-effort daemon status string.
    pub daemon_status: String,
    /// Persisted queue depth, if known.
    pub queue_depth: usize,
}

impl InspectionSyncSummary {
    /// Returns the default summary for builds without inspection sync support.
    pub fn unavailable() -> Self {
        Self {
            configured: false,
            source_id: None,
            registry_url: None,
            workspace_id: None,
            daemon_status: "unavailable".to_string(),
            queue_depth: 0,
        }
    }
}

/// Legacy inspection-sync configuration request retained for private extension
/// compatibility. The fullscreen TUI is read-only and never constructs this
/// request; configuration remains a CLI-only extension operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectionSyncConfigureRequest {
    /// Base central URL or full snapshot-ingest URL.
    pub registry_url: String,
    /// Central-issued durable source identifier.
    pub source_id: String,
    /// Operator/admin supplied workspace label.
    pub workspace_id: String,
    /// Sync profile selector.
    pub sync_profile: String,
    /// Explicit sync mode selector.
    pub sync_mode: InspectionSyncMode,
    /// Include workflow input values when sync mode is extended.
    pub include_workflow_input_values: bool,
    /// Include event payloads when sync mode is extended.
    pub include_event_payloads: bool,
    /// Include full run details when sync mode is extended.
    pub include_run_detail_full: bool,
    /// Include stderr tails when sync mode is extended.
    pub include_stderr_tail: bool,
    /// Include gate request summaries when sync mode is extended.
    pub include_gate_request_summary: bool,
}

/// Serialized response returned after an inspection command executes.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectionCommandResponse {
    /// Desired process exit code.
    pub exit_code: i32,
    /// Optional command label for paged JSON output.
    pub json_title: Option<String>,
    /// Optional JSON payload emitted to stdout.
    pub json_value: Option<Value>,
    /// Additional stderr lines printed after execution.
    pub stderr_lines: Vec<String>,
}

impl InspectionCommandResponse {
    /// Builds a JSON-emitting success response.
    pub fn json(title: impl Into<String>, value: Value) -> Self {
        Self {
            exit_code: 0,
            json_title: Some(title.into()),
            json_value: Some(value),
            stderr_lines: Vec::new(),
        }
    }

    /// Builds an exit-only response with no stdout payload.
    pub fn exit_only(exit_code: i32) -> Self {
        Self {
            exit_code,
            json_title: None,
            json_value: None,
            stderr_lines: Vec::new(),
        }
    }
}

/// Private/full-build CLI behavior injected by the composition root.
pub trait CliExtensions: Send + Sync {
    /// Returns whether the current build should expose inspection sync affordances.
    fn inspection_sync_available(&self) -> bool {
        true
    }

    /// Loads dashboard summary data for local inspection sync.
    fn load_inspection_sync_summary(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        resolved: &transist::ResolvedHome,
    ) -> Result<InspectionSyncSummary>;

    /// Legacy CLI-only configuration hook. Fullscreen callers must not invoke it.
    fn configure_inspection_sync(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        resolved: &transist::ResolvedHome,
        request: InspectionSyncConfigureRequest,
    ) -> Result<String>;

    /// Executes the parsed `inspection` subcommand.
    fn handle_inspection_command(
        &self,
        command: InspectionSubcommand,
        repo_root: &Path,
        resolved: &transist::ResolvedHome,
        interrupt_flag: &Arc<AtomicBool>,
    ) -> Result<InspectionCommandResponse>;
}

static CLI_EXTENSIONS: OnceLock<Arc<dyn CliExtensions>> = OnceLock::new();

/// Installs CLI extensions for the current process.
pub fn install_cli_extensions(extensions: Arc<dyn CliExtensions>) -> Result<()> {
    CLI_EXTENSIONS
        .set(extensions)
        .map_err(|_| anyhow::anyhow!("CLI extensions already installed"))
}

pub(crate) fn inspection_sync_available() -> bool {
    CLI_EXTENSIONS
        .get()
        .map(|extensions| extensions.inspection_sync_available())
        .unwrap_or(false)
}

pub(crate) fn load_inspection_sync_summary(
    repo_root: &Path,
    home_dir: &Path,
    resolved: &transist::ResolvedHome,
) -> InspectionSyncSummary {
    CLI_EXTENSIONS
        .get()
        .and_then(|extensions| {
            extensions
                .load_inspection_sync_summary(repo_root, home_dir, resolved)
                .ok()
        })
        .unwrap_or_else(InspectionSyncSummary::unavailable)
}

pub(crate) fn handle_inspection_command(
    command: InspectionSubcommand,
    repo_root: &Path,
    resolved: &transist::ResolvedHome,
    interrupt_flag: &Arc<AtomicBool>,
) -> Result<InspectionCommandResponse> {
    match CLI_EXTENSIONS.get() {
        Some(extensions) => {
            extensions.handle_inspection_command(command, repo_root, resolved, interrupt_flag)
        }
        None => bail!("inspection sync is unavailable in this build"),
    }
}
