// SPDX-License-Identifier: MPL-2.0
//! Default local runtime composition.
//!
//! The functions in this module wire together the built-in adapters, local storage,
//! policy loading, clocks, ids, and secret providers used by the library's
//! top-level application APIs.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use transist_adapters::AdapterRouter;
use transist_core::ports::{PolicyPort, RuntimeOperatingMode, SecretProviderPort};
use transist_core::{
    NoopTelemetry, RuntimeKernel, RuntimeServiceParts, RuntimeServices, TrustDecisionRecord,
};
use transist_storage_local::{
    EnvSecretProvider, LocalArtifactStore, LocalClock, LocalControlRequestStore, LocalEventSink,
    LocalIdGenerator, LocalRunStore, LocalRunSummaryStore, LocalStorageConfig, shared_lease_store,
};

mod home_settings;
mod observed;
mod observers;
mod plugin_metadata;
mod plugin_verifier;
mod prompt_files;
pub(crate) mod router_cache;
pub(crate) use home_settings::LocalHomeSettings;
pub(crate) use plugin_metadata::LocalPluginMetadata;
pub(crate) use prompt_files::LocalPromptFiles;

pub(crate) fn runtime_policy_and_secrets_with_modes(
    home_dir: &Path,
    owner: transist_core::RuntimeOwnerIdentity,
    trust_decision: Option<TrustDecisionRecord>,
    local_only: bool,
) -> Result<(Arc<dyn PolicyPort>, Arc<dyn SecretProviderPort>)> {
    let policy: Arc<dyn PolicyPort> = crate::runtime_policy::load_policy(home_dir)?;
    let secrets: Arc<dyn SecretProviderPort> = Arc::new(EnvSecretProvider);
    let overlay = crate::runtime_extensions::apply_runtime_service_overlay(
        crate::runtime_extensions::RuntimeServiceOverlayInput {
            owner,
            policy,
            secrets,
        },
    )?;
    let policy = crate::runtime_policy::wrap_policy_for_modes(
        home_dir,
        overlay.policy,
        trust_decision,
        local_only,
    )?;
    Ok((policy, overlay.secrets))
}

pub(crate) fn runtime_policy_with_modes(
    home_dir: &Path,
    trust_decision: Option<TrustDecisionRecord>,
    local_only: bool,
) -> Result<Arc<dyn PolicyPort>> {
    runtime_policy_and_secrets_with_modes(
        home_dir,
        crate::runtime_owner_identity()?,
        trust_decision,
        local_only,
    )
    .map(|(policy, _)| policy)
}

/// Builds default local runtime services with optional trusted-execution and local-only modes.
pub(crate) fn default_runtime_services_with_modes(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &router_cache::RouterCache,
    trust_decision: Option<TrustDecisionRecord>,
    local_only: bool,
) -> Result<RuntimeServices> {
    default_runtime_services_with_router(
        repo_root,
        home_dir,
        router_cache.get_or_insert(home_dir)?,
        trust_decision,
        local_only,
    )
}

/// Builds default local runtime services with a caller-supplied adapter router.
pub(crate) fn default_runtime_services_with_router(
    _repo_root: &Path,
    home_dir: &Path,
    router: Arc<AdapterRouter>,
    trust_decision: Option<TrustDecisionRecord>,
    local_only: bool,
) -> Result<RuntimeServices> {
    let storage_root = home_dir.to_path_buf();
    let config = LocalStorageConfig::new(storage_root.clone());
    let owner = crate::runtime_owner_identity()?;
    let posture = crate::runtime_policy::load_posture(home_dir)?;
    let (policy, secrets) =
        runtime_policy_and_secrets_with_modes(home_dir, owner.clone(), trust_decision, local_only)?;
    if matches!(posture.operating_mode, RuntimeOperatingMode::Development) {
        tracing::warn!("runtime operating mode is development; do not use this mode in production");
    }
    let summary_store = LocalRunSummaryStore::new(home_dir);
    let metrics_query = Arc::new(crate::application::metrics::LocalMetricsQuery::new(
        home_dir,
    ));

    let run_observers: Vec<Arc<dyn transist_core::ports::RunWriteObserver>> = vec![
        Arc::new(observers::SummaryObserver::new(summary_store.clone())),
        Arc::new(observers::MetricsObserver::new(metrics_query.clone())),
    ];
    let event_observers: Vec<Arc<dyn transist_core::ports::EventObserver>> = vec![
        Arc::new(observers::SummaryObserver::new(summary_store.clone())),
        Arc::new(observers::MetricsObserver::new(metrics_query)),
    ];

    Ok(RuntimeServices::new(RuntimeServiceParts {
        execution: router.clone(),
        artifacts: Arc::new(LocalArtifactStore::new(config.clone())),
        run_store: Arc::new(observed::ObservedRunStore::new(
            Arc::new(LocalRunStore::new(config.clone())),
            run_observers,
        )),
        lease: shared_lease_store(storage_root, owner.clone()),
        control: Arc::new(LocalControlRequestStore::new(home_dir, Some(owner))),
        clock: Arc::new(LocalClock),
        ids: Arc::new(LocalIdGenerator),
        events: Arc::new(observed::ObservedEventSink::new(
            Arc::new(LocalEventSink::new(config)),
            event_observers,
        )),
        policy,
        preflight: router,
        secrets,
        plugin_verifier: Arc::new(plugin_verifier::LocalPluginVerifier::new()),
        plugin_metadata: Arc::new(plugin_metadata::LocalPluginMetadata::new(
            home_dir.to_path_buf(),
        )),
        prompt_files: Arc::new(prompt_files::LocalPromptFiles::new()),
        home_settings: Arc::new(home_settings::LocalHomeSettings::new()),
        telemetry: Arc::new(NoopTelemetry),
    }))
}

/// Builds the default runtime kernel wired with local adapters, storage, and policy services.
pub(crate) fn default_runtime_kernel(
    router_cache: &router_cache::RouterCache,
    repo_root: &Path,
    home_dir: &Path,
) -> Result<RuntimeKernel> {
    default_runtime_kernel_with_trust_decision(router_cache, repo_root, home_dir, None)
}

/// Builds the default runtime kernel with an optional trusted-execution decision.
pub(crate) fn default_runtime_kernel_with_trust_decision(
    router_cache: &router_cache::RouterCache,
    repo_root: &Path,
    home_dir: &Path,
    trust_decision: Option<TrustDecisionRecord>,
) -> Result<RuntimeKernel> {
    default_runtime_kernel_with_modes(router_cache, repo_root, home_dir, trust_decision, false)
}

/// Builds the default runtime kernel with optional trusted-execution and local-only modes.
pub(crate) fn default_runtime_kernel_with_modes(
    router_cache: &router_cache::RouterCache,
    repo_root: &Path,
    home_dir: &Path,
    trust_decision: Option<TrustDecisionRecord>,
    local_only: bool,
) -> Result<RuntimeKernel> {
    let posture = crate::runtime_policy::load_posture(home_dir)?;
    Ok(RuntimeKernel::new_with_posture(
        default_runtime_services_with_modes(
            repo_root,
            home_dir,
            router_cache,
            trust_decision,
            local_only,
        )?,
        posture,
    ))
}
