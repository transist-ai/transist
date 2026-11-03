// SPDX-License-Identifier: MPL-2.0
use transist_protocol::{
    APP_INVOKE_PROGRESS_TYPE_V1, APP_INVOKE_REQUEST_TYPE_V1, APP_PROTOCOL_VERSION_V1,
    AppInvokeProgress, AppInvokeRequest, AppInvokeResult, AppInvokeStep, AppPluginIdentity,
    InvokePaths, KernelValue, RecipeSkillBundle, RecipeSkillDocument, RecipeSkillRef,
    RecipeSkillRoot, RecipeSkillsBinding, jsonl_decode_line, jsonl_encode, normalize_metrics,
};

#[test]
fn protocol_helpers_roundtrip_values() {
    let value = serde_json::json!({"ok": true, "metrics": {"tokens": 12}});
    let line = jsonl_encode(&value).expect("encode");
    let decoded = jsonl_decode_line(&line).expect("decode");
    assert_eq!(decoded, value);
}

#[test]
fn kernel_value_conversion_preserves_structure() {
    let src = serde_json::json!({"arr": [1, 2], "nested": {"flag": true}});
    let kernel = KernelValue::from_json(src.clone());
    let back: serde_json::Value = kernel.into();
    assert_eq!(back, src);
}

#[test]
fn normalize_metrics_flattens_nested_numbers() {
    let metrics = normalize_metrics(&serde_json::json!({"usage": {"in": 7, "out": 3}}));
    assert_eq!(metrics.get("usage.in"), Some(&7.0));
    assert_eq!(metrics.get("usage.out"), Some(&3.0));
}

#[test]
fn skills_binding_roundtrips_through_json() {
    let binding = RecipeSkillsBinding {
        bundle_field: "skills_bundle".to_string(),
        rendered_field: "skills_rendered".to_string(),
        bundle: RecipeSkillBundle {
            version: "v1".to_string(),
            authored_refs: vec![RecipeSkillRef {
                raw: "./skills/authoring".to_string(),
                resolved_path: "/tmp/skills/authoring".to_string(),
                resolution_kind: "skill_root".to_string(),
            }],
            roots: vec![RecipeSkillRoot {
                skill_name: "authoring".to_string(),
                skill_root: "/tmp/skills/authoring".to_string(),
                skill_file: "/tmp/skills/authoring/SKILL.md".to_string(),
                documents: vec![RecipeSkillDocument {
                    logical_path: "SKILL.md".to_string(),
                    resolved_path: "/tmp/skills/authoring/SKILL.md".to_string(),
                    content: "# Skill\n".to_string(),
                }],
            }],
        },
        rendered: "# Workflow Skills\n".to_string(),
    };

    let value = serde_json::to_value(&binding).expect("serialize");
    let decoded: RecipeSkillsBinding = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded, binding);
}

#[test]
fn app_invoke_result_keeps_valid_numeric_metrics_without_validation_errors() {
    let result: AppInvokeResult = serde_json::from_value(serde_json::json!({
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {
            "latency_ms": 12.0,
            "tokens": 7
        },
        "usage": null,
        "error": null
    }))
    .expect("decode");
    assert_eq!(result.metrics_validation_errors, 0);
    assert_eq!(result.metrics.get("latency_ms"), Some(&12.0));
    assert_eq!(result.metrics.get("tokens"), Some(&7.0));
}

#[test]
fn app_invoke_result_counts_one_invalid_metric_entry() {
    let result: AppInvokeResult = serde_json::from_value(serde_json::json!({
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {
            "latency_ms": 12.0,
            "bad": "oops"
        },
        "usage": null,
        "error": null
    }))
    .expect("decode");
    assert_eq!(result.metrics_validation_errors, 1);
    assert_eq!(result.metrics.get("latency_ms"), Some(&12.0));
    assert!(!result.metrics.contains_key("bad"));
}

#[test]
fn app_invoke_result_counts_multiple_invalid_metric_entries() {
    let result: AppInvokeResult = serde_json::from_value(serde_json::json!({
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {
            "latency_ms": 12.0,
            "bad": "oops",
            "nested": { "nope": true },
            "arr": [1, 2, 3]
        },
        "usage": null,
        "error": null
    }))
    .expect("decode");
    assert_eq!(result.metrics_validation_errors, 3);
    assert_eq!(result.metrics.get("latency_ms"), Some(&12.0));
    assert!(!result.metrics.contains_key("bad"));
    assert!(!result.metrics.contains_key("nested"));
    assert!(!result.metrics.contains_key("arr"));
}

#[test]
fn app_invoke_result_counts_invalid_metrics_container_as_one_validation_error() {
    let result: AppInvokeResult = serde_json::from_value(serde_json::json!({
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": ["bad"],
        "usage": null,
        "error": null
    }))
    .expect("decode");
    assert_eq!(result.metrics_validation_errors, 1);
    assert!(result.metrics.is_empty());
}

#[test]
fn app_invoke_progress_defaults_delta_lines_to_one() {
    let progress: AppInvokeProgress = serde_json::from_value(serde_json::json!({
        "type": APP_INVOKE_PROGRESS_TYPE_V1,
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "stream": "stdout"
    }))
    .expect("decode progress");
    assert_eq!(progress.delta_lines, 1);
    assert_eq!(progress.stream, "stdout");
}

#[test]
fn app_invoke_progress_accepts_optional_line_preview() {
    let progress: AppInvokeProgress = serde_json::from_value(serde_json::json!({
        "type": APP_INVOKE_PROGRESS_TYPE_V1,
        "protocol_version": "app.v1",
        "invoke_id": "invoke_1",
        "stream": "stdout",
        "delta_lines": 1,
        "line_preview": "planning step output"
    }))
    .expect("decode progress");
    assert_eq!(
        progress.line_preview.as_deref(),
        Some("planning step output")
    );
}

#[test]
fn app_invoke_request_roundtrips_policy_metadata() {
    let request = AppInvokeRequest {
        message_type: APP_INVOKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        run_id: "run_1".to_string(),
        workflow_id: "workflow_1".to_string(),
        step: AppInvokeStep {
            id: "step_1".to_string(),
            execution: 1,
        },
        plugin: AppPluginIdentity {
            id: "shell".to_string(),
            version: "0.1.0".to_string(),
        },
        action: "exec".to_string(),
        input: serde_json::json!({"command": "echo hi"}),
        paths: InvokePaths {
            run_root: "/tmp/run".to_string(),
            artifacts_dir: "/tmp/run/artifacts".to_string(),
            evidence_dir: "/tmp/run/evidence".to_string(),
            workspace_dir: "/tmp/workspace".to_string(),
        },
        requested_capabilities: vec![
            transist_protocol::CapabilityRequest::new(
                transist_protocol::CapabilityName::Action,
                "shell@0.1.0/exec",
                ["invoke"],
            )
            .expect("capability"),
        ],
        policy: serde_json::json!({
            "filesystem": {
                "source": "step",
                "workspace_root": "/tmp/workspace",
                "workspace_access": {
                    "mode": "read_only"
                },
                "read_allow_paths": ["/etc/gitconfig"],
                "write_allow_paths": ["~/.cache/transist"]
            },
            "network": {
                "source": "workflow",
                "mode": "allowlist",
                "allow_hosts": ["api.example.com"],
                "allow_ports": [443],
                "tls_required": true,
                "max_redirects": 5
            }
        }),
        timeout_ms: Some(1_000),
        max_timeout_ms: Some(2_000),
        metadata: serde_json::json!({"plugin_id": "shell"}),
    };

    let decoded: AppInvokeRequest =
        serde_json::from_value(serde_json::to_value(&request).expect("serialize"))
            .expect("deserialize");
    assert_eq!(decoded.policy["filesystem"]["source"], "step");
    assert_eq!(
        decoded.policy["filesystem"]["write_allow_paths"][0],
        "~/.cache/transist"
    );
    assert_eq!(decoded.policy["network"]["mode"], "allowlist");
    assert_eq!(
        decoded.policy["network"]["allow_hosts"][0],
        "api.example.com"
    );
    assert_eq!(decoded.step.execution, 1);
    assert_eq!(decoded.plugin.id, "shell");
    assert_eq!(decoded.protocol_version, APP_PROTOCOL_VERSION_V1);
    assert_eq!(decoded.message_type, APP_INVOKE_REQUEST_TYPE_V1);
    assert_eq!(decoded.input["command"], "echo hi");
    assert_eq!(decoded.run_id, "run_1");
    assert_eq!(decoded.workflow_id, "workflow_1");
    assert_eq!(decoded.timeout_ms, Some(1_000));
    assert_eq!(decoded.max_timeout_ms, Some(2_000));
    assert_eq!(decoded.metadata["plugin_id"], "shell");
    assert_eq!(decoded.step.id, "step_1");
    assert_eq!(decoded.plugin.version, "0.1.0");
    assert_eq!(decoded.plugin.id, "shell");
    assert_eq!(decoded.paths.workspace_dir, "/tmp/workspace");
    assert_eq!(decoded.requested_capabilities.len(), 1);
    assert_eq!(
        decoded.requested_capabilities[0].capability,
        transist_protocol::CapabilityName::Action
    );
}
