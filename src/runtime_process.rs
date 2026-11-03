// SPDX-License-Identifier: MPL-2.0
use anyhow::{Result, anyhow};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use transist_core::{ManagementMode, RuntimeOwnerIdentity};
use uuid::Uuid;

/// Process-level runtime-management overrides applied before the first kernel is built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProcessOverrides {
    /// Optional explicit management mode.
    pub management_mode: Option<ManagementMode>,
    /// Optional explicit owner id.
    pub owner_id: Option<String>,
    /// Optional explicit external host id.
    pub host_id: Option<String>,
    /// Optional explicit external runner id.
    pub runner_id: Option<String>,
    /// Optional explicit external run id.
    pub external_run_id: Option<String>,
}

/// Resolver used by overlay crates to provide non-standalone process identity.
pub trait RuntimeProcessResolver: Send + Sync {
    /// Resolves the runtime owner identity for this process.
    fn resolve(&self, overrides: RuntimeProcessOverrides) -> Result<RuntimeOwnerIdentity>;

    /// Returns environment variables that detached local runners must inherit.
    fn detached_runner_env(&self, _owner: &RuntimeOwnerIdentity) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
}

/// Installs the runtime process resolver for this process.
pub fn set_runtime_process_resolver(resolver: Arc<dyn RuntimeProcessResolver>) -> Result<()> {
    let mut state = lock_runtime_process_state()?;
    if state.owner.is_some() {
        return Err(anyhow!(
            "runtime process identity is already initialized; configure the resolver before runtime use"
        ));
    }
    if state.resolver.is_some() {
        return Err(anyhow!("runtime process resolver is already configured"));
    }
    state.resolver = Some(resolver);
    Ok(())
}

/// Resolves and stores the process-level runtime owner identity for subsequent kernel construction.
pub fn configure_runtime_process(
    overrides: RuntimeProcessOverrides,
) -> Result<RuntimeOwnerIdentity> {
    let mut state = lock_runtime_process_state()?;
    if let Some(owner) = state.owner.as_ref() {
        return Err(anyhow!(
            "runtime process identity is already initialized as '{}'",
            owner.owner_id
        ));
    }
    let owner = resolve_runtime_process(state.resolver.as_ref(), overrides)?;
    state.owner = Some(owner.clone());
    Ok(owner)
}

/// Returns the resolved process-level runtime owner identity, initializing it from env/defaults on first use.
pub fn runtime_owner_identity() -> Result<RuntimeOwnerIdentity> {
    let mut state = lock_runtime_process_state()?;
    if let Some(owner) = state.owner.as_ref().cloned() {
        return Ok(owner);
    }
    let owner =
        resolve_runtime_process(state.resolver.as_ref(), RuntimeProcessOverrides::default())?;
    state.owner = Some(owner.clone());
    Ok(owner)
}

/// Returns the active process-level management mode.
pub fn runtime_management_mode() -> Result<ManagementMode> {
    Ok(runtime_owner_identity()?.management_mode)
}

#[cfg(test)]
pub(crate) fn clear_runtime_process_for_tests() {
    let mut state = lock_runtime_process_state().expect("runtime process state");
    state.owner = None;
}

/// Returns extra runtime env vars that detached runners must inherit.
pub fn detached_runner_management_env() -> Result<Vec<(String, String)>> {
    let owner = runtime_owner_identity()?;
    let state = lock_runtime_process_state()?;
    match state.resolver.as_ref() {
        Some(resolver) => resolver.detached_runner_env(&owner),
        None => Ok(Vec::new()),
    }
}

#[derive(Default)]
struct RuntimeProcessState {
    resolver: Option<Arc<dyn RuntimeProcessResolver>>,
    owner: Option<RuntimeOwnerIdentity>,
}

fn runtime_process_state() -> &'static Mutex<RuntimeProcessState> {
    static STORE: OnceLock<Mutex<RuntimeProcessState>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(RuntimeProcessState::default()))
}

fn lock_runtime_process_state() -> Result<MutexGuard<'static, RuntimeProcessState>> {
    let state = runtime_process_state();
    match state.lock() {
        Ok(guard) => Ok(guard),
        Err(poisoned) => {
            drop(poisoned.into_inner());
            Err(anyhow!("runtime process state is poisoned"))
        }
    }
}

fn resolve_runtime_process(
    resolver: Option<&Arc<dyn RuntimeProcessResolver>>,
    overrides: RuntimeProcessOverrides,
) -> Result<RuntimeOwnerIdentity> {
    if let Some(resolver) = resolver {
        return resolver.resolve(overrides);
    }
    Ok(RuntimeOwnerIdentity {
        management_mode: ManagementMode::Standalone,
        owner_id: Uuid::new_v4().to_string(),
        lease_id: Uuid::new_v4().to_string(),
        pid: std::process::id(),
        hostname: hostname(),
        command: std::env::args().collect::<Vec<_>>().join(" "),
        transist_version: env!("CARGO_PKG_VERSION").to_string(),
        external: None,
        runtime_id: None,
        organization_id: None,
    })
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_runner_management_env_is_empty_for_standalone() {
        clear_runtime_process_for_tests();
        let env = detached_runner_management_env().expect("standalone env");
        assert!(env.is_empty());
        assert_eq!(
            runtime_management_mode().unwrap(),
            ManagementMode::Standalone
        );
    }
}
