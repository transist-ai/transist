// SPDX-License-Identifier: MPL-2.0
//! Plugin loading helpers shared by adapters and preflight probes.

use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};
use transist_core::ports::ExecutionInvocation;
use transist_plugins::{
    ActionDescriptor, AdapterKind, PluginManifest, PluginVersionSelector, resolve_plugin_in_home,
    resolve_plugin_with_selector_in_home,
};
use transist_protocol::ExecutionRequest;

use crate::paths::home_dir_from_run_root;

pub(crate) fn load_plugin(
    invocation: &ExecutionInvocation,
    expected_adapter: AdapterKind,
) -> Result<(PluginManifest, PathBuf, ActionDescriptor)> {
    load_plugin_from_request(&invocation.request, expected_adapter)
}

pub(crate) fn load_plugin_from_request(
    request: &ExecutionRequest,
    expected_adapter: AdapterKind,
) -> Result<(PluginManifest, PathBuf, ActionDescriptor)> {
    let plugin_id = request
        .metadata
        .get("plugin_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("request metadata.plugin_id is required"))?;
    let home_dir = home_dir_from_run_root(Path::new(&request.paths.run_root))?;

    // When the recipe pinned a specific plugin version, load it directly so we
    // bypass the "multiple installed versions" ambiguity error in the default
    // resolver and guarantee the exact version compiled into the recipe is used.
    let (manifest, plugin_dir) = if let Some(version) = request
        .metadata
        .get("plugin_version")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        let selector = PluginVersionSelector::Exact(version.to_string());
        resolve_plugin_with_selector_in_home(&home_dir, plugin_id, &selector)?
    } else {
        resolve_plugin_in_home(&home_dir, plugin_id)?
    };

    if manifest.adapter != expected_adapter {
        bail!(
            "plugin '{}' uses adapter '{}' but step requested '{}'",
            manifest.id,
            manifest.adapter.as_str(),
            request.adapter_kind
        );
    }
    let action = manifest
        .actions
        .iter()
        .find(|candidate| candidate.name == request.action)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "plugin '{}' does not support action '{}'",
                manifest.id,
                request.action
            )
        })?;
    Ok((manifest, plugin_dir, action))
}
