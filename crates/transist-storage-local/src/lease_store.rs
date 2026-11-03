// SPDX-License-Identifier: MPL-2.0

use crate::util::{now_unix_ms, write_json_create_new};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use transist_core::ports::{
    ExternalOwnerMetadata, LeaseBreakOptions, LeaseConflict, LeaseHandle, LeasePort,
    ManagementMode, RunTreeLeaseRecord, RuntimeOwnerIdentity,
};

/// Default heartbeat interval applied by the lease-owning runtime process when
/// no override is supplied via [`reaper_config_from_env`].
pub const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
/// Default age past `last_heartbeat_ms` after which a lease whose owning PID
/// is no longer alive is considered orphaned and reapable.
pub const DEFAULT_STALE_AFTER_MS: u64 = 30_000;

const HEARTBEAT_INTERVAL_MS: u64 = DEFAULT_HEARTBEAT_INTERVAL_MS;

/// Environment variable: override the orphan-lease staleness threshold (ms).
pub const REAPER_STALE_AFTER_MS_ENV: &str = "TRANSIST_REAPER_STALE_AFTER_MS";
/// Environment variable: when set to `1`/`true`, disables the passive reaper.
pub const REAPER_DISABLE_ENV: &str = "TRANSIST_REAPER_DISABLE";

/// Tunables for the passive orphan-lease reaper.
#[derive(Debug, Clone, Copy)]
pub struct ReaperConfig {
    /// When true, [`LocalLeaseStore::release_orphan_leases`] returns an empty
    /// vec without scanning the lease directory.
    pub disabled: bool,
    /// Age past `last_heartbeat_ms` after which a lease becomes reapable.
    pub stale_after_ms: u64,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
        }
    }
}

/// Reads [`ReaperConfig`] from the process environment, falling back to
/// defaults when env vars are unset or unparseable.
pub fn reaper_config_from_env() -> ReaperConfig {
    let mut cfg = ReaperConfig::default();
    if let Ok(raw) = std::env::var(REAPER_DISABLE_ENV) {
        let raw = raw.trim();
        cfg.disabled = matches!(
            raw.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    if let Ok(raw) = std::env::var(REAPER_STALE_AFTER_MS_ENV) {
        if let Ok(parsed) = raw.trim().parse::<u64>() {
            cfg.stale_after_ms = parsed;
        }
    }
    cfg
}

struct ActiveLeaseSession {
    refcount: usize,
    stop_tx: mpsc::Sender<()>,
    join_handle: thread::JoinHandle<()>,
}

/// Filesystem-backed run-tree lease store used by local runtime composition.
pub struct LocalLeaseStore {
    root: PathBuf,
    owner: RuntimeOwnerIdentity,
    active: Mutex<BTreeMap<String, ActiveLeaseSession>>,
}

impl LocalLeaseStore {
    /// Creates a local lease store rooted at the given Transist home directory.
    pub fn new(root: impl Into<PathBuf>, owner: RuntimeOwnerIdentity) -> Self {
        Self {
            root: root.into(),
            owner,
            active: Mutex::new(BTreeMap::new()),
        }
    }

    fn lease_path(&self, root_run_id: &str) -> PathBuf {
        self.root
            .join("locks")
            .join("runs")
            .join(format!("{root_run_id}.lock"))
    }

    fn base_lease_record(&self, root_run_id: &str) -> RunTreeLeaseRecord {
        let now_ms = now_unix_ms();
        let external = match self.owner.management_mode {
            ManagementMode::Standalone => None,
            ManagementMode::Managed => {
                self.owner
                    .external
                    .as_ref()
                    .map(|external| ExternalOwnerMetadata {
                        host_id: external.host_id.clone(),
                        runner_id: external.runner_id.clone(),
                        external_run_id: external
                            .external_run_id
                            .clone()
                            .or_else(|| Some(root_run_id.to_string())),
                    })
            }
        };
        RunTreeLeaseRecord {
            schema_version: RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
            root_run_id: root_run_id.to_string(),
            lease_id: self.owner.lease_id.clone(),
            owner_id: self.owner.owner_id.clone(),
            management_mode: self.owner.management_mode,
            pid: self.owner.pid,
            hostname: self.owner.hostname.clone(),
            command: self.owner.command.clone(),
            transist_version: self.owner.transist_version.clone(),
            acquired_at_ms: now_ms,
            last_heartbeat_ms: now_ms,
            external,
        }
    }

    fn start_heartbeat_session(
        &self,
        root_run_id: &str,
        handle: &LeaseHandle,
    ) -> ActiveLeaseSession {
        let (stop_tx, stop_rx) = mpsc::channel();
        let lease_path = self.lease_path(root_run_id);
        let lease_id = handle.lease_id.clone();
        let owner = handle.owner.clone();
        let root_run_id = root_run_id.to_string();
        let join_handle = thread::spawn(move || {
            loop {
                match stop_rx.recv_timeout(Duration::from_millis(HEARTBEAT_INTERVAL_MS)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // A transient filesystem failure must not turn an
                        // active owner into a permanently stale lease.
                        let _ = refresh_lease_file(&lease_path, &root_run_id, &lease_id, &owner);
                    }
                }
            }
        });
        ActiveLeaseSession {
            refcount: 1,
            stop_tx,
            join_handle,
        }
    }
}

impl LeasePort for LocalLeaseStore {
    fn owner_identity(&self) -> RuntimeOwnerIdentity {
        self.owner.clone()
    }

    fn acquire_root_run_tree_lease(&self, root_run_id: &str) -> Result<LeaseHandle> {
        let handle = LeaseHandle {
            root_run_id: root_run_id.to_string(),
            lease_id: self.owner.lease_id.clone(),
            owner: self.owner.clone(),
        };

        let mut guard = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("lease store poisoned"))?;
        if let Some(session) = guard.get_mut(root_run_id) {
            session.refcount = session.refcount.saturating_add(1);
            return Ok(handle);
        }

        let path = self.lease_path(root_run_id);
        if path.exists() {
            let record = load_lease_record(&path)?;
            if record.lease_id == handle.lease_id && record.owner_id == handle.owner.owner_id {
                guard.insert(
                    root_run_id.to_string(),
                    self.start_heartbeat_session(root_run_id, &handle),
                );
                return Ok(handle);
            }
            return Err(LeaseConflict {
                root_run_id: root_run_id.to_string(),
                live_lease: record,
            }
            .into());
        }

        let record = self.base_lease_record(root_run_id);
        match write_json_create_new(&path, &record) {
            Ok(()) => {
                guard.insert(
                    root_run_id.to_string(),
                    self.start_heartbeat_session(root_run_id, &handle),
                );
                Ok(handle)
            }
            Err(error) if path.exists() => {
                let record = load_lease_record(&path).or(Err(error))?;
                if record.lease_id == handle.lease_id && record.owner_id == handle.owner.owner_id {
                    guard.insert(
                        root_run_id.to_string(),
                        self.start_heartbeat_session(root_run_id, &handle),
                    );
                    return Ok(handle);
                }
                Err(LeaseConflict {
                    root_run_id: root_run_id.to_string(),
                    live_lease: record,
                }
                .into())
            }
            Err(error) => Err(error),
        }
    }

    fn refresh_root_run_tree_lease(&self, handle: &LeaseHandle) -> Result<RunTreeLeaseRecord> {
        refresh_lease_file(
            &self.lease_path(&handle.root_run_id),
            &handle.root_run_id,
            &handle.lease_id,
            &handle.owner,
        )
    }

    fn inspect_root_run_tree_lease(&self, root_run_id: &str) -> Result<Option<RunTreeLeaseRecord>> {
        let path = self.lease_path(root_run_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(load_lease_record(&path)?))
    }

    fn break_root_run_tree_lease(
        &self,
        root_run_id: &str,
        options: LeaseBreakOptions,
    ) -> Result<Option<RunTreeLeaseRecord>> {
        let path = self.lease_path(root_run_id);
        if !path.exists() {
            return Ok(None);
        }
        let record = load_lease_record(&path)?;
        let local_owner_alive = is_pid_alive(record.pid);
        if local_owner_alive && !options.force_live_owner {
            anyhow::bail!(
                "refusing to break live lease for root run tree '{}'; rerun with --force-live-owner to override",
                root_run_id
            );
        }

        let maybe_local_session = {
            let mut guard = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("lease store poisoned"))?;
            guard.remove(root_run_id)
        };
        if let Some(session) = maybe_local_session {
            let _ = session.stop_tx.send(());
            let _ = session.join_handle.join();
        }

        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed removing lease file: {}", path.display()))?;
        }
        Ok(Some(record))
    }

    fn release_root_run_tree_lease(&self, handle: &LeaseHandle) -> Result<()> {
        let maybe_session = {
            let mut guard = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("lease store poisoned"))?;
            let Some(session) = guard.get_mut(&handle.root_run_id) else {
                return Ok(());
            };
            if session.refcount > 1 {
                session.refcount -= 1;
                return Ok(());
            }
            guard.remove(&handle.root_run_id)
        };

        if let Some(session) = maybe_session {
            let _ = session.stop_tx.send(());
            let _ = session.join_handle.join();
        }
        let path = self.lease_path(&handle.root_run_id);
        let result = (|| {
            if !path.exists() {
                return Ok(());
            }
            let record = load_lease_record(&path)?;
            if record.lease_id == handle.lease_id && record.owner_id == handle.owner.owner_id {
                fs::remove_file(&path)
                    .with_context(|| format!("failed removing lease file: {}", path.display()))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            // Keep ownership alive after a failed unlock attempt. The first
            // session was stopped before removal to prevent a heartbeat from
            // recreating a successfully removed lock, so restart it only when
            // the lock still exists and the caller must retry or reap it.
            if path.exists() {
                let mut guard = self
                    .active
                    .lock()
                    .map_err(|_| anyhow::anyhow!("lease store poisoned"))?;
                guard
                    .entry(handle.root_run_id.clone())
                    .or_insert_with(|| self.start_heartbeat_session(&handle.root_run_id, handle));
            }
            return Err(error);
        }
        Ok(())
    }
}

/// Creates a shared filesystem-backed lease store.
pub fn shared_lease_store(
    root: impl Into<PathBuf>,
    owner: RuntimeOwnerIdentity,
) -> Arc<LocalLeaseStore> {
    Arc::new(LocalLeaseStore::new(root, owner))
}

fn load_lease_record(path: &Path) -> Result<RunTreeLeaseRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading lease file: {}", path.display()))?;
    let record: RunTreeLeaseRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid lease json: {}", path.display()))?;
    if record.schema_version != RunTreeLeaseRecord::SCHEMA_VERSION {
        anyhow::bail!(
            "lease '{}' uses unsupported schema '{}'",
            path.display(),
            record.schema_version
        );
    }
    Ok(record)
}

fn refresh_lease_file(
    path: &Path,
    root_run_id: &str,
    lease_id: &str,
    owner: &RuntimeOwnerIdentity,
) -> Result<RunTreeLeaseRecord> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed opening lease for refresh: {}", path.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("failed reading lease file: {}", path.display()))?;
    let mut record: RunTreeLeaseRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid lease json: {}", path.display()))?;
    if record.root_run_id != root_run_id
        || record.lease_id != lease_id
        || record.owner_id != owner.owner_id
    {
        anyhow::bail!(
            "lease '{}' is no longer owned by '{}'",
            path.display(),
            owner.owner_id
        );
    }
    record.last_heartbeat_ms = now_unix_ms();
    let payload = serde_json::to_vec_pretty(&record)
        .with_context(|| format!("failed serializing lease: {}", path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed seeking lease file: {}", path.display()))?;
    file.write_all(&payload)
        .with_context(|| format!("failed writing lease file: {}", path.display()))?;
    file.set_len(payload.len() as u64)
        .with_context(|| format!("failed truncating lease file: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed syncing lease file: {}", path.display()))?;
    Ok(record)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` checks liveness without sending a signal.
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return !process_is_zombie(pid);
    }
    matches!(std::io::Error::last_os_error().raw_os_error(), Some(code) if code == libc::EPERM)
}

/// Returns whether a Linux PID remains only as an unreaped zombie.
///
/// A zombie still accepts `kill(pid, 0)` but cannot own a runnable detached
/// runner lease, so lease recovery must treat it as terminated.
#[cfg(target_os = "linux")]
fn process_is_zombie(pid: u32) -> bool {
    let Ok(raw) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(end_comm) = raw.rfind(')') else {
        return false;
    };
    raw[end_comm + 1..]
        .split_whitespace()
        .next()
        .is_some_and(|state| state == "Z")
}

/// macOS `kill(pid, 0)` remains the portable liveness probe for leases.
#[cfg(not(target_os = "linux"))]
fn process_is_zombie(_pid: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

impl LocalLeaseStore {
    /// Sweeps `<home>/locks/runs/*.lock` and removes lease files whose owning
    /// PID is no longer alive on this host **and** whose last heartbeat is
    /// older than `cfg.stale_after_ms`.
    ///
    /// This is a *passive* reaper: it never signals the owning process, never
    /// touches active-invocation records, and never reaps a lease this store
    /// instance currently owns. It is safe to call from any read-only command
    /// path.
    ///
    /// Returns the records of the leases that were reaped, in directory order.
    /// A disabled reaper (via [`ReaperConfig::disabled`]) returns an empty vec
    /// without scanning the filesystem.
    pub fn release_orphan_leases(&self, cfg: ReaperConfig) -> Result<Vec<RunTreeLeaseRecord>> {
        if cfg.disabled {
            return Ok(Vec::new());
        }
        let lock_dir = self.root.join("locks").join("runs");
        let entries = match fs::read_dir(&lock_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed scanning {}", lock_dir.display()));
            }
        };

        let now_ms = now_unix_ms();
        let active_root_ids: std::collections::BTreeSet<String> = match self.active.lock() {
            Ok(guard) => guard.keys().cloned().collect(),
            Err(_) => return Err(anyhow::anyhow!("lease store poisoned")),
        };

        let mut reaped = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
                continue;
            }
            // Best-effort: ignore unreadable / non-conforming files so a single
            // corrupt entry does not block the sweep.
            let Ok(record) = load_lease_record(&path) else {
                continue;
            };
            if active_root_ids.contains(&record.root_run_id) {
                continue;
            }
            // Cross-host PID checks are unsafe; only reap leases owned by this
            // host. Same-host leases whose PID is alive are never reaped, even
            // if the heartbeat thread is delayed.
            if record.hostname != self.owner.hostname {
                continue;
            }
            if is_pid_alive(record.pid) {
                continue;
            }
            if !record.is_stale(now_ms, cfg.stale_after_ms) {
                continue;
            }
            if let Err(error) = fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error).with_context(|| {
                        format!("failed removing orphan lease: {}", path.display())
                    });
                }
            }
            reaped.push(record);
        }
        Ok(reaped)
    }
}
