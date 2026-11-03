// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use transist::{
    RuntimeOperatingMode, RuntimeServiceOverlay, RuntimeServiceOverlayInput,
    RuntimeServiceOverlayOutput, TransistContext, ValidationStatus, init_home_dir_with_mode,
    set_runtime_service_overlay,
};

struct PermissivePolicyOverlay;

static OVERLAY_APPLIED: AtomicBool = AtomicBool::new(false);

impl RuntimeServiceOverlay for PermissivePolicyOverlay {
    fn apply(
        &self,
        input: RuntimeServiceOverlayInput,
    ) -> anyhow::Result<RuntimeServiceOverlayOutput> {
        OVERLAY_APPLIED.store(true, Ordering::SeqCst);
        Ok(RuntimeServiceOverlayOutput {
            policy: Arc::new(transist_policy::StaticPolicy::permissive()),
            secrets: input.secrets,
        })
    }
}

#[test]
fn bound_validation_applies_runtime_service_policy_overlay_before_mode_wrappers() {
    set_runtime_service_overlay(Arc::new(PermissivePolicyOverlay)).expect("install policy overlay");
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".agents/testing/fixtures/plugins/remote.echo/plugin.yaml");
    context
        .install_plugin_file(&plugin)
        .expect("install remote plugin");
    let workflow = temp.path().join("overlay.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: overlay.demo\nversion: v1\nplugins:\n  remote.echo: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: remote\n    uses: process:remote.echo/echo\n    with:\n      message: static\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("overlay.demo@v1", BTreeMap::new(), false, true)
        .expect("validation");

    assert!(OVERLAY_APPLIED.load(Ordering::SeqCst));
    assert!(validation.checks.iter().any(|check| {
        check.code == "local_only_denied_external"
            && check.status == ValidationStatus::Failed
            && check.step_path.as_deref() == Some("remote")
    }));
}
