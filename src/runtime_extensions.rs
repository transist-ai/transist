// SPDX-License-Identifier: MPL-2.0
//! Extension hooks for runtime service composition.
//!
//! The runtime owns deterministic local defaults. Overlay crates may install
//! process-wide hooks before the first kernel is built to wrap policy/secrets
//! without creating a dependency from the public runtime back to private code.

use anyhow::{Result, anyhow};
use std::sync::{Arc, OnceLock};
use transist_core::RuntimeOwnerIdentity;
use transist_core::ports::{PolicyPort, SecretProviderPort};

/// Inputs available to a runtime service overlay during composition.
pub struct RuntimeServiceOverlayInput {
    /// Resolved process owner identity for the current runtime process.
    pub owner: RuntimeOwnerIdentity,
    /// Current policy port to wrap or replace.
    pub policy: Arc<dyn PolicyPort>,
    /// Current secret provider to wrap or replace.
    pub secrets: Arc<dyn SecretProviderPort>,
}

/// Outputs returned by a runtime service overlay.
pub struct RuntimeServiceOverlayOutput {
    /// Policy port after overlay application.
    pub policy: Arc<dyn PolicyPort>,
    /// Secret provider after overlay application.
    pub secrets: Arc<dyn SecretProviderPort>,
}

/// Process-wide overlay for runtime policy and secret-provider composition.
pub trait RuntimeServiceOverlay: Send + Sync {
    /// Applies the overlay to the current service composition.
    fn apply(&self, input: RuntimeServiceOverlayInput) -> Result<RuntimeServiceOverlayOutput>;
}

/// Installs a runtime service overlay for this process.
pub fn set_runtime_service_overlay(overlay: Arc<dyn RuntimeServiceOverlay>) -> Result<()> {
    SERVICE_OVERLAY
        .set(overlay)
        .map_err(|_| anyhow!("runtime service overlay is already configured"))
}

pub(crate) fn apply_runtime_service_overlay(
    input: RuntimeServiceOverlayInput,
) -> Result<RuntimeServiceOverlayOutput> {
    if let Some(overlay) = SERVICE_OVERLAY.get() {
        overlay.apply(input)
    } else {
        Ok(RuntimeServiceOverlayOutput {
            policy: input.policy,
            secrets: input.secrets,
        })
    }
}

static SERVICE_OVERLAY: OnceLock<Arc<dyn RuntimeServiceOverlay>> = OnceLock::new();
