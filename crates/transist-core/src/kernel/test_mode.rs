// SPDX-License-Identifier: MPL-2.0
//! Reserved test-workflow version helpers.
//!
//! Compiled unconditionally; the helpers degrade at runtime based on the
//! `dev-version-bypass` feature (reserved versions are rejected unless the
//! feature is enabled).

/// Reserved workflow version used to mark non-production test workflows.
pub(crate) const TEST_WORKFLOW_VERSION: &str = "v0";

/// Returns whether the workflow version is reserved for development-only test behavior.
pub(crate) fn is_reserved_test_workflow_version(version: &str) -> bool {
    version == TEST_WORKFLOW_VERSION
}

/// Returns whether the workflow version enables development-only test-workflow behavior.
pub(crate) fn is_test_workflow(version: &str) -> bool {
    cfg!(feature = "dev-version-bypass") && is_reserved_test_workflow_version(version)
}

/// Refuses reserved test workflow versions in builds where the dev bypass is absent.
pub(crate) fn ensure_workflow_version_allowed(
    workflow_id: &str,
    version: &str,
) -> anyhow::Result<()> {
    #[cfg(feature = "dev-version-bypass")]
    let _ = (workflow_id, version);

    #[cfg(not(feature = "dev-version-bypass"))]
    if is_reserved_test_workflow_version(version) {
        anyhow::bail!(
            "workflow '{}' uses reserved test version '{}'; rebuild with dev-version-bypass to use development-only test workflows",
            workflow_id,
            version
        );
    }
    Ok(())
}
