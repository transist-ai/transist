// SPDX-License-Identifier: MPL-2.0
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// CLI surface selected by the composition root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliSurface {
    /// Full in-tree surface, retained for compatibility and local testing.
    Full,
    /// Core surface: core commands plus local run-control, without org-only flags or sync delivery.
    Core,
}

impl CliSurface {
    /// Returns the surface used by the current in-tree binary build.
    pub fn current_build() -> Self {
        Self::Core
    }
}

pub(crate) fn command_for_surface(surface: CliSurface) -> clap::Command {
    let command = Cli::command();
    match surface {
        CliSurface::Full => command,
        CliSurface::Core => core_surface_command(command),
    }
}

fn core_surface_command(command: clap::Command) -> clap::Command {
    let command = strip_managed_metadata_flags(command);
    strip_inspection_subcommand(command)
}

fn strip_managed_metadata_flags(command: clap::Command) -> clap::Command {
    #[cfg(feature = "managed-mode")]
    let command = command
        .mut_arg("management_mode", |arg| {
            arg.long("__managed-mode-internal").alias(None).hide(true)
        })
        .mut_arg("owner_id", |arg| {
            arg.long("__owner-id-internal").alias(None).hide(true)
        })
        .mut_arg("host_id", |arg| {
            arg.long("__host-id-internal").alias(None).hide(true)
        })
        .mut_arg("runner_id", |arg| {
            arg.long("__runner-id-internal").alias(None).hide(true)
        })
        .mut_arg("external_run_id", |arg| {
            arg.long("__external-run-id-internal")
                .alias(None)
                .hide(true)
        });

    command
}

fn strip_inspection_subcommand(command: clap::Command) -> clap::Command {
    command.mut_subcommand("inspection", |subcommand| {
        subcommand
            .name("__inspection-internal")
            .alias(None)
            .hide(true)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliOutputFormat {
    Json,
    #[value(alias = "text")]
    Plain,
    Yaml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliUiMode {
    Plain,
    Tui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
/// Inspection sync export mode.
pub enum InspectionSyncMode {
    /// Export metadata-only snapshots.
    MetadataOnly,
    /// Allow extended sync options that include additional run data.
    Extended,
}

impl InspectionSyncMode {
    /// Returns the runtime sync policy matching this CLI mode.
    pub fn as_runtime(self) -> transist::SyncPolicyMode {
        match self {
            Self::MetadataOnly => transist::SyncPolicyMode::MetadataOnly,
            Self::Extended => transist::SyncPolicyMode::Extended,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "transist", version, about = "Transist CLI")]
pub struct Cli {
    /// Override the effective Transist home for this command.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Choose plain output or the fullscreen TUI when supported.
    #[arg(long, global = true, value_enum)]
    pub ui: Option<CliUiMode>,

    /// Request a specific machine-readable or human-readable output format.
    #[arg(long, global = true, value_enum)]
    pub format: Option<CliOutputFormat>,

    #[cfg(feature = "managed-mode")]
    #[arg(long, global = true, hide = true)]
    pub management_mode: Option<String>,

    #[cfg(feature = "managed-mode")]
    #[arg(long, global = true, hide = true)]
    pub owner_id: Option<String>,

    #[cfg(feature = "managed-mode")]
    #[arg(long = "host-id", alias = "node-id", global = true, hide = true)]
    pub host_id: Option<String>,

    #[cfg(feature = "managed-mode")]
    #[arg(long = "runner-id", alias = "worker-id", global = true, hide = true)]
    pub runner_id: Option<String>,

    #[cfg(feature = "managed-mode")]
    #[arg(
        long = "external-run-id",
        alias = "fleet-run-id",
        global = true,
        hide = true
    )]
    pub external_run_id: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// View or change Transist home settings.
    Home {
        #[command(subcommand)]
        command: HomeSubcommand,
    },
    /// Validate and inspect the effective local runtime policy.
    Policy {
        #[command(subcommand)]
        command: PolicySubcommand,
    },
    /// List, inspect, and recover runs in the active Transist home.
    Runs {
        #[command(subcommand)]
        command: RunsSubcommand,
    },
    /// Install, inspect, and remove plugins.
    #[command(name = "plugins")]
    Plugin {
        #[command(subcommand)]
        command: PluginSubcommand,
    },
    /// Import and inspect trusted runtime public keys.
    Trust {
        #[command(subcommand)]
        command: TrustSubcommand,
    },
    /// Check, install, run, and inspect workflows.
    #[command(name = "workflows")]
    Workflow {
        #[command(subcommand)]
        command: WorkflowSubcommand,
    },
    /// Configure and run outbound local inspection sync.
    Inspection {
        #[command(subcommand)]
        command: InspectionSubcommand,
    },
}

#[derive(Debug, Subcommand)]
/// Inspection sync commands available only on the full CLI surface.
pub enum InspectionSubcommand {
    /// Configure local inspection sync without contacting central.
    Configure {
        /// Base central URL or full snapshot-ingest URL.
        #[arg(long = "registry-url")]
        registry_url: String,
        /// Central-issued durable source identifier.
        #[arg(long = "source-id")]
        source_id: String,
        /// Operator/admin supplied workspace label.
        #[arg(long = "workspace-id")]
        workspace_id: String,
        /// Optional operator or machine label.
        #[arg(long = "operator-label")]
        operator_label: Option<String>,
        /// Sync profile selector.
        #[arg(long = "sync-profile", default_value = "default")]
        sync_profile: String,
        /// Explicit sync mode selector.
        #[arg(long = "sync-mode", value_enum, default_value = "metadata_only")]
        sync_mode: InspectionSyncMode,
        /// Include workflow input values when sync mode is extended.
        #[arg(long = "include-workflow-input-values")]
        include_workflow_input_values: bool,
        /// Include event payloads when sync mode is extended.
        #[arg(long = "include-event-payloads")]
        include_event_payloads: bool,
        /// Include full run details when sync mode is extended.
        #[arg(long = "include-run-detail-full")]
        include_run_detail_full: bool,
        /// Include stderr tails when sync mode is extended.
        #[arg(long = "include-stderr-tail")]
        include_stderr_tail: bool,
        /// Include gate request summaries when sync mode is extended.
        #[arg(long = "include-gate-request-summary")]
        include_gate_request_summary: bool,
        /// Read a one-shot token from stdin and print an export command to stderr.
        #[arg(long = "token-stdin")]
        token_stdin: bool,
        /// Reserved for a future keyring-backed token store.
        #[arg(long = "keyring-store")]
        keyring_store: bool,
    },
    /// Push one run tree or a recent backfill window to central.
    Push {
        /// Optional run id to push. Child runs resolve to the root run tree.
        #[arg(required_unless_present = "since")]
        run_id: Option<String>,
        /// Relative lookback window such as `30d` for manual backfill.
        #[arg(long, conflicts_with = "run_id")]
        since: Option<String>,
        /// Build and validate snapshots without making a network request.
        #[arg(long)]
        dry_run: bool,
    },
    /// Send one heartbeat to central or print the payload.
    Heartbeat {
        /// Build and validate the heartbeat without making a network request.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the foreground inspection sync daemon.
    Daemon {
        /// Override the initial backfill window used when no fingerprints exist yet.
        #[arg(long = "backfill-window")]
        backfill_window: Option<String>,
        /// Force takeover of a stale daemon lock even if the recorded PID is still alive.
        #[arg(long)]
        force: bool,
        /// Clear a persisted terminal halt before starting the daemon again.
        #[arg(long)]
        resume: bool,
    },
    /// Show local inspection sync status.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum RunsSubcommand {
    /// Start or resume a previously prepared run.
    #[command(name = "start")]
    Run {
        /// Run id to start or resume.
        run_id: String,
        /// Internal: run the kernel headlessly in this process. Hidden from users.
        #[arg(long, hide = true)]
        runner: bool,
        /// Verify the persisted workflow and referenced plugins before execution.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
    },
    /// List recent runs with one-line status summaries.
    List {
        /// Include nested child runs created by loops or subflows.
        #[arg(long)]
        all: bool,
        /// Emit only run ids using the legacy compact JSON array format.
        #[arg(long)]
        ids_only: bool,
    },
    /// Open the fullscreen TUI to watch a run.
    Watch {
        /// Run id to inspect.
        run_id: String,
    },
    /// Interrupt an active run by requesting cancellation.
    Cancel {
        /// Run id to interrupt.
        run_id: String,
    },
    /// Resume a failed or canceled run.
    Resume {
        /// Run id to resume.
        run_id: String,
        /// Verify the persisted workflow and referenced plugins before resume.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Internal: run resume headlessly in this process (used by the
        /// detached runner). Hidden from users.
        #[arg(long, hide = true)]
        runner: bool,
    },
    /// Retry the failed step(s) of a failed or canceled run (alias for `resume`).
    Retry {
        /// Run id to retry.
        run_id: String,
        /// Verify the persisted workflow and referenced plugins before retry.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Internal: run retry headlessly in this process (used by the
        /// detached runner). Hidden from users.
        #[arg(long, hide = true)]
        runner: bool,
    },
    /// Run cleanup steps defined in the workflow for a finished run.
    Cleanup {
        /// Run id whose workflow cleanup steps should run.
        run_id: String,
        /// Verify the persisted workflow and referenced plugins before cleanup.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Internal: run cleanup headlessly in this process (used by the
        /// detached runner). Hidden from users.
        #[arg(long, hide = true)]
        runner: bool,
    },
    /// Start a fresh run from a previous run's saved recipe and inputs.
    Replay {
        /// Source run id to replay.
        run_id: String,
        /// Verify the source run's workflow and referenced plugins before replay.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Optional new run id. Omit to let Transist generate one.
        #[arg(long)]
        run_ref: Option<String>,
        /// Internal: replay headlessly in this process (used by the detached
        /// runner). Hidden from users.
        #[arg(long, hide = true)]
        runner: bool,
    },
    /// Replace a test run's snapshot with the current compiled registry recipe.
    SyncRecipe {
        /// Run id whose `recipe.yaml` should be refreshed from the registry.
        run_id: String,
    },
    /// Show a concise summary and suggested next step for a run.
    Status {
        /// Run id to summarize.
        run_id: String,
    },
    /// Re-run preflight checks for a prepared or persisted run.
    Preflight {
        /// Run id to check.
        run_id: String,
        /// Verify the persisted workflow and referenced plugins before preflight.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
    },
    /// Show the persisted capability requirements and current allowlist gap.
    Capabilities {
        /// Run id whose persisted authority should be inspected.
        run_id: String,
        /// Emit the legacy exact normalized capability list.
        #[arg(long)]
        exact: bool,
    },
    /// Show persisted run details.
    ///
    /// Emits JSON by default. Use `--tui` for the fullscreen inspector or
    /// `--failed` / `--success` to filter step results.
    Show {
        /// Run id to inspect.
        run_id: String,
        /// Show only failed steps.
        #[arg(long, conflicts_with_all = ["success", "all"])]
        failed: bool,
        /// Show only successful steps.
        #[arg(long, conflicts_with_all = ["failed", "all"])]
        success: bool,
        /// Show all steps explicitly.
        #[arg(long, conflicts_with_all = ["failed", "success"])]
        all: bool,
        /// Open the unified fullscreen run-inspection TUI instead of emitting JSON.
        #[arg(long, conflicts_with_all = ["failed", "success", "all"])]
        tui: bool,
    },
    /// Print recent or live event output for a run.
    Tail {
        /// Run id whose events should be shown.
        run_id: String,
        /// Maximum number of recent events to include.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Inspect or break run ownership leases.
    Lease {
        #[command(subcommand)]
        command: RunsLeaseSubcommand,
    },
    /// Record a decision for a manual gate and resume the run.
    Gate {
        #[command(subcommand)]
        command: GateSubcommand,
    },
    /// Export read-only evidence summaries for runs.
    Evidence {
        #[command(subcommand)]
        command: EvidenceSubcommand,
    },
    /// Export read-only local inspection snapshots for runs.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotSubcommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum GateSubcommand {
    /// Record an approval decision for a manual gate and resume the run.
    Approve {
        /// Run id to target. The top-level/root run id is usually enough.
        run_id: String,
        /// Optional specific gate id. Omit to decide the current blocked gate.
        gate_id: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Record a rejection decision for a manual gate and resume the run.
    Reject {
        /// Run id to target. The top-level/root run id is usually enough.
        run_id: String,
        /// Optional specific gate id. Omit to decide the current blocked gate.
        gate_id: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Record a request-changes decision for a manual gate and resume the run.
    RequestChanges {
        /// Run id to target. The top-level/root run id is usually enough.
        run_id: String,
        /// Optional specific gate id. Omit to decide the current blocked gate.
        gate_id: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RunsLeaseSubcommand {
    /// Print the persisted lease record for a run.
    Inspect {
        /// Run id whose lease should be inspected.
        run_id: String,
    },
    /// Force-break a run lease after acknowledging the ownership risk.
    Break {
        /// Run id whose lease should be broken.
        run_id: String,
        /// Required acknowledgement that lease breaking is destructive.
        #[arg(long = "yes-i-understand")]
        yes_i_understand: bool,
        /// Allow breaking a lease even when the recorded owner still appears live.
        #[arg(long)]
        force_live_owner: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginSubcommand {
    /// Install a plugin from a local manifest file or directory.
    Install {
        /// Path to the plugin's `plugin.yaml` manifest file (preferred) or to
        /// the plugin directory (compatibility).
        #[arg(long)]
        path: PathBuf,
        /// Require a trusted plugin signature during install.
        #[arg(long)]
        trusted_execution: bool,
        /// Detached signature path. Defaults to `<plugin-directory>.sig`.
        #[arg(long)]
        signature: Option<PathBuf>,
    },
    /// List installed plugins and their versions.
    List,
    /// Show the installed manifest for a plugin.
    Inspect {
        /// Plugin id to inspect.
        plugin_id: String,
    },
    /// Remove an installed plugin.
    Uninstall {
        /// Plugin id to remove.
        plugin_id: String,
        /// Uninstall a specific version only. If omitted, all installed versions are removed.
        #[arg(long)]
        version: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkflowSubcommand {
    /// Explain authored declarations without validation, authorization, or execution consent.
    /// No installation, probes, secret lookup, execution, or home writes. Use --format text|json.
    Explain {
        /// Authored workflow file, including its contained modules and assets.
        #[arg(long)]
        path: PathBuf,
        /// JSON or YAML file with ordinary prospective inputs (never secret values).
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Previous authored workflow to compare using the same inputs and contract snapshot.
        #[arg(long)]
        compare_to: Option<PathBuf>,
    },
    /// Validate authored or installed workflows; --live also probes installed plugins.
    /// Defaults to concise text, including pipes; --format json requests the full report.
    Check {
        /// Path to an authored workflow file to check.
        #[arg(
            long,
            required_unless_present = "workflow_ref",
            conflicts_with = "workflow_ref"
        )]
        path: Option<PathBuf>,
        /// Installed workflow id and version in the form `<id>@<version>`.
        #[arg(required_unless_present = "path", conflicts_with = "path", value_parser = parse_workflow_ref)]
        workflow_ref: Option<String>,
        /// Request fresh live plugin probes (installed workflows only).
        #[arg(long, requires = "workflow_ref", conflicts_with = "path")]
        live: bool,
        /// Input value in `key=value` form. Repeat to pass multiple inputs.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// JSON file containing workflow inputs.
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Check trusted-execution prerequisites without creating a run.
        #[arg(long)]
        trusted_execution: bool,
        /// Apply local-only policy restrictions while checking the workflow.
        #[arg(long)]
        local_only: bool,
        /// Select live probes and write canonical readiness evidence; stdout stays concise unless --format json.
        #[arg(long, requires = "workflow_ref", conflicts_with = "path")]
        output: Option<PathBuf>,
    },
    /// Review and install a workflow into the active home.
    Install {
        /// Path to an unsigned authored workflow (standalone/development only).
        #[arg(long, conflicts_with = "package")]
        path: Option<PathBuf>,
        /// Directory containing one signed workflow package (governed/trusted installs).
        #[arg(long, conflicts_with = "path")]
        package: Option<PathBuf>,
        /// Input value in `key=value` form. Repeat to pass multiple inputs.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// JSON or YAML file containing workflow inputs.
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Require trusted workflow verification in standalone or development homes.
        #[arg(long)]
        trusted_execution: bool,
        /// Apply local-only policy restrictions while validating the installation plan.
        #[arg(long)]
        local_only: bool,
        /// Detached signature path for standalone or development homes.
        #[arg(long)]
        signature: Option<PathBuf>,
        /// Policy-fragment destination for governed homes when a grant is required.
        #[arg(long)]
        policy_output: Option<PathBuf>,
        /// Report the complete installation plan without changing the active home.
        #[arg(long, conflicts_with = "approve")]
        dry_run: bool,
        /// Apply a valid installation plan without an interactive confirmation.
        #[arg(long, conflicts_with = "dry_run")]
        approve: bool,
    },
    /// Review and apply only the exact authority missing for a prospective run.
    /// Defaults to concise text, including pipes; --format json never prompts.
    Authorize {
        /// Installed workflow id and version in the form `<id>@<version>`.
        #[arg(value_parser = parse_workflow_ref)]
        workflow_ref: String,
        /// Input value in `key=value` form. Repeat to pass multiple inputs.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// JSON or YAML file containing the prospective workflow inputs.
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Optional policy fragment path beneath `<home>/policy`.
        #[arg(long)]
        policy_output: Option<PathBuf>,
        /// Report the authorization plan without changing the active home.
        #[arg(long, conflicts_with = "approve")]
        dry_run: bool,
        /// Apply a valid authorization plan without an interactive confirmation.
        #[arg(long, conflicts_with = "dry_run")]
        approve: bool,
    },
    /// Create a workflow run without starting execution.
    Prepare {
        /// Optional run id. Omit to let Transist generate one.
        #[arg(long)]
        run_ref: Option<String>,
        /// Workflow id and version in the form `<id>@<version>`.
        #[arg(value_parser = parse_workflow_ref)]
        workflow_ref: String,
        /// Input value in `key=value` form. Repeat to pass multiple inputs.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// JSON file containing workflow inputs.
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Verify the compiled workflow and referenced plugins before execution.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Require and revalidate canonical readiness evidence before creating the run.
        #[arg(long)]
        readiness_file: Option<PathBuf>,
        /// Maximum accepted readiness-evidence age in seconds.
        #[arg(
            long,
            requires = "readiness_file",
            default_value_t = 300,
            value_parser = clap::value_parser!(u64).range(1..=300)
        )]
        max_age: u64,
    },
    /// Create and start a workflow run.
    Run {
        /// Optional run id. Omit to let Transist generate one.
        #[arg(long)]
        run_ref: Option<String>,
        /// Workflow id and version in the form `<id>@<version>`.
        #[arg(value_parser = parse_workflow_ref)]
        workflow_ref: String,
        /// Input value in `key=value` form. Repeat to pass multiple inputs.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// JSON file containing workflow inputs.
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        /// Verify the compiled workflow and referenced plugins before execution.
        #[arg(long)]
        trusted_execution: bool,
        /// Block any action whose declared provider_kind is external before dispatch.
        #[arg(long)]
        local_only: bool,
        /// Require and revalidate canonical readiness evidence before creating the run.
        #[arg(long)]
        readiness_file: Option<PathBuf>,
        /// Maximum accepted readiness-evidence age in seconds.
        #[arg(
            long,
            requires = "readiness_file",
            default_value_t = 300,
            value_parser = clap::value_parser!(u64).range(1..=300)
        )]
        max_age: u64,
    },
    /// List workflows in the local registry.
    List,
    /// Show one workflow's execution recipe and summary.
    Inspect {
        /// Workflow id and version in the form `<id>@<version>`.
        workflow_ref: String,
    },
    /// Remove an installed workflow.
    Uninstall {
        /// Workflow id and version in the form `<id>@<version>`.
        workflow_ref: String,
    },
}

// Validate command targets during parsing, before home initialization or mutation.
fn parse_workflow_ref(value: &str) -> Result<String, String> {
    let Some((id, version)) = value.split_once('@') else {
        return Err("expected <workflow-id>@<version>".into());
    };
    if !id.starts_with(|ch: char| ch.is_ascii_alphanumeric())
        || !version.starts_with(|ch: char| ch.is_ascii_alphanumeric())
        || value.contains(char::is_whitespace)
        || !id
            .chars()
            .chain(version.chars())
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("expected <workflow-id>@<version> with nonempty identifier components".into());
    }
    Ok(value.to_owned())
}

#[derive(Debug, Subcommand)]
pub enum TrustSubcommand {
    /// Import one trusted public key record into the runtime trust store.
    Import {
        /// Path to a public, distributable `transist.trust_root.v1` JSON file.
        trust_root_file: PathBuf,
    },
    /// List trusted public keys imported into the runtime trust store.
    List,
    /// Show one imported trust-root record from the runtime trust store.
    Inspect {
        /// Stable trust-root key identifier.
        key_id: String,
    },
    /// Export one imported trust-root record to a file or stdout.
    Export {
        /// Stable trust-root key identifier.
        key_id: String,
        /// Output path or `-` for stdout.
        #[arg(long)]
        output: PathBuf,
    },
    /// Remove one imported trust-root record from the runtime trust store.
    Remove {
        /// Stable trust-root key identifier.
        key_id: String,
        /// Required acknowledgement for destructive local trust-store changes.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SnapshotSubcommand {
    /// Export one root-run inspection snapshot from local state.
    Export {
        /// Run id to export. Child runs resolve to the root run tree.
        run_id: String,
        /// Durable source identifier recorded in the envelope.
        #[arg(long = "source-id")]
        source_id: String,
        /// Workspace label recorded in the envelope.
        #[arg(long = "workspace-id")]
        workspace_id: String,
        /// Optional operator or machine label recorded in the envelope.
        #[arg(long = "operator-label")]
        operator_label: Option<String>,
        /// Monotonic tree version recorded in the envelope.
        #[arg(long = "tree-version", default_value_t = 0)]
        tree_version: u64,
        /// Output path or `-` for stdout.
        #[arg(long, default_value = "-")]
        output: PathBuf,
        /// Maximum allowed serialized payload size in bytes.
        #[arg(long = "max-payload-bytes")]
        max_payload_bytes: Option<usize>,
        /// Snapshot export mode.
        #[arg(long = "sync-mode", value_enum, default_value = "metadata_only")]
        sync_mode: InspectionSyncMode,
        /// Include workflow input values when sync mode is extended.
        #[arg(long = "include-workflow-input-values")]
        include_workflow_input_values: bool,
        /// Include event payloads when sync mode is extended.
        #[arg(long = "include-event-payloads")]
        include_event_payloads: bool,
        /// Include full run details when sync mode is extended.
        #[arg(long = "include-run-detail-full")]
        include_run_detail_full: bool,
        /// Include stderr tails when sync mode is extended.
        #[arg(long = "include-stderr-tail")]
        include_stderr_tail: bool,
        /// Include gate request summaries when sync mode is extended.
        #[arg(long = "include-gate-request-summary")]
        include_gate_request_summary: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum EvidenceSubcommand {
    /// Render a run evidence report from persisted local state.
    Export {
        /// Run id whose evidence should be exported.
        run_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum HomeSubcommand {
    /// Initialize the per-user default Transist home directory.
    Init {
        /// Select the runtime posture written to a new home.
        #[arg(long, value_enum, default_value = "governed")]
        mode: HomeInitMode,
    },
    /// Print the active Transist home path and resolution source.
    Get,
    /// Persist a user-level default Transist home path.
    Set {
        /// Absolute path to use as the user-level default Transist home.
        path: PathBuf,
    },
    /// Remove the persisted user-level default Transist home path.
    Unset,
}

#[derive(Debug, Subcommand)]
pub enum PolicySubcommand {
    /// Validate the composed policy without mutating the home.
    Validate,
    /// Print the composed configuration with policy.sources expanded.
    Show {
        /// Emit the flattened effective configuration.
        #[arg(long)]
        effective: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum HomeInitMode {
    Governed,
    Standalone,
    Development,
}

impl HomeInitMode {
    /// Converts the CLI mode selector to the runtime operating-mode enum.
    pub fn runtime_mode(self) -> transist::RuntimeOperatingMode {
        match self {
            Self::Governed => transist::RuntimeOperatingMode::Governed,
            Self::Standalone => transist::RuntimeOperatingMode::Standalone,
            Self::Development => transist::RuntimeOperatingMode::Development,
        }
    }
}

/// Committed recursive inventory of the shipped CLI command tree. Every
/// non-hidden command path and `--long` option, generated from
/// `command_for_surface(CliSurface::Core)` (the shipped surface). A command or
/// option added or removed without regenerating this fixture fails the
/// `recursive_command_inventory_matches_committed_fixture` test, keeping the
/// implementation in lockstep with `docs/public/reference/cli.md`.
#[cfg(test)]
pub const COMMAND_INVENTORY_JSON: &str = include_str!("command-inventory.json");

/// Serializes the recursive Clap command inventory, including visible command
/// aliases, options, defaults, requiredness, and positional arguments.
#[cfg(test)]
pub fn command_inventory_json(surface: CliSurface) -> String {
    let command = command_for_surface(surface);
    let mut entries = Vec::new();
    walk_command_inventory(&command, &mut Vec::new(), &mut entries);
    entries.sort_by_key(|entry| {
        entry["path"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|value| value.as_str())
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default()
    });
    serde_json::to_string_pretty(&serde_json::Value::Array(entries))
        .expect("serialize command inventory")
}

#[cfg(test)]
fn walk_command_inventory(
    command: &clap::Command,
    path: &mut Vec<String>,
    out: &mut Vec<serde_json::Value>,
) {
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
        let aliases: Vec<String> = arg
            .get_all_aliases()
            .unwrap_or_default()
            .into_iter()
            .map(|alias| format!("--{alias}"))
            .collect();
        let defaults: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        if arg.is_positional() {
            positionals.push(serde_json::json!({
                "name": arg.get_id().as_str(),
                "index": arg.get_index(),
                "required": arg.is_required_set(),
                "defaults": defaults,
            }));
            continue;
        }
        let Some(long) = arg.get_long() else { continue };
        options.push(serde_json::json!({
            "name": format!("--{long}"),
            "short": arg.get_short().map(|short| format!("-{short}")),
            "aliases": aliases,
            "required": arg.is_required_set(),
            "defaults": defaults,
            "global": arg.is_global_set(),
        }));
    }
    options.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    positionals.sort_by_key(|arg| arg["index"].as_u64());
    let mut aliases: Vec<String> = command.get_all_aliases().map(str::to_string).collect();
    aliases.sort();
    out.push(serde_json::json!({
        "path": path.clone(),
        "aliases": aliases,
        "options": options,
        "positionals": positionals,
    }));
    // Subcommands are emitted in sorted order so the committed fixture is
    // independent of declaration order in the derive tree. Hidden
    // subcommands (e.g. the renamed internal inspection surface on the Core
    // build) are not part of the user-facing command tree.
    let mut subcommands: Vec<&clap::Command> = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .collect();
    subcommands.sort_by(|a, b| a.get_name().cmp(b.get_name()));
    for subcommand in subcommands {
        path.push(subcommand.get_name().to_string());
        walk_command_inventory(subcommand, path, out);
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn workflow_cli_positional_rejects_usage_errors_before_home_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("uninitialized-home");
        for command in ["check", "authorize", "prepare", "run"] {
            // Intentional legacy syntax: rejection coverage, never an operational caller.
            for tail in [
                vec![],
                vec!["--workflow", "demo@v1"],
                vec!["demo@v1", "extra"],
                vec!["old-run", "--workflow", "demo@v1"],
                vec!["demo"],
                vec!["@v1"],
                vec!["demo@"],
                vec!["demo@v1@v2"],
                vec!["../demo@v1"],
                vec!["demo@../v1"],
                vec!["demo @v1"],
                vec![".demo@v1"],
                vec!["demo@_v1"],
            ] {
                let mut args = vec![
                    "transist",
                    "--home",
                    home.to_str().unwrap(),
                    "workflows",
                    command,
                ];
                args.extend(tail);
                assert!(Cli::try_parse_from(&args).is_err(), "accepted {args:?}");
                assert!(!home.exists());
            }
        }
        for tail in [
            vec!["--path", "workflow.yaml", "demo@v1"],
            vec!["--path", "workflow.yaml", "--live"],
            vec!["--path", "workflow.yaml", "--output", "readiness.json"],
            vec!["--live"],
            vec!["--output", "readiness.json"],
        ] {
            let mut args = vec![
                "transist",
                "--home",
                home.to_str().unwrap(),
                "workflows",
                "check",
            ];
            args.extend(tail);
            assert!(Cli::try_parse_from(&args).is_err(), "accepted {args:?}");
            assert!(!home.exists());
        }
    }

    #[test]
    fn workflow_cli_positional_accepts_check_modes_and_preserves_options() {
        for flags in [
            vec![],
            vec!["--live"],
            vec!["--output", "readiness.json"],
            vec!["--live", "--output", "readiness.json"],
        ] {
            let mut args = vec![
                "transist",
                "workflows",
                "check",
                "demo@v1",
                "--input",
                "subject=inline",
                "--inputs-file",
                "inputs.json",
                "--trusted-execution",
                "--local-only",
                "--format",
                "json",
            ];
            args.extend(flags.clone());
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Some(Commands::Workflow {
                command: WorkflowSubcommand::Check { workflow_ref: Some(reference), path: None,
                    inputs, inputs_file: Some(file), trusted_execution: true, local_only: true, live, output }
            }) if reference == "demo@v1" && inputs == ["subject=inline"] && file == PathBuf::from("inputs.json")
                && live == flags.contains(&"--live") && output.is_some() == flags.contains(&"--output")));
        }
        assert!(
            Cli::try_parse_from(["transist", "workflows", "check", "--path", "workflow.yaml"])
                .is_ok()
        );
    }

    #[test]
    fn workflow_cli_positional_accepts_explicit_and_generated_run_refs() {
        for command in ["prepare", "run"] {
            for explicit in [false, true] {
                let mut args = vec![
                    "transist",
                    "workflows",
                    command,
                    "demo@v1",
                    "--input",
                    "subject=inline",
                    "--inputs-file",
                    "inputs.json",
                    "--trusted-execution",
                    "--local-only",
                    "--readiness-file",
                    "readiness.json",
                    "--max-age",
                    "120",
                ];
                if explicit {
                    args.extend(["--run-ref", "chosen-run"]);
                }
                let cli = Cli::try_parse_from(args).unwrap();
                let Some(Commands::Workflow { command }) = cli.command else {
                    panic!("workflow command")
                };
                let (reference, run, inputs, file, trusted, local, evidence, age) = match command {
                    WorkflowSubcommand::Prepare {
                        workflow_ref,
                        run_ref,
                        inputs,
                        inputs_file,
                        trusted_execution,
                        local_only,
                        readiness_file,
                        max_age,
                    }
                    | WorkflowSubcommand::Run {
                        workflow_ref,
                        run_ref,
                        inputs,
                        inputs_file,
                        trusted_execution,
                        local_only,
                        readiness_file,
                        max_age,
                    } => (
                        workflow_ref,
                        run_ref,
                        inputs,
                        inputs_file,
                        trusted_execution,
                        local_only,
                        readiness_file,
                        max_age,
                    ),
                    _ => panic!("prepare or run"),
                };
                assert_eq!(reference, "demo@v1");
                assert_eq!(run.as_deref(), explicit.then_some("chosen-run"));
                assert_eq!(inputs, ["subject=inline"]);
                assert_eq!(file, Some(PathBuf::from("inputs.json")));
                assert!(trusted && local);
                assert_eq!(evidence, Some(PathBuf::from("readiness.json")));
                assert_eq!(age, 120);
            }
        }
    }

    #[test]
    fn workflow_cli_positional_help_and_inventory_match() {
        for name in ["check", "authorize", "prepare", "run"] {
            let mut command = command_for_surface(CliSurface::Core);
            let leaf = command
                .find_subcommand_mut("workflows")
                .unwrap()
                .find_subcommand_mut(name)
                .unwrap();
            let help = leaf.render_long_help().to_string();
            assert!(
                help.contains(if name == "check" {
                    "[WORKFLOW_REF]"
                } else {
                    "<WORKFLOW_REF>"
                }),
                "{help}"
            );
            // Intentional legacy spelling: ensure help exposes no removed option.
            assert!(!help.contains("--workflow"), "{help}");
            if matches!(name, "prepare" | "run") {
                assert!(help.contains("--run-ref <RUN_REF>"));
            }
            if name == "check" {
                assert!(help.contains("--live"));
            }
        }
        assert_eq!(
            command_inventory_json(CliSurface::Core).trim(),
            COMMAND_INVENTORY_JSON.trim()
        );
    }

    #[test]
    fn parse_prepare_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "prepare",
            "wf@v1",
            "--run-ref",
            "run-1",
            "--input",
            "ticket=JIRA-1",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Prepare { .. }
            })
        ));

        let trusted = Cli::try_parse_from([
            "transist",
            "workflows",
            "prepare",
            "wf@v1",
            "--run-ref",
            "run-1",
            "--trusted-execution",
        ])
        .expect("parse trusted prepare");
        assert!(matches!(
            trusted.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Prepare {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn workflow_run_parser_accepts_inherited_arguments() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "run",
            "wf@v1",
            "--run-ref",
            "run-1",
            "--input",
            "ticket=JIRA-1",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Run { .. }
            })
        ));

        let bounded = Cli::try_parse_from([
            "transist",
            "workflows",
            "run",
            "wf@v1",
            "--readiness-file",
            "readiness.json",
            "--max-age",
            "1",
        ])
        .expect("parse bounded readiness age");
        assert!(matches!(
            bounded.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Run { max_age: 1, .. }
            })
        ));
        assert!(
            Cli::try_parse_from([
                "transist",
                "workflows",
                "run",
                "wf@v1",
                "--readiness-file",
                "readiness.json",
                "--max-age",
                "301",
            ])
            .is_err(),
            "readiness age must not exceed 300 seconds"
        );
        assert!(
            Cli::try_parse_from([
                "transist",
                "workflows",
                "run",
                "wf@v1",
                "--readiness-file",
                "readiness.json",
                "--max-age",
                "0",
            ])
            .is_err(),
            "readiness age must be at least one second"
        );
    }

    #[test]
    fn parse_evidence_export_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "evidence", "export", "run-1"])
            .expect("parse evidence export");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Evidence {
                    command: EvidenceSubcommand::Export { .. }
                }
            })
        ));
    }

    #[test]
    fn parse_run_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "start", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Run { runner: false, .. }
            })
        ));

        let trusted =
            Cli::try_parse_from(["transist", "runs", "start", "run-1", "--trusted-execution"])
                .expect("parse trusted run");
        assert!(matches!(
            trusted.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Run {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_workflow_check_path_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "check",
            "--path",
            "workflows/demo/workflow.yaml",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Check { path: Some(path), .. }
            }) if path.as_path() == std::path::Path::new("workflows/demo/workflow.yaml")
        ));
    }

    #[test]
    fn parse_workflow_check_installed_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "check",
            "demo@v1",
            "--input",
            "repo_path=/tmp/demo",
            "--trusted-execution",
            "--local-only",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Check {
                    workflow_ref: Some(workflow),
                    inputs,
                    trusted_execution: true,
                    local_only: true,
                    ..
                }
            }) if workflow == "demo@v1" && inputs == vec!["repo_path=/tmp/demo"]
        ));
    }

    #[test]
    fn workflow_check_requires_exactly_one_target_and_restricts_output() {
        for args in [
            vec!["transist", "workflows", "check"],
            vec![
                "transist",
                "workflows",
                "check",
                "--path",
                "workflow.yaml",
                "demo@v1",
            ],
            vec![
                "transist",
                "workflows",
                "check",
                "--path",
                "workflow.yaml",
                "--output",
                "readiness.json",
            ],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "invalid check target parsed"
            );
        }

        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "check",
            "demo@v1",
            "--input",
            "repo_path=/tmp/demo",
            "--local-only",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Check {
                    path: None,
                    workflow_ref: Some(workflow),
                    inputs,
                    local_only: true,
                    ..
                }
            }) if workflow == "demo@v1" && inputs == vec!["repo_path=/tmp/demo"]
        ));
    }

    #[test]
    fn parse_workflow_install_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "install",
            "--path",
            "package/workflow.yaml",
            "--input",
            "ticket=OPS-1",
            "--inputs-file",
            "inputs.json",
            "--trusted-execution",
            "--local-only",
            "--signature",
            "package/workflow.sig",
            "--policy-output",
            "workflows/demo-v1.yaml",
            "--approve",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Install {
                    inputs,
                    trusted_execution: true,
                    local_only: true,
                    approve: true,
                    ..
                }
            }) if inputs == vec!["ticket=OPS-1"]
        ));
        assert!(
            Cli::try_parse_from([
                "transist",
                "workflows",
                "install",
                "--path",
                "workflow.yaml",
                "--dry-run",
                "--approve",
            ])
            .is_err()
        );
    }

    #[test]
    fn workflow_cli_positional_authorize_preserves_input_forms() {
        let parsed = Cli::try_parse_from([
            "transist",
            "workflows",
            "authorize",
            "demo@v1",
            "--input",
            "repo_path=/tmp/demo",
            "--inputs-file",
            "inputs.yaml",
            "--approve",
        ])
        .expect("parse authorization");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Authorize {
                    workflow_ref: workflow,
                    inputs,
                    inputs_file: Some(inputs_file),
                    approve: true,
                    ..
                }
            }) if workflow == "demo@v1"
                && inputs == vec!["repo_path=/tmp/demo"]
                && inputs_file == PathBuf::from("inputs.yaml")
        ));

        let inline_only = Cli::try_parse_from([
            "transist",
            "workflows",
            "authorize",
            "demo@v1",
            "--input",
            "ticket=OPS-1",
            "--dry-run",
        ])
        .expect("parse inline authorization inputs");
        assert!(matches!(
            inline_only.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Authorize {
                    inputs,
                    inputs_file: None,
                    dry_run: true,
                    ..
                }
            }) if inputs == vec!["ticket=OPS-1"]
        ));
    }

    #[test]
    fn parse_workflow_list_command() {
        let parsed = Cli::try_parse_from(["transist", "workflows", "list"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::List
            })
        ));
    }

    #[test]
    fn parse_workflow_inspect_command() {
        let parsed =
            Cli::try_parse_from(["transist", "workflows", "inspect", "demo@v1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Inspect { workflow_ref }
            }) if workflow_ref == "demo@v1"
        ));
    }

    #[test]
    fn parse_workflow_uninstall_command_and_reject_force() {
        let parsed =
            Cli::try_parse_from(["transist", "workflows", "uninstall", "demo@v1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Workflow {
                command: WorkflowSubcommand::Uninstall { workflow_ref }
            }) if workflow_ref == "demo@v1"
        ));
        assert!(
            Cli::try_parse_from(["transist", "workflows", "uninstall", "demo@v1", "--force"])
                .is_err()
        );
    }

    #[test]
    fn parse_home_command() {
        let parsed = Cli::try_parse_from(["transist", "home", "set", "/tmp/home"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Home {
                command: HomeSubcommand::Set { .. }
            })
        ));
    }

    #[test]
    fn parse_home_init_mode() {
        let parsed = Cli::try_parse_from(["transist", "home", "init", "--mode", "development"])
            .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Home {
                command: HomeSubcommand::Init {
                    mode: HomeInitMode::Development
                }
            })
        ));
    }

    #[test]
    fn parse_trust_import_command() {
        let parsed =
            Cli::try_parse_from(["transist", "trust", "import", "test-org.pub"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Trust {
                command: TrustSubcommand::Import { trust_root_file }
            }) if trust_root_file.as_path() == std::path::Path::new("test-org.pub")
        ));
    }

    #[test]
    fn trust_import_help_uses_trust_root_file_placeholder() {
        let mut command = Cli::command();
        let trust = command
            .find_subcommand_mut("trust")
            .expect("trust subcommand");
        let import = trust
            .find_subcommand_mut("import")
            .expect("trust import subcommand");
        let mut buffer = Vec::new();
        import.write_long_help(&mut buffer).expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("<TRUST_ROOT_FILE>"));
        assert!(help.contains("transist.trust_root.v1"));
        assert!(!help.contains("<PUBKEY_PATH>"));
    }

    #[test]
    fn parse_plugin_install_trusted_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "plugins",
            "install",
            "--path",
            "plugins/demo/plugin.yaml",
            "--trusted-execution",
        ])
        .expect("parse trusted plugins install");
        assert!(matches!(
            parsed.command,
            Some(Commands::Plugin {
                command: PluginSubcommand::Install {
                    trusted_execution: true,
                    ..
                }
            })
        ));

        let custom_signature = Cli::try_parse_from([
            "transist",
            "plugins",
            "install",
            "--path",
            "plugins/demo/plugin.yaml",
            "--signature",
            "signatures/demo.sig",
        ])
        .expect("parse plugins install custom signature");
        assert!(matches!(
            custom_signature.command,
            Some(Commands::Plugin {
                command: PluginSubcommand::Install {
                    signature: Some(signature),
                    ..
                }
            }) if signature == std::path::Path::new("signatures/demo.sig")
        ));
    }

    #[test]
    fn parse_trust_list_command() {
        let parsed = Cli::try_parse_from(["transist", "trust", "list"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Trust {
                command: TrustSubcommand::List
            })
        ));
    }

    #[test]
    fn parse_trust_inspect_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "trust",
            "inspect",
            "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Trust {
                command: TrustSubcommand::Inspect { key_id }
            }) if key_id == "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55"
        ));
    }

    #[test]
    fn parse_trust_export_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "trust",
            "export",
            "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55",
            "--output",
            "-",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Trust {
                command: TrustSubcommand::Export { key_id, output }
            }) if key_id == "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55"
                && output.as_os_str() == "-"
        ));
    }

    #[test]
    fn parse_trust_remove_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "trust",
            "remove",
            "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55",
            "--yes",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Trust {
                command: TrustSubcommand::Remove { key_id, yes }
            }) if key_id == "3ad05c4a519045ba1ed336946d5dce009b97824141fca25dec79972e5a0fee55" && yes
        ));
    }

    #[test]
    fn parse_snapshot_export_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "runs",
            "snapshot",
            "export",
            "run-1",
            "--source-id",
            "src_example",
            "--workspace-id",
            "example/workflows",
            "--operator-label",
            "alice",
            "--tree-version",
            "7",
            "--output",
            "snapshot.json",
            "--max-payload-bytes",
            "4096",
            "--sync-mode",
            "extended",
            "--include-workflow-input-values",
            "--include-event-payloads",
            "--include-run-detail-full",
            "--include-stderr-tail",
            "--include-gate-request-summary",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Snapshot {
                    command: SnapshotSubcommand::Export {
                    run_id,
                    source_id,
                    workspace_id,
                    operator_label,
                    tree_version,
                    output,
                    max_payload_bytes,
                    sync_mode: InspectionSyncMode::Extended,
                    include_workflow_input_values: true,
                    include_event_payloads: true,
                    include_run_detail_full: true,
                    include_stderr_tail: true,
                    include_gate_request_summary: true,
                    }
                }
            }) if run_id == "run-1"
                && source_id == "src_example"
                && workspace_id == "example/workflows"
                && operator_label.as_deref() == Some("alice")
                && tree_version == 7
                && output.as_os_str() == "snapshot.json"
                && max_payload_bytes == Some(4096)
        ));
    }

    #[test]
    fn parse_global_home_override() {
        let parsed = Cli::try_parse_from(["transist", "--home", "/tmp/home", "runs", "list"])
            .expect("parse");
        assert_eq!(parsed.home, Some(PathBuf::from("/tmp/home")));
        assert_eq!(parsed.ui, None); // Default, not explicitly set
        assert_eq!(parsed.format, None);
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::List {
                    all: false,
                    ids_only: false
                }
            })
        ));
    }

    #[test]
    fn parse_runs_list_all_flag() {
        let parsed = Cli::try_parse_from(["transist", "runs", "list", "--all"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::List {
                    all: true,
                    ids_only: false
                }
            })
        ));
    }

    #[test]
    fn parse_runs_list_ids_only_flag() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "list", "--ids-only"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::List {
                    all: false,
                    ids_only: true
                }
            })
        ));
    }

    #[test]
    fn parse_global_output_format_after_subcommand() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "list", "--format", "json"]).expect("parse");
        assert_eq!(parsed.format, Some(CliOutputFormat::Json));
    }

    #[test]
    fn parse_runs_watch_command_with_tui_ui() {
        let parsed = Cli::try_parse_from(["transist", "--ui", "tui", "runs", "watch", "run-1"])
            .expect("parse");
        assert_eq!(parsed.ui, Some(CliUiMode::Tui));
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Watch { run_id }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_cancel_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "cancel", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Cancel { run_id }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_rejects_auto_ui_mode() {
        let parsed = Cli::try_parse_from(["transist", "--ui", "auto", "runs", "list"]);
        assert!(parsed.is_err());
    }

    #[cfg(feature = "managed-mode")]
    #[test]
    fn parse_hidden_managed_metadata_flags_with_new_and_legacy_aliases() {
        let parsed = Cli::try_parse_from([
            "transist",
            "--management-mode",
            "managed",
            "--owner-id",
            "owner-1",
            "--host-id",
            "host-1",
            "--runner-id",
            "runner-1",
            "--external-run-id",
            "run-ext-1",
            "runs",
            "list",
        ])
        .expect("parse managed flags");
        assert_eq!(parsed.management_mode.as_deref(), Some("managed"));
        assert_eq!(parsed.owner_id.as_deref(), Some("owner-1"));
        assert_eq!(parsed.host_id.as_deref(), Some("host-1"));
        assert_eq!(parsed.runner_id.as_deref(), Some("runner-1"));
        assert_eq!(parsed.external_run_id.as_deref(), Some("run-ext-1"));

        let legacy = Cli::try_parse_from([
            "transist",
            "--node-id",
            "host-legacy",
            "--worker-id",
            "runner-legacy",
            "--fleet-run-id",
            "run-legacy",
            "runs",
            "list",
        ])
        .expect("parse legacy aliases");
        assert_eq!(legacy.host_id.as_deref(), Some("host-legacy"));
        assert_eq!(legacy.runner_id.as_deref(), Some("runner-legacy"));
        assert_eq!(legacy.external_run_id.as_deref(), Some("run-legacy"));
    }

    #[cfg(not(feature = "managed-mode"))]
    #[test]
    fn managed_flags_are_absent_without_feature() {
        assert!(
            Cli::try_parse_from(["transist", "--management-mode", "managed", "runs", "list"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["transist", "--owner-id", "owner-1", "runs", "list"]).is_err()
        );
        assert!(Cli::try_parse_from(["transist", "--host-id", "host-1", "runs", "list"]).is_err());
        assert!(
            Cli::try_parse_from(["transist", "--runner-id", "runner-1", "runs", "list"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["transist", "--external-run-id", "run-ext-1", "runs", "list"])
                .is_err()
        );
    }

    #[test]
    fn parse_runs_resume_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "resume", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Resume { .. }
            })
        ));

        let trusted =
            Cli::try_parse_from(["transist", "runs", "resume", "run-1", "--trusted-execution"])
                .expect("parse trusted resume");
        assert!(matches!(
            trusted.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Resume {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_runs_retry_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "retry", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Retry { .. }
            })
        ));

        let trusted =
            Cli::try_parse_from(["transist", "runs", "retry", "run-1", "--trusted-execution"])
                .expect("parse trusted retry");
        assert!(matches!(
            trusted.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Retry {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_runs_cleanup_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "cleanup", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Cleanup { .. }
            })
        ));

        let trusted = Cli::try_parse_from([
            "transist",
            "runs",
            "cleanup",
            "run-1",
            "--trusted-execution",
        ])
        .expect("parse trusted cleanup");
        assert!(matches!(
            trusted.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Cleanup {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_runs_replay_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "replay", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Replay { .. }
            })
        ));

        let trusted =
            Cli::try_parse_from(["transist", "runs", "replay", "run-1", "--trusted-execution"])
                .expect("parse trusted replay");
        assert!(matches!(
            trusted.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Replay {
                    trusted_execution: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_runs_replay_with_run_ref_command() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "replay", "run-1", "--run-ref", "run-2"])
                .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Replay { .. }
            })
        ));
    }

    #[test]
    fn parse_runs_status_command() {
        let parsed = Cli::try_parse_from(["transist", "runs", "status", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Status { .. }
            })
        ));
    }

    #[test]
    fn parse_runs_sync_recipe_command() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "sync-recipe", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::SyncRecipe { run_id }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_inspection_configure_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "inspection",
            "configure",
            "--registry-url",
            "https://central.example.com",
            "--source-id",
            "src_example",
            "--workspace-id",
            "example/workflows",
            "--operator-label",
            "laptop-a",
            "--sync-profile",
            "default",
            "--token-stdin",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Inspection {
                command: InspectionSubcommand::Configure {
                    sync_profile,
                    token_stdin: true,
                    keyring_store: false,
                    ..
                }
            }) if sync_profile == "default"
        ));
    }

    #[test]
    fn parse_inspection_push_since_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "inspection",
            "push",
            "--since",
            "30d",
            "--dry-run",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Inspection {
                command: InspectionSubcommand::Push {
                    run_id: None,
                    since: Some(_),
                    dry_run: true,
                }
            })
        ));
    }

    #[test]
    fn parse_inspection_push_requires_run_id_or_since() {
        assert!(Cli::try_parse_from(["transist", "inspection", "push"]).is_err());
    }

    #[test]
    fn parse_inspection_daemon_backfill_and_force_flags() {
        let parsed = Cli::try_parse_from([
            "transist",
            "inspection",
            "daemon",
            "--backfill-window",
            "7d",
            "--force",
            "--resume",
        ])
        .expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Inspection {
                command: InspectionSubcommand::Daemon {
                    backfill_window: Some(_),
                    force: true,
                    resume: true,
                }
            })
        ));
    }

    #[test]
    fn parse_runs_preflight_command() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "preflight", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Preflight { .. }
            })
        ));
    }

    #[test]
    fn parse_runs_capabilities_command() {
        let parsed = Cli::try_parse_from([
            "transist",
            "runs",
            "capabilities",
            "run-1",
            "--format",
            "json",
        ])
        .expect("parse");
        assert_eq!(parsed.format, Some(CliOutputFormat::Json));
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Capabilities { run_id, exact: false }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_show_command_defaults_to_all_steps() {
        let parsed = Cli::try_parse_from(["transist", "runs", "show", "run-1"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Show {
                    run_id,
                    failed: false,
                    success: false,
                    all: false,
                    tui: false,
                }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_show_command_with_failed_filter() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--failed"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Show {
                    run_id,
                    failed: true,
                    success: false,
                    all: false,
                    tui: false,
                }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_show_command_with_success_filter() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--success"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Show {
                    run_id,
                    failed: false,
                    success: true,
                    all: false,
                    tui: false,
                }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_show_command_with_explicit_all_filter() {
        let parsed =
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--all"]).expect("parse");
        assert!(matches!(
            parsed.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Show {
                    run_id,
                    failed: false,
                    success: false,
                    all: true,
                    tui: false,
                }
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn parse_runs_show_command_rejects_conflicting_filters() {
        assert!(
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--failed", "--success"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--failed", "--all"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["transist", "runs", "show", "run-1", "--success", "--all"])
                .is_err()
        );
    }

    #[test]
    fn parse_gate_decision_commands() {
        let approve = Cli::try_parse_from([
            "transist", "runs", "gate", "approve", "run-1", "--actor", "alice", "--reason",
            "ship it",
        ])
        .expect("approve");
        assert!(matches!(
            approve.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Gate {
                    command: GateSubcommand::Approve { .. }
                }
            })
        ));

        let approve_with_gate = Cli::try_parse_from([
            "transist", "runs", "gate", "approve", "run-1", "review", "--actor", "alice",
            "--reason", "ship it",
        ])
        .expect("approve");
        assert!(matches!(
            approve_with_gate.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Gate {
                    command: GateSubcommand::Approve { .. }
                }
            })
        ));

        let reject = Cli::try_parse_from([
            "transist",
            "runs",
            "gate",
            "reject",
            "run-1",
            "--actor",
            "alice",
            "--reason",
            "needs changes",
        ])
        .expect("reject");
        assert!(matches!(
            reject.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Gate {
                    command: GateSubcommand::Reject { .. }
                }
            })
        ));

        let reject_with_gate = Cli::try_parse_from([
            "transist",
            "runs",
            "gate",
            "reject",
            "run-1",
            "review",
            "--actor",
            "alice",
            "--reason",
            "needs changes",
        ])
        .expect("reject");
        assert!(matches!(
            reject_with_gate.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Gate {
                    command: GateSubcommand::Reject { .. }
                }
            })
        ));

        let request_changes = Cli::try_parse_from([
            "transist",
            "runs",
            "gate",
            "request-changes",
            "run-1",
            "--actor",
            "alice",
            "--reason",
            "needs changes",
        ])
        .expect("request changes");
        assert!(matches!(
            request_changes.command,
            Some(Commands::Runs {
                command: RunsSubcommand::Gate {
                    command: GateSubcommand::RequestChanges { .. }
                }
            })
        ));
    }

    #[test]
    fn request_changes_help_explains_root_run_inference() {
        let mut command = Cli::command();
        let runs = command
            .find_subcommand_mut("runs")
            .expect("runs subcommand");
        let gate = runs.find_subcommand_mut("gate").expect("gate subcommand");
        let request_changes = gate
            .find_subcommand_mut("request-changes")
            .expect("request-changes subcommand");
        let mut buffer = Vec::new();
        request_changes
            .write_long_help(&mut buffer)
            .expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("Run id to target. The top-level/root run id is usually enough"));
        assert!(help.contains("Omit to decide the current blocked gate"));
    }

    #[test]
    fn root_help_lists_command_descriptions() {
        let mut buffer = Vec::new();
        command_for_surface(CliSurface::Core)
            .write_long_help(&mut buffer)
            .expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("View or change Transist home settings"));
        assert!(help.contains("List, inspect, and recover runs in the active Transist home"));
        assert!(help.contains("Install, inspect, and remove plugins"));
        assert!(help.contains("Check, install, run, and inspect workflows"));
        assert!(!help.contains("Configure and run outbound local inspection sync"));
    }

    #[test]
    fn core_surface_hides_inspection_command() {
        assert!(Cli::try_parse_from(["transist", "inspection"]).is_err());
        let mut buffer = Vec::new();
        command_for_surface(CliSurface::Core)
            .write_long_help(&mut buffer)
            .expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");
        assert!(!help.contains("inspection sync"));
    }

    #[test]
    fn workflow_validation_fast_path_check_help_explains_command_purpose() {
        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflows")
            .expect("workflow subcommand");
        let check = workflow
            .find_subcommand_mut("check")
            .expect("workflows check subcommand");
        let mut buffer = Vec::new();
        check.write_long_help(&mut buffer).expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains(
            "Validate authored or installed workflows; --live also probes installed plugins"
        ));
        assert!(help.contains("Select live probes and write canonical readiness evidence"));
        assert!(help.contains("Path to an authored workflow file to check"));
        assert!(help.contains("Installed workflow id and version"));
        assert!(help.contains("--input <INPUTS>"));
        assert!(help.contains("--inputs-file <INPUTS_FILE>"));
        assert!(help.contains("--trusted-execution"));
        assert!(help.contains("--local-only"));
        assert!(help.contains("--output <OUTPUT>"));
    }

    #[test]
    fn workflow_install_help_documents_confirmation_and_home_dependent_flags() {
        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflows")
            .expect("workflow subcommand");
        let install = workflow
            .find_subcommand_mut("install")
            .expect("workflows install subcommand");
        let mut buffer = Vec::new();
        install.write_long_help(&mut buffer).expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("Review and install a workflow into the active home"));
        for flag in [
            "--path <PATH>",
            "--input <INPUTS>",
            "--inputs-file <INPUTS_FILE>",
            "--trusted-execution",
            "--local-only",
            "--signature <SIGNATURE>",
            "--policy-output <POLICY_OUTPUT>",
            "--dry-run",
            "--approve",
        ] {
            assert!(help.contains(flag), "missing {flag}: {help}");
        }
        assert!(help.contains("governed homes"));
        assert!(!help.contains("--apply"));
        assert!(!help.contains("--prepare"));

        let mut workflow_help = Vec::new();
        workflow
            .write_long_help(&mut workflow_help)
            .expect("render workflow help");
        let workflow_help = String::from_utf8(workflow_help).expect("utf8 workflow help");
        assert!(!workflow_help.contains("\n  promote "));
        assert!(!workflow_help.contains("\n  compile "));
        assert!(!workflow_help.contains("\n  launch "));
        assert!(workflow_help.contains("\n  run "));
    }

    #[test]
    fn workflow_authorize_help_documents_both_input_forms() {
        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflows")
            .expect("workflow subcommand");
        let authorize = workflow
            .find_subcommand_mut("authorize")
            .expect("workflows authorize subcommand");
        let mut buffer = Vec::new();
        authorize.write_long_help(&mut buffer).expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("--input <INPUTS>"));
        assert!(help.contains("--inputs-file <INPUTS_FILE>"));
    }

    #[test]
    fn workflow_run_and_prepare_help_describe_dispatch_behavior() {
        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflows")
            .expect("workflow subcommand");
        for (subcommand, expected) in [
            (
                "prepare",
                "Create a workflow run without starting execution",
            ),
            ("run", "Create and start a workflow run"),
        ] {
            let leaf = workflow
                .find_subcommand_mut(subcommand)
                .expect("workflow leaf");
            let mut buffer = Vec::new();
            leaf.write_long_help(&mut buffer)
                .expect("render workflow leaf help");
            let help = String::from_utf8(buffer).expect("utf8 workflow leaf help");
            assert!(
                help.contains(expected),
                "missing help text for {subcommand}: {help}"
            );
            for flag in [
                "<WORKFLOW_REF>",
                "--run-ref <RUN_REF>",
                "--input <INPUTS>",
                "--inputs-file <INPUTS_FILE>",
                "--trusted-execution",
                "--local-only",
                "--readiness-file <READINESS_FILE>",
                "--max-age <MAX_AGE>",
            ] {
                assert!(
                    help.contains(flag),
                    "missing {flag} for {subcommand}: {help}"
                );
            }
        }
    }

    #[test]
    fn workflow_validation_fast_path_help_matches_command_contract() {
        let expected = [
            (
                "explain",
                "Explain authored declarations without validation, authorization, or execution consent",
            ),
            (
                "check",
                "Validate authored or installed workflows; --live also probes installed plugins",
            ),
            (
                "install",
                "Review and install a workflow into the active home",
            ),
            (
                "authorize",
                "Review and apply only the exact authority missing for a prospective run",
            ),
            (
                "prepare",
                "Create a workflow run without starting execution",
            ),
            ("run", "Create and start a workflow run"),
            ("list", "List workflows in the local registry"),
            (
                "inspect",
                "Show one workflow's execution recipe and summary",
            ),
            ("uninstall", "Remove an installed workflow"),
        ];
        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflows")
            .expect("workflows subcommand");
        let names = workflow
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            expected.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        );

        for (name, summary) in expected {
            let leaf = workflow.find_subcommand_mut(name).expect("workflow leaf");
            let mut buffer = Vec::new();
            leaf.write_long_help(&mut buffer)
                .expect("render workflow leaf help");
            let help = String::from_utf8(buffer).expect("utf8 workflow leaf help");
            assert!(help.contains(summary), "missing {name} summary: {help}");

            if matches!(name, "inspect" | "uninstall") {
                assert!(
                    help.contains("<WORKFLOW_REF>"),
                    "missing workflow ref: {help}"
                );
            }
            if name == "uninstall" {
                assert!(
                    !help.contains("--force"),
                    "obsolete flag documented: {help}"
                );
            }
        }
    }

    #[test]
    fn removed_workflow_commands_are_unknown() {
        for command in ["promote", "compile", "launch"] {
            assert!(
                Cli::try_parse_from(["transist", "workflows", command]).is_err(),
                "removed command parsed: {command}"
            );
        }
    }

    #[test]
    fn home_help_shows_updated_location_copy() {
        let mut command = Cli::command();
        let home = command
            .find_subcommand_mut("home")
            .expect("home subcommand");

        let mut home_buffer = Vec::new();
        home.write_long_help(&mut home_buffer)
            .expect("render home help");
        let home_help = String::from_utf8(home_buffer).expect("utf8 home help");
        assert!(home_help.contains("View or change Transist home settings"));
        assert!(home_help.contains("Initialize the per-user default Transist home directory"));
        assert!(home_help.contains("Persist a user-level default Transist home path"));
        assert!(home_help.contains("Remove the persisted user-level default Transist home path"));
    }

    #[test]
    fn nested_subcommand_help_shows_descriptions() {
        let mut command = Cli::command();
        let runs = command
            .find_subcommand_mut("runs")
            .expect("runs subcommand");

        let mut runs_buffer = Vec::new();
        runs.write_long_help(&mut runs_buffer)
            .expect("render runs help");
        let runs_help = String::from_utf8(runs_buffer).expect("utf8 runs help");
        assert!(runs_help.contains("List, inspect, and recover runs in the active Transist home"));
        assert!(runs_help.contains("List recent runs with one-line status summaries"));

        let list = runs
            .find_subcommand_mut("list")
            .expect("runs list subcommand");
        let mut list_buffer = Vec::new();
        list.write_long_help(&mut list_buffer)
            .expect("render list help");
        let list_help = String::from_utf8(list_buffer).expect("utf8 list help");
        assert!(list_help.contains("List recent runs with one-line status summaries"));
        assert!(list_help.contains("--all"));
        assert!(list_help.contains("--ids-only"));
    }

    #[test]
    fn run_recovery_help_shows_updated_copy() {
        for (subcommand, expected) in [
            ("watch", "Open the fullscreen TUI to watch a run"),
            (
                "cancel",
                "Interrupt an active run by requesting cancellation",
            ),
            ("resume", "Resume a failed or canceled run"),
            (
                "retry",
                "Retry the failed step(s) of a failed or canceled run (alias for `resume`)",
            ),
            (
                "cleanup",
                "Run cleanup steps defined in the workflow for a finished run",
            ),
            (
                "replay",
                "Start a fresh run from a previous run's saved recipe and inputs",
            ),
        ] {
            let mut command = Cli::command();
            let runs = command
                .find_subcommand_mut("runs")
                .expect("runs subcommand");
            let leaf = runs.find_subcommand_mut(subcommand).expect("runs leaf");
            let mut buffer = Vec::new();
            leaf.write_long_help(&mut buffer)
                .expect("render runs leaf help");
            let help = String::from_utf8(buffer).expect("utf8 runs leaf help");
            assert!(
                help.contains(expected),
                "missing help text for {subcommand}: {help}"
            );
        }
    }

    #[test]
    fn run_recovery_help_shows_detached_runner_copy() {
        for (subcommand, expected) in [
            ("watch", "Open the fullscreen TUI to watch a run"),
            ("resume", "Resume a failed or canceled run"),
            (
                "retry",
                "Retry the failed step(s) of a failed or canceled run (alias for `resume`)",
            ),
            (
                "cleanup",
                "Run cleanup steps defined in the workflow for a finished run",
            ),
            (
                "replay",
                "Start a fresh run from a previous run's saved recipe and inputs",
            ),
        ] {
            let mut command = Cli::command();
            let runs = command
                .find_subcommand_mut("runs")
                .expect("runs subcommand");
            let leaf = runs.find_subcommand_mut(subcommand).expect("runs leaf");
            let mut buffer = Vec::new();
            leaf.write_long_help(&mut buffer)
                .expect("render runs leaf help");
            let help = String::from_utf8(buffer).expect("utf8 runs leaf help");
            assert!(
                help.contains(expected),
                "missing help text for {subcommand}: {help}"
            );
        }
    }

    #[test]
    fn parse_bare_command_without_subcommand() {
        let parsed = Cli::try_parse_from(["transist"]).expect("parse");
        assert!(parsed.command.is_none());
    }

    #[cfg(feature = "managed-mode")]
    #[test]
    fn core_surface_rejects_managed_metadata_flags() {
        let error = command_for_surface(CliSurface::Core)
            .try_get_matches_from(["transist", "--management-mode", "managed", "runs", "list"])
            .expect_err("managed metadata flags should be absent from core surface");
        let rendered = error.to_string();
        assert!(
            rendered.contains("--management-mode"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn core_surface_rejects_inspection_subcommand() {
        let error = command_for_surface(CliSurface::Core)
            .try_get_matches_from(["transist", "inspection", "status"])
            .expect_err("inspection should be absent from core surface");
        let rendered = error.to_string();
        assert!(
            rendered.contains("inspection"),
            "unexpected error: {rendered}"
        );
    }

    #[cfg(feature = "managed-mode")]
    #[test]
    fn full_surface_still_accepts_managed_parse_shape() {
        let matches = command_for_surface(CliSurface::Full)
            .try_get_matches_from([
                "transist",
                "--management-mode",
                "managed",
                "inspection",
                "status",
            ])
            .expect("full surface parse");
        let parsed =
            <Cli as clap::FromArgMatches>::from_arg_matches(&matches).expect("typed parse");

        assert_eq!(parsed.management_mode.as_deref(), Some("managed"));
        assert!(matches!(
            parsed.command,
            Some(Commands::Inspection {
                command: InspectionSubcommand::Status
            })
        ));
    }

    #[cfg(feature = "managed-mode")]
    #[test]
    fn core_surface_help_is_clean() {
        let mut buffer = Vec::new();
        command_for_surface(CliSurface::Core)
            .write_long_help(&mut buffer)
            .expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(!help.contains("\n  inspection "), "unexpected help: {help}");
        assert!(
            !help.contains("--management-mode"),
            "unexpected help: {help}"
        );
    }

    #[test]
    fn recursive_command_inventory_matches_committed_fixture() {
        let actual = command_inventory_json(CliSurface::Core);
        let fixture_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/command-inventory.json");
        if std::env::var_os("UPDATE_INVENTORY").is_some() {
            std::fs::write(&fixture_path, actual).expect("write command inventory fixture");
            return;
        }
        let committed = COMMAND_INVENTORY_JSON.trim();
        let formatted = format!("{}\n", actual.trim());
        assert_eq!(
            committed,
            formatted.trim(),
            "CLI command/option tree drifted from the committed inventory; regenerate it with \
             UPDATE_INVENTORY=1 cargo test -p transist-cli recursive_command_inventory_matches_committed_fixture"
        );
    }

    /// Parses only complete, anchored command headings from `cli.md`.
    fn cli_md_headings(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| {
                let body = line.strip_prefix("### `")?.strip_suffix('`')?;
                (!body.contains('`')).then(|| body.to_string())
            })
            .collect()
    }

    /// Splits a heading body into its command path and `--flag` tokens,
    /// dropping option groups (`[...]`), positional placeholders (`<...>`),
    /// and alternations.
    fn heading_path_and_flags(content: &str) -> (Vec<String>, Vec<String>) {
        let mut path = Vec::new();
        let mut flags = Vec::new();
        for token in content.split_whitespace() {
            let token = token.trim_matches(['[', ']', '(', ')', ',', ':', ';', '.']);
            if token.starts_with("--") {
                flags.push(token.to_string());
            } else if token.starts_with('<') || token.contains('|') {
                continue;
            } else {
                path.push(token.to_string());
            }
        }
        (path, flags)
    }

    fn inventory_entries(inventory: &serde_json::Value) -> Vec<serde_json::Value> {
        inventory.as_array().expect("inventory array").to_vec()
    }

    fn entry_path(entry: &serde_json::Value) -> Vec<String> {
        entry["path"]
            .as_array()
            .expect("path array")
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect()
    }

    fn entry_options(entry: &serde_json::Value) -> std::collections::BTreeSet<String> {
        entry["options"]
            .as_array()
            .expect("options array")
            .iter()
            .filter_map(|v| v["name"].as_str())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn cli_docs_reference_matches_recursive_command_inventory() {
        let inventory: serde_json::Value =
            serde_json::from_str(COMMAND_INVENTORY_JSON).expect("inventory fixture json");
        let entries = inventory_entries(&inventory);
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli_md_path = repo_root.join("docs/public/reference/cli.md");
        let cli_md = std::fs::read_to_string(&cli_md_path)
            .expect("failed reading docs/public/reference/cli.md");

        let headings = cli_md_headings(&cli_md);
        assert!(
            !headings.is_empty(),
            "cli.md must document command headings"
        );

        // Direction A: every documented `### \`cmd ...\`` heading names a real
        // command path and only real `--flags` of that command.
        for heading in &headings {
            let (path, flags) = heading_path_and_flags(heading);
            assert!(
                !path.is_empty(),
                "empty command path in heading '{heading}'"
            );
            let node = entries.iter().find(|entry| entry_path(entry) == path).unwrap_or_else(|| {
                panic!(
                    "cli.md documents command '{heading}' that is not present in the CLI command tree"
                )
            });
            let mut options = entry_options(node);
            // The inventory stores global flags at their defining ancestor,
            // while Clap accepts them on every descendant command too.
            for ancestor in entries
                .iter()
                .filter(|entry| path.starts_with(&entry_path(entry)))
            {
                for option in ancestor["options"].as_array().expect("options array") {
                    if option["global"] == true {
                        options.insert(option["name"].as_str().expect("option name").to_string());
                    }
                }
            }
            for flag in &flags {
                assert!(
                    options.contains(flag),
                    "cli.md documents option '{flag}' on command '{heading}' that is not an option \
                     of '{}' in the CLI command tree",
                    path.join(" ")
                );
            }
        }

        // Direction B: every invocable leaf command path is documented in cli.md.
        for entry in &entries {
            let path = entry_path(entry);
            if path.is_empty() {
                continue;
            }
            let is_leaf = !entries.iter().any(|other| {
                let other_path = entry_path(other);
                other_path.len() == path.len() + 1 && other_path.starts_with(&path)
            });
            if !is_leaf {
                continue;
            }
            let documented_paths: std::collections::BTreeSet<Vec<String>> = headings
                .iter()
                .map(|heading| heading_path_and_flags(heading).0)
                .collect();
            assert!(
                documented_paths.contains(&path),
                "CLI command '{}' is not documented in docs/public/reference/cli.md",
                path.join(" ")
            );
        }
    }
}
