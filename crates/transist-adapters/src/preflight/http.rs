// SPDX-License-Identifier: MPL-2.0
//! HTTP-adapter preflight probes.

use super::super::*;
use super::preflight_group_check;

pub(crate) fn run_http_preflight_probe(group: &PreflightProbeGroup) -> Result<Vec<PreflightCheck>> {
    let target = &group.target;
    let request = &target.request;
    let PluginEntrypoint::Http { base_url, ca_cert } = &target.manifest.entrypoint else {
        return Ok(vec![preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Failed,
            format!(
                "http plugin '{}' has invalid entrypoint",
                target.manifest.id
            ),
        )]);
    };
    let ca_cert = ca_cert.as_ref().map(|path| target.plugin_dir.join(path));
    let network_policy = match network_policy_from_request(request) {
        Ok(policy) => policy.unwrap_or_default(),
        Err(error) => {
            return Ok(vec![preflight_group_check(
                group,
                "adapter_handshake",
                PreflightStatus::Failed,
                error,
            )]);
        }
    };
    let http_policy = HttpRequestPolicy::from_action(network_policy, &target.action);
    transist_core::record_preflight_probe_batch(AdapterKind::Http, 0, 1);
    if let Err(error) = perform_http_handshake(
        base_url,
        ca_cert.as_deref(),
        request,
        &target.manifest,
        &target.action,
        &http_policy,
    ) {
        return Ok(vec![preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Failed,
            error.to_string(),
        )]);
    }

    let mut checks = vec![preflight_group_check(
        group,
        "adapter_handshake",
        PreflightStatus::Passed,
        "HTTP APP handshake succeeded",
    )];
    if target.manifest.capabilities.supports_health {
        transist_core::record_preflight_probe_batch(AdapterKind::Http, 0, 1);
        match perform_http_health(
            base_url,
            ca_cert.as_deref(),
            &[],
            request.timeout_ms,
            &http_policy,
        ) {
            Ok(()) => checks.push(preflight_group_check(
                group,
                "adapter_health",
                PreflightStatus::Passed,
                "HTTP health probe succeeded",
            )),
            Err(error) => checks.push(preflight_group_check(
                group,
                "adapter_health",
                PreflightStatus::Failed,
                error.to_string(),
            )),
        }
    } else {
        checks.push(preflight_group_check(
            group,
            "adapter_health",
            PreflightStatus::Warning,
            "HTTP plugin does not advertise health support; connectivity coverage is partial",
        ));
    }
    Ok(checks)
}
