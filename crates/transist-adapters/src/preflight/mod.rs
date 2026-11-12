// SPDX-License-Identifier: MPL-2.0
//! Shared preflight validation and grouping for adapter probes.

// SAFETY: OS-integration: temporary-root ownership uses libc advisory locks and uid checks.
#![allow(unsafe_code)]

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::{Builder, TempDir};
use transist_core::ports::PreflightInvocation;
use transist_core::{PreflightCheck, PreflightStatus};
use transist_plugins::{ActionDescriptor, AdapterKind, PluginEntrypoint, PluginManifest};
use transist_prerequisites::{
    EvaluationContext, PrerequisiteScope, PrerequisiteStatus as HostPrerequisiteStatus,
    evaluate_prerequisites,
};
use transist_protocol::ExecutionRequest;

use crate::paths::RUNTIME_TEMP_DIR_METADATA_KEY;
use crate::paths::home_dir_from_run_root;
use crate::plugin_loader::load_plugin_from_request;
use crate::{
    SandboxPlan, build_sandbox_plan, filesystem_policy_from_request, network_policy_from_request,
    project_action_input, sandbox_context_from_request, strict_filesystem_policy_requested,
    strict_local_policy_requested, strict_local_sandbox_backend_label,
};

const PREFLIGHT_PROBE_CONCURRENCY_ENV: &str = "TRANSIST_PREFLIGHT_PROBE_CONCURRENCY";
const MAX_PREFLIGHT_PROBE_CONCURRENCY: usize = 32;
const RUNTIME_TEMP_PREFIX: &str = "transist-runtime-temp-";
const RUNTIME_TEMP_OWNER_SUFFIX: &str = ".owner.lock";
const RUNTIME_TEMP_OWNER_INIT_PREFIX: &str = ".transist-runtime-owner-init-";

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

pub(crate) mod http;
pub(crate) mod mcp;
pub(crate) mod process;

pub(crate) use http::run_http_preflight_probe;
pub(crate) use mcp::run_mcp_preflight_probe;
pub(crate) use process::run_process_preflight_probe;

pub(crate) fn validate_preflight_probe_concurrency(concurrency: usize) -> Result<()> {
    if !(1..=MAX_PREFLIGHT_PROBE_CONCURRENCY).contains(&concurrency) {
        bail!(
            "preflight probe concurrency must be in 1..={MAX_PREFLIGHT_PROBE_CONCURRENCY}, got {concurrency}"
        );
    }
    Ok(())
}

fn preflight_probe_concurrency_from_value(value: Option<&OsStr>) -> Result<usize> {
    let Some(value) = value else {
        return Ok(crate::DEFAULT_PREFLIGHT_PROBE_CONCURRENCY);
    };
    let value = value.to_str().ok_or_else(|| {
        anyhow!("{PREFLIGHT_PROBE_CONCURRENCY_ENV} must be a UTF-8 integer in 1..={MAX_PREFLIGHT_PROBE_CONCURRENCY}")
    })?;
    let concurrency = value.parse::<usize>().with_context(|| {
        format!(
            "{PREFLIGHT_PROBE_CONCURRENCY_ENV} must be an integer in 1..={MAX_PREFLIGHT_PROBE_CONCURRENCY}, got '{value}'"
        )
    })?;
    validate_preflight_probe_concurrency(concurrency)
        .with_context(|| format!("invalid {PREFLIGHT_PROBE_CONCURRENCY_ENV} value '{value}'"))?;
    Ok(concurrency)
}

pub(crate) fn preflight_probe_concurrency_from_env() -> Result<usize> {
    preflight_probe_concurrency_from_value(
        std::env::var_os(PREFLIGHT_PROBE_CONCURRENCY_ENV).as_deref(),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedPreflightTarget {
    pub(crate) request: ExecutionRequest,
    pub(crate) available_secret_refs: Vec<String>,
    pub(crate) manifest: PluginManifest,
    pub(crate) plugin_dir: PathBuf,
    pub(crate) action: ActionDescriptor,
    pub(crate) input_schema_path: PathBuf,
    pub(crate) tool_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreflightProbeGroup {
    pub(crate) target: ValidatedPreflightTarget,
    pub(crate) step_ids: Vec<String>,
}

pub(crate) fn insert_preflight_probe_target(
    groups: &mut BTreeMap<String, PreflightProbeGroup>,
    target: ValidatedPreflightTarget,
) -> Result<()> {
    let key = preflight_probe_key(&target)?;
    let step_id = target.request.step.id.clone();
    groups
        .entry(key)
        .and_modify(|group| {
            if !group.step_ids.contains(&step_id) {
                group.step_ids.push(step_id.clone());
            }
        })
        .or_insert_with(|| PreflightProbeGroup {
            step_ids: vec![step_id],
            target,
        });
    Ok(())
}

/// Owns private paths used while probing an adapter before dispatch.
pub(crate) struct PrivatePreflightRequest {
    pub(crate) request: ExecutionRequest,
    _root: TempDir,
    _runtime_temp_root: RuntimeTempRoot,
}

/// Owns a disposable preflight temporary root and a sibling crash-recovery
/// lock that is outside the plugin-visible sandbox.
struct RuntimeTempRoot {
    root: PathBuf,
    owner_path: PathBuf,
    _owner_lock: File,
}

impl RuntimeTempRoot {
    fn path(&self) -> &std::path::Path {
        &self.root
    }
}

impl Drop for RuntimeTempRoot {
    fn drop(&mut self) {
        if fs::remove_dir_all(&self.root).is_ok() {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

pub(crate) struct PreparedLocalPreflightProbe {
    pub(crate) group: PreflightProbeGroup,
    pub(crate) private: PrivatePreflightRequest,
    pub(crate) sandbox_plan: SandboxPlan,
}

pub(crate) struct PreflightChildGuard {
    child: Arc<Mutex<Child>>,
}

impl PreflightChildGuard {
    pub(crate) fn new(child: Arc<Mutex<Child>>) -> Self {
        Self { child }
    }
}

impl Drop for PreflightChildGuard {
    fn drop(&mut self) {
        let _ = crate::terminate_child(
            &self.child,
            Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS),
        );
    }
}

enum PreparedLocalPreflightOutcome {
    Ready(Box<PreparedLocalPreflightProbe>),
    Check(PreflightCheck),
}

fn prepare_local_preflight_probe(
    group: PreflightProbeGroup,
) -> Result<PreparedLocalPreflightOutcome> {
    let private = private_preflight_request(&group.target.request)?;
    let request = &private.request;
    let filesystem_policy = match filesystem_policy_from_request(request) {
        Ok(policy) => policy.unwrap_or_default(),
        Err(error) => {
            return Ok(PreparedLocalPreflightOutcome::Check(preflight_group_check(
                &group,
                "adapter_handshake",
                PreflightStatus::Failed,
                error,
            )));
        }
    };
    let network_policy = match network_policy_from_request(request) {
        Ok(policy) => policy.unwrap_or_default(),
        Err(error) => {
            return Ok(PreparedLocalPreflightOutcome::Check(preflight_group_check(
                &group,
                "adapter_handshake",
                PreflightStatus::Failed,
                error,
            )));
        }
    };
    let sandbox_context = sandbox_context_from_request(request, &group.target.plugin_dir);
    let sandbox_plan =
        match build_sandbox_plan(&filesystem_policy, &network_policy, &sandbox_context) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(PreparedLocalPreflightOutcome::Check(preflight_group_check(
                    &group,
                    "adapter_handshake",
                    PreflightStatus::Failed,
                    error,
                )));
            }
        };
    Ok(PreparedLocalPreflightOutcome::Ready(Box::new(
        PreparedLocalPreflightProbe {
            group,
            private,
            sandbox_plan,
        },
    )))
}

pub(crate) fn run_local_preflight_probe_groups(
    groups: Vec<PreflightProbeGroup>,
    concurrency: usize,
    adapter_kind: AdapterKind,
) -> Result<Vec<PreflightCheck>> {
    validate_preflight_probe_concurrency(concurrency)?;
    let group_count = groups.len();
    let mut results = vec![None; group_count];
    let mut eligible = Vec::new();

    for (slot, group) in groups.into_iter().enumerate() {
        let prepared = match prepare_local_preflight_probe(group) {
            Ok(prepared) => prepared,
            Err(error) => {
                run_prepared_probe_batch(&mut eligible, concurrency, adapter_kind, &mut results)?;
                return Err(error.context(format!(
                    "failed preparing preflight probe in stable group slot {slot}"
                )));
            }
        };
        match prepared {
            PreparedLocalPreflightOutcome::Check(check) => {
                run_prepared_probe_batch(&mut eligible, concurrency, adapter_kind, &mut results)?;
                results[slot] = Some(check);
            }
            PreparedLocalPreflightOutcome::Ready(probe)
                if probe.sandbox_plan.preflight_concurrency_eligible() =>
            {
                eligible.push((slot, *probe));
            }
            PreparedLocalPreflightOutcome::Ready(probe) => {
                run_prepared_probe_batch(&mut eligible, concurrency, adapter_kind, &mut results)?;
                results[slot] = Some(run_prepared_local_probe(*probe, adapter_kind)?);
            }
        }
    }
    run_prepared_probe_batch(&mut eligible, concurrency, adapter_kind, &mut results)?;

    results
        .into_iter()
        .enumerate()
        .map(|(slot, check)| {
            check.ok_or_else(|| anyhow!("preflight scheduler left stable result slot {slot} empty"))
        })
        .collect()
}

fn run_prepared_local_probe(
    probe: PreparedLocalPreflightProbe,
    adapter_kind: AdapterKind,
) -> Result<PreflightCheck> {
    match adapter_kind {
        AdapterKind::Process => run_process_preflight_probe(probe),
        AdapterKind::Mcp => run_mcp_preflight_probe(probe),
        _ => bail!("bounded local preflight scheduler does not support adapter '{adapter_kind}'"),
    }
}

fn run_prepared_probe_batch(
    batch: &mut Vec<(usize, PreparedLocalPreflightProbe)>,
    concurrency: usize,
    adapter_kind: AdapterKind,
    results: &mut [Option<PreflightCheck>],
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let jobs = std::mem::take(batch);
    let completed = run_bounded_ordered_jobs(jobs, concurrency, |probe| {
        run_prepared_local_probe(probe, adapter_kind)
    })?;
    for (slot, check) in completed {
        results[slot] = Some(check);
    }
    Ok(())
}

fn run_bounded_ordered_jobs<J, R, F>(
    jobs: Vec<(usize, J)>,
    concurrency: usize,
    worker: F,
) -> Result<Vec<(usize, R)>>
where
    J: Send,
    R: Send,
    F: Fn(J) -> Result<R> + Sync,
{
    validate_preflight_probe_concurrency(concurrency)?;
    let measurement_context = transist_core::current_preflight_measurement_context();
    let mut jobs = VecDeque::from(jobs);
    let mut running = 0;
    let mut first_error: Option<(usize, anyhow::Error)> = None;
    let mut completed = BTreeMap::new();
    let (sender, receiver) = mpsc::channel();

    thread::scope(|scope| -> Result<()> {
        while running < concurrency {
            let Some(job) = jobs.pop_front() else {
                break;
            };
            spawn_preflight_worker(scope, &sender, job, &worker, measurement_context.clone());
            running += 1;
        }
        while running > 0 {
            let (slot, result) = receiver
                .recv()
                .context("preflight probe worker channel closed before all workers completed")?;
            running -= 1;
            match result {
                Ok(value) => {
                    completed.insert(slot, value);
                }
                Err(error) => {
                    if first_error
                        .as_ref()
                        .is_none_or(|(first_slot, _)| slot < *first_slot)
                    {
                        first_error = Some((slot, error));
                    }
                }
            }
            if first_error.is_none()
                && let Some(job) = jobs.pop_front()
            {
                spawn_preflight_worker(scope, &sender, job, &worker, measurement_context.clone());
                running += 1;
            }
        }
        Ok(())
    })?;

    if let Some((slot, error)) = first_error {
        return Err(error.context(format!(
            "preflight probe failed in stable group slot {slot}"
        )));
    }
    Ok(completed.into_iter().collect())
}

fn spawn_preflight_worker<'scope, J, R, F>(
    scope: &'scope thread::Scope<'scope, '_>,
    sender: &mpsc::Sender<(usize, Result<R>)>,
    job: (usize, J),
    worker: &'scope F,
    measurement_context: Option<transist_core::PreflightMeasurementContext>,
) where
    J: Send + 'scope,
    R: Send + 'scope,
    F: Fn(J) -> Result<R> + Sync,
{
    let sender = sender.clone();
    let (slot, job) = job;
    scope.spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            transist_core::with_preflight_measurement_context(measurement_context, || worker(job))
        }))
        .unwrap_or_else(|_| Err(anyhow!("preflight probe worker panicked")));
        let _ = sender.send((slot, result));
    });
}

/// Rehomes preflight-visible runtime paths outside the durable run tree.
pub(crate) fn private_preflight_request(
    request: &ExecutionRequest,
) -> Result<PrivatePreflightRequest> {
    let home_dir = home_dir_from_run_root(std::path::Path::new(&request.paths.run_root))?;
    transist_core::validate_runtime_temp_parent(&runtime_temp_parent(), &home_dir, request)?;
    let staging_parent = home_dir.join(".transist-staging");
    fs::create_dir_all(&staging_parent)?;
    set_private_mode(&staging_parent)?;
    let root = Builder::new()
        .prefix("invoke-")
        .tempdir_in(&staging_parent)?;
    set_private_mode(root.path())?;
    cleanup_stale_runtime_temp_roots()?;
    let runtime_temp_root = create_runtime_temp_root()?;
    let artifacts_dir = root.path().join("artifacts");
    let evidence_dir = root.path().join("evidence");
    fs::create_dir(&artifacts_dir)?;
    fs::create_dir(&evidence_dir)?;
    set_private_mode(&artifacts_dir)?;
    set_private_mode(&evidence_dir)?;
    let mut request = request.clone();
    request.paths.run_root = root.path().display().to_string();
    request.paths.artifacts_dir = artifacts_dir.display().to_string();
    request.paths.evidence_dir = evidence_dir.display().to_string();
    if !request.metadata.is_object() {
        request.metadata = serde_json::json!({});
    }
    let metadata = request
        .metadata
        .as_object_mut()
        .expect("metadata was normalized to an object");
    metadata.insert(
        "__transist_staging_dir".to_string(),
        serde_json::Value::String(root.path().display().to_string()),
    );
    metadata.insert(
        RUNTIME_TEMP_DIR_METADATA_KEY.to_string(),
        serde_json::Value::String(runtime_temp_root.path().display().to_string()),
    );
    Ok(PrivatePreflightRequest {
        request,
        _root: root,
        _runtime_temp_root: runtime_temp_root,
    })
}

fn runtime_temp_parent() -> PathBuf {
    transist_core::runtime_temp_parent()
}

fn cleanup_stale_runtime_temp_roots() -> Result<()> {
    let parent = runtime_temp_parent();
    cleanup_stale_runtime_temp_roots_in(&parent)
}

fn cleanup_stale_runtime_temp_roots_in(parent: &std::path::Path) -> Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir()
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(RUNTIME_TEMP_PREFIX))
        {
            continue;
        }
        let owner_path = runtime_temp_owner_path(&path)?;
        let Some(owner_lock) = open_existing_runtime_temp_owner(Some(&path), &owner_path)? else {
            continue;
        };
        if try_lock_runtime_temp_owner(&owner_lock)? {
            remove_stale_runtime_temp_entries(Some(&path), &owner_path)?;
        }
    }
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let owner_path = entry.path();
        let Some(owner_name) = owner_path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !owner_name.starts_with(&format!(".{RUNTIME_TEMP_PREFIX}"))
            || !owner_name.ends_with(RUNTIME_TEMP_OWNER_SUFFIX)
        {
            continue;
        }
        let root = runtime_temp_root_from_owner_path(&owner_path)?;
        // Only a definitely absent root makes an owner record orphaned.
        if !matches!(fs::symlink_metadata(&root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let Some(owner_lock) = open_existing_runtime_temp_owner(None, &owner_path)? else {
            continue;
        };
        if try_lock_runtime_temp_owner(&owner_lock)? {
            remove_stale_runtime_temp_entries(None, &owner_path)?;
        }
    }
    Ok(())
}

fn remove_stale_runtime_temp_entries(
    root: Option<&std::path::Path>,
    owner: &std::path::Path,
) -> Result<()> {
    // An owner can unlink its root, or another sweep can win after we opened
    // the same stale inode. Disappearance is idempotent; every other I/O error
    // remains visible. Callers must already hold the verified owner lock.
    if let Some(root) = root {
        stale_entry_removed(fs::remove_dir_all(root))?;
    }
    stale_entry_removed(fs::remove_file(owner))?;
    Ok(())
}

fn stale_entry_removed(result: std::io::Result<()>) -> std::io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn create_runtime_temp_root() -> Result<RuntimeTempRoot> {
    let parent = runtime_temp_parent();
    let mut unpublished_owner = Builder::new()
        .prefix(RUNTIME_TEMP_OWNER_INIT_PREFIX)
        .tempfile_in(&parent)?;
    set_private_file_mode(unpublished_owner.path())?;
    lock_runtime_temp_owner(unpublished_owner.as_file())?;
    unpublished_owner.write_all(std::process::id().to_string().as_bytes())?;
    unpublished_owner.as_file().sync_all()?;
    let token = unpublished_owner
        .path()
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_prefix(RUNTIME_TEMP_OWNER_INIT_PREFIX))
        .context("unpublished runtime temporary owner lock has an invalid name")?;
    let root = parent.join(format!("{RUNTIME_TEMP_PREFIX}{token}"));
    let owner_path = runtime_temp_owner_path(&root)?;
    let owner_lock = unpublished_owner.persist_noclobber(&owner_path)?;
    if let Err(error) = fs::create_dir(&root) {
        drop(owner_lock);
        let _ = fs::remove_file(&owner_path);
        return Err(error.into());
    }
    let runtime_temp_root = RuntimeTempRoot {
        root,
        owner_path,
        _owner_lock: owner_lock,
    };
    set_private_mode(runtime_temp_root.path())?;
    Ok(runtime_temp_root)
}

fn runtime_temp_root_from_owner_path(owner_path: &std::path::Path) -> Result<PathBuf> {
    let parent = owner_path
        .parent()
        .context("runtime temporary owner lock has no parent")?;
    let owner_name = owner_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("runtime temporary owner lock has no UTF-8 name")?;
    let root_name = owner_name
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix(RUNTIME_TEMP_OWNER_SUFFIX))
        .filter(|value| value.starts_with(RUNTIME_TEMP_PREFIX))
        .context("runtime temporary owner lock has an invalid name")?;
    Ok(parent.join(root_name))
}

fn runtime_temp_owner_path(root: &std::path::Path) -> Result<PathBuf> {
    let parent = root
        .parent()
        .context("runtime temporary root has no parent")?;
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .context("runtime temporary root has no UTF-8 name")?;
    Ok(parent.join(format!(".{name}{RUNTIME_TEMP_OWNER_SUFFIX}")))
}

fn open_existing_runtime_temp_owner(
    root: Option<&std::path::Path>,
    owner_path: &std::path::Path,
) -> Result<Option<File>> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid reads the effective UID of the current process without dereferencing pointers.
        let current_uid = unsafe { libc::geteuid() };
        open_existing_runtime_temp_owner_checked(root, owner_path, current_uid)
    }
    #[cfg(not(unix))]
    {
        open_existing_runtime_temp_owner_checked(root, owner_path)
    }
}

fn open_existing_runtime_temp_owner_checked(
    root: Option<&std::path::Path>,
    owner_path: &std::path::Path,
    #[cfg(unix)] current_uid: u32,
) -> Result<Option<File>> {
    let root_metadata = match root.map(fs::symlink_metadata).transpose() {
        Ok(metadata) => metadata,
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if root_metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.file_type().is_dir())
    {
        return Ok(None);
    }
    // Shared temporary parents contain other users' records. Inspect without
    // following links before attempting a read/write open of any candidate.
    let owner_path_metadata = match fs::symlink_metadata(owner_path) {
        Ok(metadata) => metadata,
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !owner_path_metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(unix)]
    if root_metadata
        .as_ref()
        .is_some_and(|metadata| !runtime_temp_metadata_is_private(metadata, current_uid))
        || !runtime_temp_metadata_is_private(&owner_path_metadata, current_uid)
        || owner_path_metadata.permissions().mode() & 0o600 != 0o600
    {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let owner_lock = match options.open(owner_path) {
        Ok(file) => file,
        // A replacement can become inaccessible, a symlink, or a nonregular
        // object after the path check. Retain it rather than fail other work.
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed opening runtime temporary owner lock: {}",
                    owner_path.display()
                )
            });
        }
    };
    let owner_metadata = owner_lock.metadata()?;
    if !owner_metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        // The descriptor must still identify the private regular file inspected
        // above. O_NONBLOCK also prevents a replacement FIFO from stalling open.
        if !runtime_temp_metadata_is_private(&owner_metadata, current_uid)
            || owner_metadata.permissions().mode() & 0o600 != 0o600
            || owner_metadata.dev() != owner_path_metadata.dev()
            || owner_metadata.ino() != owner_path_metadata.ino()
        {
            return Ok(None);
        }
    }
    Ok(Some(owner_lock))
}

#[cfg(unix)]
fn runtime_temp_metadata_is_private(metadata: &fs::Metadata, current_uid: u32) -> bool {
    metadata.uid() == current_uid && metadata.permissions().mode() & 0o077 == 0
}

fn runtime_temp_entry_unavailable(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::IsADirectory
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENXIO | libc::ENODEV)
    ) {
        return true;
    }
    false
}

fn lock_runtime_temp_owner(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: file owns a valid open descriptor for the duration of the flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed locking runtime temporary owner file");
        }
    }
    let _ = file;
    Ok(())
}

#[cfg(unix)]
fn try_lock_runtime_temp_owner(file: &File) -> Result<bool> {
    // SAFETY: file owns a valid open descriptor for the duration of the flock call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false);
    }
    Err(error).context("failed probing runtime temporary owner lock")
}

#[cfg(not(unix))]
fn try_lock_runtime_temp_owner(file: &File) -> Result<bool> {
    let _ = file;
    Ok(false)
}

fn set_private_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let _ = path;
    Ok(())
}

pub(crate) fn preflight_check(
    kind: impl Into<String>,
    status: PreflightStatus,
    step_ids: Vec<String>,
    adapter_kind: Option<String>,
    plugin_id: Option<String>,
    action: Option<String>,
    message: impl Into<String>,
) -> PreflightCheck {
    PreflightCheck {
        kind: kind.into(),
        status,
        step_ids,
        adapter_kind,
        plugin_id,
        action,
        message: message.into(),
    }
}

fn validate_preflight_target_with_catalog(
    invocation: &PreflightInvocation,
    expected_adapter: AdapterKind,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
    deferred_paths: &[String],
) -> std::result::Result<ValidatedPreflightTarget, Box<PreflightCheck>> {
    let request = &invocation.request;
    let package = catalog
        .map(|catalog| -> Result<_> {
            let id = request.metadata["plugin_id"]
                .as_str()
                .ok_or_else(|| anyhow!("missing plugin id"))?;
            let version = request.metadata["plugin_version"]
                .as_str()
                .ok_or_else(|| anyhow!("missing plugin version"))?;
            catalog.load(id, version)
        })
        .transpose()
        .map_err(|error| {
            Box::new(preflight_check_for_request(
                request,
                "plugin_contract",
                PreflightStatus::Failed,
                error.to_string(),
            ))
        })?;
    let loaded = if let Some(package) = &package {
        (|| -> Result<_> {
            if package.manifest.adapter != expected_adapter {
                bail!("plugin adapter differs from request");
            }
            let action = package
                .manifest
                .actions
                .iter()
                .find(|a| a.name == request.action)
                .cloned()
                .ok_or_else(|| anyhow!("plugin action not found"))?;
            Ok((package.manifest.clone(), package.directory.clone(), action))
        })()
    } else {
        load_plugin_from_request(request, expected_adapter)
    };
    let (manifest, plugin_dir, action) = loaded.map_err(|error| {
        Box::new(preflight_check_for_request(
            request,
            "plugin_contract",
            PreflightStatus::Failed,
            error.to_string(),
        ))
    })?;
    let tool_name = if expected_adapter == AdapterKind::Mcp {
        Some(mcp_tool_name(&action).map_err(|error| {
            Box::new(preflight_check_for_request(
                request,
                "plugin_contract",
                PreflightStatus::Failed,
                error.to_string(),
            ))
        })?)
    } else {
        None
    };
    let input_schema_path = plugin_dir.join(&action.input_schema);
    let action_input = project_action_input(&action, &request.input).map_err(|error| {
        Box::new(preflight_check_for_request(
            request,
            "plugin_contract",
            PreflightStatus::Failed,
            error.to_string(),
        ))
    })?;
    // Logical document objects are unknown string paths, never fabricated schema
    // instances. Prove known type incompatibilities while deferring path constraints.
    let document_types = (|| -> Result<()> {
        for location in transist_protocol::collect_document_bindings(&action_input)
            .map_err(anyhow::Error::msg)?
        {
            if expected_adapter != AdapterKind::Process {
                bail!("$document bindings are unsupported on HTTP/MCP actions");
            }
            let path = location
                .path
                .split('/')
                .skip(1)
                .map(|p| p.replace("~1", "/").replace("~0", "~"))
                .collect::<Vec<_>>();
            let types =
                transist_plugins::schema_types_at_path(&plugin_dir, &action.input_schema, &path)?;
            if !types.is_empty() && !types.contains("string") {
                bail!(
                    "statically incompatible $document string path at '{}'",
                    location.path
                );
            }
            if !deferred_paths.contains(&location.path) {
                bail!("$document path has not been materialized");
            }
        }
        Ok(())
    })();
    document_types.map_err(|error| {
        Box::new(preflight_check_for_request(
            request,
            "input_schema",
            PreflightStatus::Failed,
            error.to_string(),
        ))
    })?;
    let input_result = if !deferred_paths.is_empty() {
        if let (Some(catalog), Some(package)) = (catalog, package.as_ref()) {
            catalog.validate_input_partial(package, &action, &action_input, deferred_paths)
        } else {
            transist_plugins::validate_action_input_partial(
                &plugin_dir,
                &action,
                &action_input,
                deferred_paths,
            )
        }
    } else if let (Some(catalog), Some(package)) = (catalog, package.as_ref()) {
        catalog.validate_input(package, &action, &action_input)
    } else {
        transist_plugins::validate_action_input(&plugin_dir, &action, &action_input)
    };
    input_result.map_err(|error| {
        Box::new(preflight_check_for_request(
            request,
            "input_schema",
            PreflightStatus::Failed,
            error.to_string(),
        ))
    })?;
    Ok(ValidatedPreflightTarget {
        request: request.clone(),
        available_secret_refs: invocation.available_secret_refs.clone(),
        manifest,
        plugin_dir,
        action,
        input_schema_path,
        tool_name,
    })
}

/// Performs the adapter checks which are safe to run before a live probe.
///
/// This boundary reads installed plugin metadata and evaluates input,
/// prerequisite, and sandbox-plan contracts. It never creates preflight
/// staging, resolves secret values, or starts an adapter process or connection.
pub(crate) fn validate_static_preflight_target(
    invocation: &PreflightInvocation,
    expected_adapter: AdapterKind,
) -> std::result::Result<(ValidatedPreflightTarget, Vec<PreflightCheck>), Box<PreflightCheck>> {
    validate_static_preflight_target_with_catalog(invocation, expected_adapter, None, &[])
}

fn validate_static_preflight_target_with_catalog(
    invocation: &PreflightInvocation,
    expected_adapter: AdapterKind,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
    deferred_paths: &[String],
) -> std::result::Result<(ValidatedPreflightTarget, Vec<PreflightCheck>), Box<PreflightCheck>> {
    if invocation.request.adapter_kind != expected_adapter {
        return Err(Box::new(preflight_check_for_request(
            &invocation.request,
            "adapter_probe",
            PreflightStatus::Failed,
            format!(
                "expected adapter '{}' but received '{}'",
                expected_adapter, invocation.request.adapter_kind
            ),
        )));
    }
    let target = validate_preflight_target_with_catalog(
        invocation,
        expected_adapter,
        catalog,
        deferred_paths,
    )?;
    let group = PreflightProbeGroup {
        step_ids: vec![target.request.step.id.clone()],
        target: target.clone(),
    };
    let mut checks = vec![
        preflight_check_for_request(
            &target.request,
            "plugin_contract",
            PreflightStatus::Passed,
            "plugin contract is valid",
        ),
        preflight_check_for_request(
            &target.request,
            "input_schema",
            if deferred_paths.is_empty() {
                PreflightStatus::Passed
            } else {
                PreflightStatus::DeferredToDispatch
            },
            format!(
                "{} schema '{}'",
                if deferred_paths.is_empty() {
                    "input matches"
                } else {
                    "known input portions pass; complete input deferred for"
                },
                target.input_schema_path.display()
            ),
        ),
    ];
    let prerequisites = prerequisite_checks_for_group(&group);
    let blocked = group_has_blocking_prerequisites(&prerequisites);
    checks.extend(prerequisites);
    if !blocked {
        if let Some(check) = filesystem_policy_static_check(&group) {
            checks.push(check);
        }
        if let Some(check) = network_policy_static_check(&group) {
            checks.push(check);
        }
    }
    Ok((target, checks))
}

/// Validates materialized adapter requests without probing external adapters.
///
/// The function reads installed plugin packages and the local filesystem only.
/// It does not resolve secret values, create staging paths, or execute adapter
/// entrypoints.
pub fn validate_static_preflight_steps(
    invocations: &[PreflightInvocation],
) -> Result<Vec<PreflightCheck>> {
    let mut checks = Vec::new();
    for invocation in invocations {
        match validate_static_preflight_target(invocation, invocation.request.adapter_kind) {
            Ok((_, mut static_checks)) => checks.append(&mut static_checks),
            Err(check) => checks.push(*check),
        }
    }
    Ok(checks)
}

/// Checks each input and current host prerequisites using one operation-scoped catalog.
/// This reads local packages only and creates no staging or transport work.
pub fn validate_static_preflight_steps_with_catalog(
    invocations: &[PreflightInvocation],
    catalog: &transist_plugins::ValidatedPluginCatalog,
) -> Result<Vec<PreflightCheck>> {
    let mut checks = Vec::new();
    for invocation in invocations {
        match validate_static_preflight_target_with_catalog(
            invocation,
            invocation.request.adapter_kind,
            Some(catalog),
            &[],
        ) {
            Ok((_, mut static_checks)) => checks.append(&mut static_checks),
            Err(check) => checks.push(*check),
        }
    }
    Ok(checks)
}

/// Checks a partial authored request without fabricating values or running probes.
/// Masks are explicit JSON-pointer dependencies; full materialization is mandatory
/// before any live probe or adapter effect.
pub fn validate_deferred_preflight_input(
    invocation: &PreflightInvocation,
    deferred_paths: &[String],
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
) -> Result<Vec<PreflightCheck>> {
    Ok(
        match validate_static_preflight_target_with_catalog(
            invocation,
            invocation.request.adapter_kind,
            catalog,
            deferred_paths,
        ) {
            Ok((_, checks)) => checks,
            Err(check) => vec![*check],
        },
    )
}

pub(crate) fn preflight_check_for_request(
    request: &ExecutionRequest,
    kind: impl Into<String>,
    status: PreflightStatus,
    message: impl Into<String>,
) -> PreflightCheck {
    preflight_check(
        kind,
        status,
        vec![request.step.id.clone()],
        Some(request.adapter_kind.to_string()),
        request
            .metadata
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        Some(request.action.clone()),
        message,
    )
}

pub(crate) fn mcp_tool_name(action: &ActionDescriptor) -> Result<String> {
    let tool_name = action
        .bindings
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "MCP action '{}' is missing required tool_name binding",
                action.name
            )
        })?;
    Ok(tool_name.to_string())
}

type EntrypointJsonSerializer = fn(&PluginEntrypoint) -> serde_json::Result<String>;

#[derive(Serialize)]
struct PreflightProbeKey<'a> {
    adapter_kind: &'a AdapterKind,
    plugin_id: &'a str,
    plugin_version: &'a str,
    entrypoint: String,
    action: &'a str,
    tool_name: Option<&'a str>,
    secret_names: &'a [String],
    authority: PreflightProbeAuthority<'a>,
}

#[derive(Serialize)]
struct PreflightProbeAuthority<'a> {
    workspace_dir: &'a str,
    requested_capabilities: Vec<transist_protocol::CapabilityRequest>,
    timeout_ms: Option<u64>,
    max_timeout_ms: Option<u64>,
    policy: serde_json::Value,
    metadata: serde_json::Value,
}

pub(crate) fn preflight_probe_key(target: &ValidatedPreflightTarget) -> Result<String> {
    preflight_probe_key_with_serializer(target, serialize_entrypoint)
}

fn preflight_probe_key_with_serializer(
    target: &ValidatedPreflightTarget,
    serializer: EntrypointJsonSerializer,
) -> Result<String> {
    let mut secret_names = target.available_secret_refs.clone();
    secret_names.sort();
    let entrypoint = serializer(&target.manifest.entrypoint).map_err(|error| {
        anyhow!(
            "failed serializing preflight entrypoint for workflow '{}' step '{}' action '{}' from plugin source '{}': {error}",
            target.request.workflow_id,
            target.request.step.id,
            target.request.action,
            target.plugin_dir.display()
        )
    })?;
    let mut requested_capabilities = target.request.requested_capabilities.clone();
    requested_capabilities.sort();
    let key = PreflightProbeKey {
        adapter_kind: &target.request.adapter_kind,
        plugin_id: &target.manifest.id,
        plugin_version: &target.manifest.version,
        entrypoint,
        action: &target.action.name,
        tool_name: target.tool_name.as_deref(),
        secret_names: &secret_names,
        // Probe execution inherits these request fields. Never share a probe
        // across targets whose sandbox or child-process authority differs.
        authority: PreflightProbeAuthority {
            workspace_dir: &target.request.paths.workspace_dir,
            requested_capabilities,
            timeout_ms: target.request.timeout_ms,
            max_timeout_ms: target.request.max_timeout_ms,
            policy: canonical_json_value(&target.request.policy),
            metadata: canonical_probe_metadata(&target.request.metadata),
        },
    };
    serde_json::to_string(&key).map_err(|error| {
        anyhow!(
            "failed serializing preflight probe key for workflow '{}' step '{}' action '{}' from plugin source '{}': {error}",
            target.request.workflow_id,
            target.request.step.id,
            target.request.action,
            target.plugin_dir.display()
        )
    })
}

fn canonical_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json_value).collect())
        }
        serde_json::Value::Object(values) => {
            let values = values
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json_value(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            serde_json::Value::Object(values.into_iter().collect())
        }
        value => value.clone(),
    }
}

fn canonical_probe_metadata(metadata: &serde_json::Value) -> serde_json::Value {
    let mut metadata = metadata.clone();
    if let Some(values) = metadata.as_object_mut() {
        // These top-level fields identify the invocation, not its authority.
        // Every unknown current or future metadata field remains in the key.
        values.remove("step_id");
        values.remove("workflow_id");
    }
    canonical_json_value(&metadata)
}

fn serialize_entrypoint(entrypoint: &PluginEntrypoint) -> serde_json::Result<String> {
    serde_json::to_string(entrypoint)
}

pub(crate) fn preflight_group_check(
    group: &PreflightProbeGroup,
    kind: impl Into<String>,
    status: PreflightStatus,
    message: impl Into<String>,
) -> PreflightCheck {
    preflight_check(
        kind,
        status,
        group.step_ids.clone(),
        Some(group.target.request.adapter_kind.to_string()),
        group
            .target
            .request
            .metadata
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        Some(group.target.request.action.clone()),
        message,
    )
}

/// Validates filesystem sandbox construction without creating a staging area.
pub(crate) fn filesystem_policy_static_check(
    group: &PreflightProbeGroup,
) -> Option<PreflightCheck> {
    let request = &group.target.request;
    if matches!(
        request.adapter_kind,
        AdapterKind::Process | AdapterKind::Mcp
    ) && let Ok(home) = home_dir_from_run_root(std::path::Path::new(&request.paths.run_root))
        && let Err(error) =
            transist_core::validate_runtime_temp_parent(&runtime_temp_parent(), &home, request)
    {
        return Some(preflight_group_check(
            group,
            "runtime_temp_boundary",
            PreflightStatus::Failed,
            error.to_string(),
        ));
    }
    if !strict_filesystem_policy_requested(request) {
        return None;
    }
    let policy = match filesystem_policy_from_request(request) {
        Ok(Some(policy)) => policy,
        Ok(None) => {
            return Some(preflight_group_check(
                group,
                "filesystem_workspace_boundary",
                PreflightStatus::Failed,
                "filesystem policy requested but policy is missing or malformed",
            ));
        }
        Err(error) => {
            return Some(preflight_group_check(
                group,
                "filesystem_workspace_boundary",
                PreflightStatus::Failed,
                error,
            ));
        }
    };
    let network_policy = network_policy_from_request(request)
        .ok()
        .flatten()
        .unwrap_or_default();
    let sandbox_context = sandbox_context_from_request(request, &group.target.plugin_dir);
    match crate::filesystem_sandbox::validate_sandbox_plan(
        &policy,
        &network_policy,
        &sandbox_context,
    ) {
        Ok(_) => Some(preflight_group_check(
            group,
            "filesystem_workspace_boundary",
            PreflightStatus::Passed,
            format!(
                "workspace-scoped filesystem sandbox backend '{}' can be constructed",
                strict_local_sandbox_backend_label()
            ),
        )),
        Err(error) => Some(preflight_group_check(
            group,
            "filesystem_workspace_boundary",
            PreflightStatus::Failed,
            error,
        )),
    }
}

/// Validates network sandbox construction without creating a staging area.
pub(crate) fn network_policy_static_check(group: &PreflightProbeGroup) -> Option<PreflightCheck> {
    let request = &group.target.request;
    if request.adapter_kind == AdapterKind::Http
        || !strict_local_policy_requested(request)
        || request
            .policy
            .get("network")
            .is_none_or(|value| value.is_null())
    {
        return None;
    }
    let network_policy = match network_policy_from_request(request) {
        Ok(Some(policy)) => policy,
        Ok(None) => {
            return Some(preflight_group_check(
                group,
                "network_egress_boundary",
                PreflightStatus::Failed,
                "network policy requested but policy is missing or malformed",
            ));
        }
        Err(error) => {
            return Some(preflight_group_check(
                group,
                "network_egress_boundary",
                PreflightStatus::Failed,
                error,
            ));
        }
    };
    let filesystem_policy = filesystem_policy_from_request(request)
        .ok()
        .flatten()
        .unwrap_or_default();
    let sandbox_context = sandbox_context_from_request(request, &group.target.plugin_dir);
    match crate::filesystem_sandbox::validate_sandbox_plan(
        &filesystem_policy,
        &network_policy,
        &sandbox_context,
    ) {
        Ok(_) => Some(preflight_group_check(
            group,
            "network_egress_boundary",
            PreflightStatus::Passed,
            format!(
                "local sandbox backend '{}' can enforce strict network policy",
                strict_local_sandbox_backend_label()
            ),
        )),
        Err(error) => Some(preflight_group_check(
            group,
            "network_egress_boundary",
            PreflightStatus::Failed,
            error,
        )),
    }
}

pub(crate) fn prerequisite_checks_for_group(group: &PreflightProbeGroup) -> Vec<PreflightCheck> {
    let mut checks = Vec::new();
    checks.extend(scope_prerequisite_checks(
        group,
        "plugin_prerequisite",
        PrerequisiteScope::Plugin,
        &group.target.manifest.prerequisites,
    ));
    checks.extend(scope_prerequisite_checks(
        group,
        "action_prerequisite",
        PrerequisiteScope::Action,
        &group.target.action.prerequisites,
    ));
    checks
}

pub(crate) fn group_has_blocking_prerequisites(checks: &[PreflightCheck]) -> bool {
    checks.iter().any(|check| {
        matches!(
            check.kind.as_str(),
            "plugin_prerequisite" | "action_prerequisite"
        ) && check.status == PreflightStatus::Failed
    })
}

fn scope_prerequisite_checks(
    group: &PreflightProbeGroup,
    preflight_kind: &str,
    scope: PrerequisiteScope,
    prerequisites: &transist_prerequisites::PrerequisiteSet,
) -> Vec<PreflightCheck> {
    if prerequisites.is_empty() {
        return Vec::new();
    }
    let report = match evaluate_prerequisites(
        prerequisites,
        &EvaluationContext::new(scope, &group.target.plugin_dir),
    ) {
        Ok(report) => report,
        Err(error) => {
            return vec![preflight_group_check(
                group,
                preflight_kind,
                PreflightStatus::Failed,
                error.to_string(),
            )];
        }
    };
    report
        .checks
        .into_iter()
        .map(|check| {
            preflight_group_check(
                group,
                preflight_kind,
                match check.status {
                    HostPrerequisiteStatus::Passed => PreflightStatus::Passed,
                    HostPrerequisiteStatus::Warning => PreflightStatus::Warning,
                    HostPrerequisiteStatus::Failed => PreflightStatus::Failed,
                },
                format!(
                    "[{}:{}] {}",
                    check.scope.as_str(),
                    check.kind,
                    check.message
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod handshake_diagnostics;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use transist_plugins::{PluginCapabilities, PluginEntrypoint, PluginPackage};
    use transist_protocol::{InvokePaths, InvokeStep};

    fn failing_serializer(_: &PluginEntrypoint) -> serde_json::Result<String> {
        serde_json::from_str("[")
    }

    #[test]
    fn preflight_concurrency_config_defaults_and_enforces_bounds() {
        assert_eq!(
            preflight_probe_concurrency_from_value(None).expect("default"),
            crate::DEFAULT_PREFLIGHT_PROBE_CONCURRENCY
        );
        for value in ["1", "4", "32"] {
            assert_eq!(
                preflight_probe_concurrency_from_value(Some(OsStr::new(value)))
                    .expect("valid limit"),
                value.parse::<usize>().unwrap()
            );
        }
        for value in ["0", "33", "invalid", "", "-1"] {
            let error = preflight_probe_concurrency_from_value(Some(OsStr::new(value)))
                .expect_err("invalid limit");
            assert!(
                error.to_string().contains(PREFLIGHT_PROBE_CONCURRENCY_ENV),
                "{value}: {error:#}"
            );
        }
    }

    #[test]
    fn preflight_concurrency_scheduler_is_bounded_and_returns_stable_order() {
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let jobs = (0..12).map(|slot| (slot, slot)).collect();

        let completed = run_bounded_ordered_jobs(jobs, 3, |value| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            thread::sleep(Duration::from_millis((3 - value % 3) as u64 * 5));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(value)
        })
        .expect("scheduled jobs");

        assert_eq!(
            completed,
            (0..12).map(|slot| (slot, slot)).collect::<Vec<_>>()
        );
        assert!(maximum.load(Ordering::SeqCst) > 1);
        assert!(maximum.load(Ordering::SeqCst) <= 3);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn preflight_concurrency_scheduler_propagates_measurement_context() {
        let (_, records) = transist_core::capture_preflight_measurements(|| {
            transist_core::with_preflight_measurement_scope(
                transist_core::PreflightMeasurementScope::ApplicationReadiness,
                || {
                    run_bounded_ordered_jobs(vec![(0, ()), (1, ())], 2, |_| -> Result<()> {
                        transist_core::record_preflight_probe_batch(AdapterKind::Process, 0, 1);
                        Ok(())
                    })
                    .expect("measured jobs");
                },
            )
        });

        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record,
                    transist_core::PreflightMeasurement::ProbeBatch {
                        scope: transist_core::PreflightMeasurementScope::ApplicationReadiness,
                        adapter_kind: AdapterKind::Process,
                        remote_probe_count: 1,
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn preflight_concurrency_scheduler_stops_queued_work_and_selects_earliest_error() {
        let started = AtomicUsize::new(0);
        let error = run_bounded_ordered_jobs(
            (0..8).map(|slot| (slot, slot)).collect(),
            2,
            |slot| -> Result<usize> {
                started.fetch_add(1, Ordering::SeqCst);
                if slot == 0 {
                    thread::sleep(Duration::from_millis(20));
                    bail!("first error");
                }
                bail!("later error")
            },
        )
        .expect_err("scheduler error");

        assert!(
            error.to_string().contains("stable group slot 0"),
            "{error:#}"
        );
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "queued jobs must not start"
        );
    }

    #[test]
    fn preflight_concurrency_scheduler_keeps_mixed_results_deterministic() {
        let expected = vec!["passed", "warning", "failed-check", "passed"];
        for _ in 0..16 {
            let completed = run_bounded_ordered_jobs(
                expected.iter().copied().enumerate().collect::<Vec<_>>(),
                4,
                |status| {
                    thread::sleep(Duration::from_millis(match status {
                        "passed" => 8,
                        "warning" => 4,
                        _ => 1,
                    }));
                    Ok(status)
                },
            )
            .expect("mixed checks");

            assert_eq!(
                completed
                    .into_iter()
                    .map(|(_, status)| status)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn preflight_concurrency_scheduler_converts_worker_panic() {
        let dropped = Arc::new(AtomicUsize::new(0));
        struct DropMarker(Arc<AtomicUsize>);
        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let marker = DropMarker(Arc::clone(&dropped));
        let error = run_bounded_ordered_jobs(vec![(0, marker)], 1, |_marker| -> Result<()> {
            panic!("injected worker panic")
        })
        .expect_err("panic must become an error");

        assert!(
            error.to_string().contains("stable group slot 0"),
            "{error:#}"
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn preflight_concurrency_private_paths_are_removed_when_prepared_work_is_dropped() {
        let home = tempfile::tempdir().expect("home");
        let mut request = target().request;
        request.paths.run_root = home.path().join("runs/run-1").display().to_string();
        let private = private_preflight_request(&request).expect("private request");
        let staging = PathBuf::from(&private.request.paths.run_root);
        let runtime_temp = private.request.metadata[RUNTIME_TEMP_DIR_METADATA_KEY]
            .as_str()
            .map(PathBuf::from)
            .expect("runtime temp path");
        let runtime_temp_owner = private._runtime_temp_root.owner_path.clone();

        assert!(staging.is_dir());
        assert!(runtime_temp.is_dir());
        assert!(runtime_temp_owner.is_file());
        drop(private);
        assert!(!staging.exists(), "private staging directory leaked");
        assert!(
            !runtime_temp.exists(),
            "private runtime temp directory leaked"
        );
        assert!(
            !runtime_temp_owner.exists(),
            "private runtime temp owner lock leaked"
        );
    }

    #[test]
    fn preflight_runtime_temp_cleanup_reaps_orphan_owner_locks() {
        let parent = runtime_temp_parent();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let orphan_owner = parent.join(format!(
            ".{RUNTIME_TEMP_PREFIX}orphan-{unique}{RUNTIME_TEMP_OWNER_SUFFIX}"
        ));
        fs::write(&orphan_owner, b"orphan").expect("orphan owner");
        set_private_file_mode(&orphan_owner).expect("orphan owner mode");

        cleanup_stale_runtime_temp_roots().expect("cleanup");

        assert!(!orphan_owner.exists(), "orphan owner lock must be reaped");
    }

    #[cfg(unix)]
    #[test]
    fn preflight_runtime_temp_cleanup_ignores_invalid_owner_records() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let parent = tempfile::tempdir_in("/tmp").expect("isolated temporary parent");
        let target = parent.path().join("untouched-target");
        fs::write(&target, b"not runtime data").expect("symlink target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).expect("target mode");
        let mut records = Vec::new();
        let mut listeners = Vec::new();
        for orphan in [false, true] {
            for kind in ["symlink", "dangling", "directory", "socket", "inaccessible"] {
                let root = parent
                    .path()
                    .join(format!("{RUNTIME_TEMP_PREFIX}{kind}-{orphan}"));
                if !orphan {
                    fs::create_dir(&root).expect("root");
                    set_private_mode(&root).expect("root mode");
                    fs::write(root.join("keep"), b"keep").expect("root contents");
                }
                let owner = runtime_temp_owner_path(&root).expect("owner path");
                match kind {
                    "symlink" => symlink(&target, &owner).expect("symlink owner"),
                    "dangling" => symlink(parent.path().join("missing"), &owner)
                        .expect("dangling symlink owner"),
                    "directory" => fs::create_dir(&owner).expect("directory owner"),
                    "socket" => listeners.push(UnixListener::bind(&owner).expect("socket owner")),
                    "inaccessible" => {
                        fs::write(&owner, b"keep owner").expect("inaccessible owner");
                        fs::set_permissions(&owner, fs::Permissions::from_mode(0o000))
                            .expect("inaccessible owner mode");
                    }
                    _ => unreachable!(),
                }
                assert!(
                    open_existing_runtime_temp_owner((!orphan).then_some(root.as_path()), &owner)
                        .expect("invalid record must not fail owner lookup")
                        .is_none(),
                    "{kind}, orphan={orphan}"
                );
                records.push((root, owner, orphan));
            }
        }
        let stale = parent.path().join(format!("{RUNTIME_TEMP_PREFIX}stale"));
        fs::create_dir(&stale).expect("stale root");
        set_private_mode(&stale).expect("stale root mode");
        let stale_owner = runtime_temp_owner_path(&stale).expect("stale owner path");
        fs::write(&stale_owner, b"stale").expect("stale owner");
        set_private_file_mode(&stale_owner).expect("stale owner mode");

        cleanup_stale_runtime_temp_roots_in(parent.path()).expect("cleanup must continue");

        assert!(!stale.exists(), "eligible stale root must still be reaped");
        assert!(!stale_owner.exists(), "eligible owner must still be reaped");
        for (root, owner, orphan) in records {
            assert!(
                fs::symlink_metadata(&owner).is_ok(),
                "owner must be retained"
            );
            if !orphan {
                assert_eq!(fs::read(root.join("keep")).expect("retained root"), b"keep");
            }
        }
        assert_eq!(
            fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o000,
            "cleanup must not change the symlink target"
        );
        set_private_file_mode(&target).expect("restore target mode");
        assert_eq!(
            fs::read(&target).expect("retained target"),
            b"not runtime data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_runtime_temp_owner_lookup_rejects_foreign_uid() {
        let parent = tempfile::tempdir_in("/tmp").expect("isolated temporary parent");
        set_private_mode(parent.path()).expect("root mode");
        let owner = parent.path().join("owner");
        fs::write(&owner, b"owner").expect("owner");
        set_private_file_mode(&owner).expect("owner mode");
        let owner_uid = fs::symlink_metadata(&owner).expect("owner metadata").uid();
        for root in [None, Some(parent.path())] {
            assert!(
                open_existing_runtime_temp_owner_checked(root, &owner, owner_uid)
                    .expect("owned record lookup")
                    .is_some()
            );
            // Model another caller's UID through the production lookup body,
            // without chown, credential changes, or test-only unsafe syscalls.
            assert!(
                open_existing_runtime_temp_owner_checked(root, &owner, owner_uid.wrapping_add(1))
                    .expect("foreign record must not fail owner lookup")
                    .is_none()
            );
        }
        assert_eq!(fs::read(&owner).expect("retained owner"), b"owner");
    }

    #[cfg(unix)]
    #[test]
    fn preflight_runtime_temp_cleanup_tolerates_a_competing_unlink() {
        let parent = tempfile::tempdir().expect("isolated parent");
        let root = parent
            .path()
            .join(format!("{RUNTIME_TEMP_PREFIX}competing"));
        fs::create_dir(&root).expect("root");
        set_private_mode(&root).expect("private root");
        let owner = runtime_temp_owner_path(&root).expect("owner path");
        fs::write(&owner, b"stale").expect("owner");
        set_private_file_mode(&owner).expect("private owner");
        let first = open_existing_runtime_temp_owner(Some(&root), &owner)
            .expect("first lookup")
            .expect("first owner");
        let second = open_existing_runtime_temp_owner(Some(&root), &owner)
            .expect("second lookup")
            .expect("second owner");
        assert!(try_lock_runtime_temp_owner(&first).expect("first lock"));
        remove_stale_runtime_temp_entries(Some(&root), &owner).expect("winning sweep");
        drop(first);
        assert!(try_lock_runtime_temp_owner(&second).expect("old inode lock"));
        remove_stale_runtime_temp_entries(Some(&root), &owner).expect("losing sweep is idempotent");
        assert!(!root.exists());
        assert!(!owner.exists());
        assert_eq!(
            stale_entry_removed(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            )))
            .expect_err("permission failures must propagate")
            .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    pub(super) fn target() -> ValidatedPreflightTarget {
        ValidatedPreflightTarget {
            request: ExecutionRequest {
                message_type: "tep.invoke.request.v1".to_string(),
                protocol_version: "tep.v1".to_string(),
                invoke_id: "invoke-1".to_string(),
                run_id: "run-1".to_string(),
                workflow_id: "workflow-1".to_string(),
                step: InvokeStep {
                    id: "step-1".to_string(),
                    execution: 1,
                },
                adapter_kind: AdapterKind::Process,
                action: "run".to_string(),
                input: serde_json::json!({"value": "input"}),
                paths: InvokePaths {
                    run_root: "/runs/run-1".to_string(),
                    artifacts_dir: "/runs/run-1/artifacts".to_string(),
                    evidence_dir: "/runs/run-1/evidence".to_string(),
                    workspace_dir: "/workspace".to_string(),
                },
                requested_capabilities: Vec::new(),
                timeout_ms: None,
                max_timeout_ms: None,
                policy: serde_json::json!({}),
                metadata: serde_json::json!({"plugin_id": "plugin"}),
            },
            available_secret_refs: Vec::new(),
            manifest: PluginManifest {
                schema: "transist.plugin.v1".to_string(),
                id: "plugin".to_string(),
                version: "1.0.0".to_string(),
                adapter: AdapterKind::Process,
                entrypoint: PluginEntrypoint::Process {
                    command: "plugin".to_string(),
                    args: Vec::new(),
                    cwd: None,
                    env: BTreeMap::new(),
                },
                package: PluginPackage { files: Vec::new() },
                prerequisites: Default::default(),
                actions: Vec::new(),
                capabilities: PluginCapabilities {
                    supports_cancel: false,
                    supports_health: false,
                    supports_artifact_hints: false,
                    supports_evidence_hints: false,
                },
                default_provider_kind: None,
            },
            plugin_dir: PathBuf::from("/plugins/plugin/1.0.0"),
            action: ActionDescriptor {
                name: "run".to_string(),
                input_schema: "input.json".to_string(),
                output_schema: "output.json".to_string(),
                secret_inputs: Vec::new(),
                bindings: BTreeMap::new(),
                prerequisites: Default::default(),
                provider_kind: None,
            },
            input_schema_path: PathBuf::from("/plugins/plugin/1.0.0/input.json"),
            tool_name: None,
        }
    }

    #[test]
    fn validation_reuse_checks_each_input_with_shared_catalog_without_staging() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let dir = home.path().join("plugins/plugin/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        let mut fixture = target();
        fixture.manifest.package.files =
            vec!["plugin".into(), "input.json".into(), "output.json".into()];
        fixture.manifest.actions = vec![fixture.action.clone()];
        fs::write(dir.join("plugin"), "#!/bin/sh\nexit 99\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(dir.join("plugin"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            dir.join("input.json"),
            r#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}"#,
        )
        .unwrap();
        fs::write(dir.join("output.json"), "{}").unwrap();
        fs::write(
            dir.join("plugin.yaml"),
            yaml_serde::to_string(&fixture.manifest).unwrap(),
        )
        .unwrap();
        fixture.request.metadata["plugin_version"] = "1.0.0".into();
        let first = PreflightInvocation {
            request: fixture.request.clone(),
            available_secret_refs: vec![],
            remote_secret_policy: Default::default(),
        };
        let mut second = first.clone();
        second.request.step.id = "second".into();
        second.request.input = serde_json::json!({"value": 42});
        let catalog = transist_plugins::ValidatedPluginCatalog::new(
            home.path().to_path_buf(),
            BTreeMap::new(),
        );
        let checks =
            validate_static_preflight_steps_with_catalog(&[first, second], &catalog).unwrap();
        assert!(checks.iter().any(|c| c.kind == "input_schema"
            && c.status == PreflightStatus::Passed
            && c.step_ids == ["step-1"]));
        assert!(checks.iter().any(|c| c.kind == "input_schema"
            && c.status == PreflightStatus::Failed
            && c.step_ids == ["second"]));
        assert!(!home.path().join(".transist-staging").exists());
    }

    #[test]
    fn preflight_entrypoint_serialization_failure_is_contextual() {
        let error = preflight_probe_key_with_serializer(&target(), failing_serializer)
            .expect_err("injected serializer failure");

        let message = error.to_string();
        assert!(message.contains("workflow 'workflow-1'"), "{message}");
        assert!(message.contains("step 'step-1'"), "{message}");
        assert!(message.contains("action 'run'"), "{message}");
        assert!(message.contains("/plugins/plugin/1.0.0"), "{message}");
    }

    #[test]
    fn preflight_probe_key_does_not_group_distinct_authority_contexts() {
        let target = target();
        let base_key = preflight_probe_key(&target).expect("base key");

        let mut filesystem = target.clone();
        filesystem.request.policy = serde_json::json!({
            "filesystem": {"workspace_access": "read_only"}
        });
        let filesystem_key = preflight_probe_key(&filesystem).expect("filesystem key");
        assert_ne!(base_key, filesystem_key);
        let groups = BTreeMap::from([(base_key.clone(), ()), (filesystem_key, ())]);
        assert_eq!(
            groups.len(),
            2,
            "distinct authority must create distinct groups"
        );

        let mut network = target.clone();
        network.request.policy = serde_json::json!({
            "network": {"mode": "deny"}
        });
        assert_ne!(
            base_key,
            preflight_probe_key(&network).expect("network key")
        );

        let mut workspace = target.clone();
        workspace.request.paths.workspace_dir = "/other-workspace".to_string();
        assert_ne!(
            base_key,
            preflight_probe_key(&workspace).expect("workspace key")
        );

        let mut capabilities = target.clone();
        capabilities.request.requested_capabilities = vec![
            transist_protocol::CapabilityRequest::new(
                transist_protocol::CapabilityName::Filesystem,
                "workspace:///workspace",
                ["read"],
            )
            .expect("valid capability"),
        ];
        assert_ne!(
            base_key,
            preflight_probe_key(&capabilities).expect("capability key")
        );

        let mut runtime_context = target.clone();
        runtime_context.request.policy = serde_json::json!({
            "runtime_auth_context": {"active": true, "child_env_allowlist": ["PATH"]}
        });
        assert_ne!(
            base_key,
            preflight_probe_key(&runtime_context).expect("runtime context key")
        );

        let mut metadata_context = target;
        metadata_context.request.metadata = serde_json::json!({
            "plugin_id": "plugin",
            "runtime_metadata_file": "/runtime/context.json"
        });
        assert_ne!(
            base_key,
            preflight_probe_key(&metadata_context).expect("metadata context key")
        );
    }

    #[test]
    fn preflight_probe_key_ignores_only_top_level_invocation_identity_metadata() {
        let mut first = target();
        first.request.metadata = serde_json::json!({
            "plugin_id": "plugin",
            "step_id": "step-a",
            "workflow_id": "workflow-a",
            "nested": {"step_id": "nested-a", "workflow_id": "nested-workflow-a"}
        });
        let mut second = first.clone();
        second.request.step.id = "step-b".to_string();
        second.request.workflow_id = "workflow-b".to_string();
        second.request.metadata["step_id"] = serde_json::json!("step-b");
        second.request.metadata["workflow_id"] = serde_json::json!("workflow-b");

        assert_eq!(
            preflight_probe_key(&first).expect("first key"),
            preflight_probe_key(&second).expect("second key"),
            "top-level invocation identities must not fragment equivalent authority"
        );

        second.request.metadata["nested"]["step_id"] = serde_json::json!("nested-b");
        assert_ne!(
            preflight_probe_key(&first).expect("first key"),
            preflight_probe_key(&second).expect("nested identity key"),
            "nested identity-like metadata remains authority-significant"
        );
    }

    #[test]
    fn preflight_probe_key_splits_every_authority_field() {
        let target = target();
        let base_key = preflight_probe_key(&target).expect("base key");

        let mut cases = Vec::new();
        let mut policy = target.clone();
        policy.request.policy =
            serde_json::json!({"filesystem": {"workspace_access": "read_only"}});
        cases.push(("policy", policy));
        let mut network = target.clone();
        network.request.policy = serde_json::json!({"network": {"mode": "deny"}});
        cases.push(("network", network));
        let mut workspace = target.clone();
        workspace.request.paths.workspace_dir = "/other-workspace".to_string();
        cases.push(("workspace", workspace));
        let mut capabilities = target.clone();
        capabilities.request.requested_capabilities = vec![
            transist_protocol::CapabilityRequest::new(
                transist_protocol::CapabilityName::Filesystem,
                "workspace:///workspace",
                ["read"],
            )
            .expect("valid capability"),
        ];
        cases.push(("requested capabilities", capabilities));
        let mut timeout = target.clone();
        timeout.request.timeout_ms = Some(1);
        cases.push(("timeout", timeout));
        let mut max_timeout = target.clone();
        max_timeout.request.max_timeout_ms = Some(2);
        cases.push(("maximum timeout", max_timeout));
        let mut runtime_auth = target.clone();
        runtime_auth.request.policy = serde_json::json!({
            "runtime_auth_context": {"active": true, "child_env_allowlist": ["PATH"]}
        });
        cases.push(("runtime auth context", runtime_auth));
        let mut runtime_metadata = target.clone();
        runtime_metadata.request.metadata["runtime_metadata_file"] =
            serde_json::json!("/runtime/context.json");
        cases.push(("runtime metadata file", runtime_metadata));
        let mut unknown_metadata = target;
        unknown_metadata.request.metadata["future_authority"] = serde_json::json!({"value": 1});
        cases.push(("unknown metadata", unknown_metadata));

        for (name, case) in cases {
            assert_ne!(
                base_key,
                preflight_probe_key(&case).unwrap_or_else(|error| panic!("{name}: {error}")),
                "{name} must split probe groups"
            );
        }
    }

    #[test]
    fn probe_group_step_ids_are_duplicate_free_and_in_insertion_order() {
        let mut groups = BTreeMap::new();
        for step_id in ["step-b", "step-a", "step-b"] {
            let mut target = target();
            target.request.step.id = step_id.to_string();
            target.request.metadata["step_id"] = serde_json::json!(step_id);
            insert_preflight_probe_target(&mut groups, target).expect("insert target");
        }

        let group = groups.into_values().next().expect("probe group");
        assert_eq!(group.step_ids, ["step-b", "step-a"]);
    }
}
