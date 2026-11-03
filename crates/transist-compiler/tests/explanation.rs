// SPDX-License-Identifier: MPL-2.0
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use transist_compiler::explanation::{WorkflowExplanation, compare_explanations, explain_workflow};
use transist_protocol::KernelValue;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/explanation/workflow.yaml")
}
fn known() -> BTreeMap<String, KernelValue> {
    serde_json::from_value(json!({"repo": "/declared/repository", "host": "review.example", "token_ref": "TOKEN", "ticket": "ISSUE-42", "payload": {"items": [1, true, null]}})).unwrap()
}
fn explain(path: &Path) -> WorkflowExplanation {
    explain_workflow(path, None, &known()).unwrap()
}
fn value(explanation: &WorkflowExplanation) -> Value {
    serde_json::to_value(explanation).unwrap()
}

#[test]
fn compiled_explanation_matches_golden_and_repeated_bytes() {
    let first = explain(&fixture());
    let bytes = serde_json::to_string_pretty(&value(&first)).unwrap() + "\n";
    assert_eq!(
        bytes,
        serde_json::to_string_pretty(&value(&explain(&fixture()))).unwrap() + "\n"
    );
    let golden = explain_workflow(
        &fixture().with_file_name("golden.yaml"),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&value(&golden)).unwrap() + "\n",
        include_str!("fixtures/explanation/explanation.golden.json")
    );
}

#[test]
fn compiled_explanation_has_exact_dataflow_documents_control_and_lexical_assets() {
    let explanation = value(&explain(&fixture()));
    let consume = &explanation["steps"]["root/steps/consume"];
    assert_eq!(consume["action"], "process:fixture/review");
    assert_eq!(consume["pin"], "fixture@0.1.0");
    assert_eq!(consume["contract_status"], "unresolved_missing");
    assert_eq!(consume["contract"], Value::Null);
    assert_eq!(consume["bound_bindings"]["approval_count"], 2);
    for selector in [
        "inputs.approvals",
        "steps.produce.out.answer",
        "vars.copied",
        "inputs.token_ref",
        "inputs.ticket",
    ] {
        assert!(
            consume["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dep| dep["selector"] == selector),
            "{selector}"
        );
    }
    assert_eq!(consume["documents"][0]["binding"]["reviewed_by"], "review");
    assert_eq!(consume["documents"][1]["binding"]["access"], "update");
    assert_eq!(
        explanation["steps"]["root/steps/review"]["gate"]["review_documents"],
        json!(["issue"])
    );
    assert_eq!(
        explanation["steps"]["root/steps/repeat"]["loop_limits"]["max_iters"],
        3
    );
    assert_eq!(
        explanation["steps"]["root/steps/choose"]["branch"]["then"],
        json!([{"ref": "middle"}])
    );
    assert_eq!(
        explanation["steps"]["root/cleanup/tidy"]["cleanup_when"],
        json!(["success", "canceled", "failed"])
    );
    let inner = "root/steps/middle/module/steps/inner/module";
    assert_eq!(
        explanation["scopes"][inner]["source"],
        "modules/nested/inner.yaml"
    );
    assert_eq!(
        explanation["scopes"][inner]["inputs"]["value"]["state"],
        "symbolic"
    );
    assert_eq!(
        explanation["scopes"][inner]["outputs"]["result"],
        "{{steps.call.out.answer}}"
    );
    assert!(explanation["assets"]["modules/nested/helpers/task.sh"]["sha256"].is_string());
    assert!(explanation["assets"]["modules/nested/prompts/task.md"]["sha256"].is_string());
    assert_eq!(
        explanation["steps"]["root/steps/produce"]["bound_bindings"]["payload"],
        json!({"items": [1, true, null]})
    );
    assert!(
        !serde_json::to_string(&explanation)
            .unwrap()
            .contains(env!("CARGO_MANIFEST_DIR"))
    );
}

#[test]
fn unresolved_required_inputs_are_typed_missing_not_fabricated() {
    let explanation = value(&explain_workflow(&fixture(), None, &BTreeMap::new()).unwrap());
    for name in ["repo", "host", "token_ref", "ticket", "payload"] {
        let input = &explanation["scopes"]["root"]["inputs"][name];
        assert_eq!(input["state"], "missing");
        assert_eq!(input["required"], true);
        assert!(input.get("value").is_none());
        assert!(input["type"].is_string());
    }
    assert_eq!(
        explanation["scopes"]["root"]["authority_requests"]["bound"]["workspace"]["path"],
        "{{inputs.repo}}"
    );
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

#[test]
fn comparisons_cover_approval_prompt_document_network_action_pin_and_limit_changes() {
    let before = explain(&fixture());
    for (from, to, expected) in [
        ("default: 2", "default: 3", "/bound_bindings/approval_count"),
        (
            "Review {{inputs.ticket}}",
            "Reconsider {{inputs.ticket}}",
            "/prompt/sha256",
        ),
        (
            "$document: notes, access: update",
            "$document: issue, access: update",
            "/bindings/notes_file/$document",
        ),
        (
            "allow_ports: [443]",
            "allow_ports: [8443]",
            "/authority_requests/declared/workflow_network/allow_ports",
        ),
        (
            "process:fixture/produce",
            "process:fixture/other",
            "/action",
        ),
        ("fixture: 0.1.0", "fixture: 0.1.1", "/pin"),
        ("max_iters: 3", "max_iters: 4", "/loop_limits/max_iters"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        copy_tree(fixture().parent().unwrap(), temp.path());
        let path = temp.path().join("workflow.yaml");
        fs::write(&path, fs::read_to_string(&path).unwrap().replace(from, to)).unwrap();
        let diff = compare_explanations(&before, &explain(&path)).unwrap();
        let category = match expected {
            "/bound_bindings/approval_count" => "gates",
            "/prompt/sha256" => "assets",
            "/bindings/notes_file/$document" => "bindings",
            "/authority_requests/declared/workflow_network/allow_ports" => "authority",
            "/action" => "actions",
            "/pin" => "pins",
            "/loop_limits/max_iters" => "limits",
            _ => unreachable!(),
        };
        assert!(
            diff.changes
                .iter()
                .any(|change| change.path.ends_with(expected) && change.category == category),
            "missing {expected}: {diff:?}"
        );
        assert!(
            diff.changes
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
        );
    }
    assert!(
        compare_explanations(&before, &before)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn detached_asset_changes_are_reported_at_exact_module_paths() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(fixture().parent().unwrap(), temp.path());
    let path = temp.path().join("workflow.yaml");
    let before = explain(&path);
    fs::write(
        temp.path().join("modules/nested/prompts/task.md"),
        "changed opaque prompt",
    )
    .unwrap();
    fs::write(
        temp.path().join("modules/nested/helpers/task.sh"),
        "opaque helper change",
    )
    .unwrap();
    let diff = compare_explanations(&before, &explain(&path)).unwrap();
    for asset in [
        "modules~1nested~1prompts~1task.md",
        "modules~1nested~1helpers~1task.sh",
    ] {
        assert!(
            diff.changes
                .iter()
                .any(|change| change.path == format!("/assets/{asset}/sha256")
                    && change.category == "assets")
        );
    }
}

#[test]
fn host_relocation_and_lexical_module_paths_preserve_labels() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(fixture().parent().unwrap(), temp.path());
    let path = temp.path().join("workflow.yaml");
    assert_eq!(value(&explain(&fixture())), value(&explain(&path)));
    let before = explain(&path);
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("./modules/", "modules/"),
    )
    .unwrap();
    let after = explain(&path);
    assert_eq!(value(&before)["steps"], value(&after)["steps"]);
    assert_eq!(value(&before)["scopes"], value(&after)["scopes"]);
}

#[test]
fn input_template_shaped_data_is_not_reinterpreted_in_root_or_nested_scopes() {
    let mut inputs = known();
    inputs.insert(
        "ticket".into(),
        KernelValue::String("{{steps.not_real.out.secret}}".into()),
    );
    let view = value(&explain_workflow(&fixture(), None, &inputs).unwrap());
    assert_eq!(
        view["scopes"]["root"]["inputs"]["ticket"]["state"],
        "provided"
    );
    assert_eq!(
        view["steps"]["root/steps/produce"]["bound_bindings"]["label"],
        "Ticket {{steps.not_real.out.secret}}"
    );
    assert_eq!(
        view["scopes"]["root/steps/middle/module"]["inputs"]["ticket"]["state"],
        "provided"
    );
}

#[test]
fn symbolic_explanation_does_not_relax_execution_compilation() {
    let snapshot = transist_compiler::capture_workflow_definition(&fixture()).unwrap();
    assert!(
        transist_compiler::compile_workflow_snapshot(&snapshot)
            .unwrap_err()
            .to_string()
            .contains("requires plugin-aware compilation")
    );
    assert!(transist_compiler::explanation::explain_snapshot(&snapshot, None, &known()).is_ok());
}

#[test]
fn birth_known_bindings_preserve_runtime_array_coercion_and_reject_structured_interpolation() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(fixture().parent().unwrap(), temp.path());
    let path = temp.path().join("workflow.yaml");
    let source = fs::read_to_string(&path).unwrap().replace(
        "      label: '{{context.label}}'",
        "      label: '{{context.label}}'\n      args: ['{{inputs.approvals}}', {nested: '{{inputs.approvals}}'}]",
    );
    fs::write(&path, &source).unwrap();
    let explanation = value(&explain(&path));
    assert_eq!(
        explanation["steps"]["root/steps/produce"]["bound_bindings"]["args"],
        json!(["2", {"nested": "2"}])
    );
    fs::write(
        &path,
        source.replace(
            "payload: '{{inputs.payload}}'",
            "payload: '{{ inputs.payload }}'",
        ),
    )
    .unwrap();
    let error = explain_workflow(&path, None, &known()).unwrap_err();
    assert_eq!(
        error.root_cause().to_string(),
        "structured output can only be used as a whole-value reference"
    );
}
