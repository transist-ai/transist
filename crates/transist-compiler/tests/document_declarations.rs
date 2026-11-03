// SPDX-License-Identifier: MPL-2.0
//! Root-lexical document declarations, deliberately separate from action data.
use transist_compiler::{Flow, compile_flow, load_flow};
use transist_protocol::StepExecutionDescriptor;

fn source(declarations: &str) -> String {
    format!(
        "schema: transist.flow.v1\nid: document-demo\nversion: 0.1.0\n{declarations}\nsteps:\n  - id: finish\n    control: control.noop\n"
    )
}

#[test]
fn document_declarations_are_literal_bounded_and_not_authority_or_deferred_metadata() {
    let flow: Flow = yaml_serde::from_str(&source("documents:\n  file: {kind: file, max_bytes: 32, initial: '{{inputs.secret}}'}\n  tree: {kind: tree, max_bytes: 64, max_files: 3}")).unwrap();
    let recipe = compile_flow(&flow).unwrap();
    assert_eq!(
        recipe.documents["file"].initial.as_deref(),
        Some("{{inputs.secret}}")
    );
    assert_eq!(recipe.documents["tree"].max_files, Some(3));
    assert!(recipe.capabilities.is_empty());
    assert!(recipe.steps[0].with.is_empty());
    assert_eq!(
        yaml_serde::from_str::<transist_protocol::Recipe>(&yaml_serde::to_string(&recipe).unwrap())
            .unwrap(),
        recipe
    );
}

#[test]
fn document_declarations_reject_zero_bounds_wrong_kinds_paths_and_large_initial_bytes() {
    for declaration in [
        "file: {kind: file, max_bytes: 0}",
        "file: {kind: file, max_bytes: -1}",
        "file: {kind: file, max_bytes: 2, initial: 'éé'}",
        "file: {kind: file, max_bytes: 32, max_files: 1}",
        "tree: {kind: tree, max_bytes: 32}",
        "tree: {kind: tree, max_bytes: 32, max_files: 0}",
        "tree: {kind: tree, max_bytes: 32, max_files: 2, initial: text}",
        "file: {kind: directory, max_bytes: 32}",
        "../file: {kind: file, max_bytes: 32}",
        "file: {kind: file, max_bytes: 32, initial_path: '/tmp/external'}",
    ] {
        let parsed = yaml_serde::from_str::<Flow>(&source(&format!("documents:\n  {declaration}")));
        let rejected = match parsed {
            Ok(flow) => compile_flow(&flow).is_err(),
            Err(_) => true,
        };
        assert!(rejected, "{declaration}");
    }
}

#[test]
fn document_declarations_inherit_through_nested_modules_without_shadowing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workflow.yaml");
    let module = tmp.path().join("module.yaml");
    let nested = tmp.path().join("nested.yaml");
    std::fs::write(&root, "schema: transist.flow.v1\nid: document-demo\nversion: 0.1.0\ndocuments:\n  notes: {kind: file, max_bytes: 32, initial: literal}\nmodules:\n  child: {path: module.yaml}\nsteps:\n  - id: call\n    module: child\n").unwrap();
    std::fs::write(&module, "schema: transist.module.v1\nid: module\nversion: 0.1.0\nmodules:\n  nested: {path: nested.yaml}\nsteps:\n  - id: nested\n    module: nested\n").unwrap();
    std::fs::write(&nested, "schema: transist.module.v1\nid: nested\nversion: 0.1.0\nsteps:\n  - id: finish\n    control: control.noop\n").unwrap();
    let recipe = compile_flow(&load_flow(&root).unwrap()).unwrap();
    let StepExecutionDescriptor::Subflow { recipe: child } = &recipe.steps[0].execution else {
        panic!("child");
    };
    let StepExecutionDescriptor::Subflow {
        recipe: nested_recipe,
    } = &child.steps[0].execution
    else {
        panic!("nested");
    };
    assert_eq!(recipe.documents, child.documents);
    assert_eq!(recipe.documents, nested_recipe.documents);
    std::fs::write(&nested, "schema: transist.module.v1\nid: nested\nversion: 0.1.0\ndocuments:\n  notes: {kind: file, max_bytes: 99}\nsteps:\n  - id: finish\n    control: control.noop\n").unwrap();
    assert!(load_flow(&root).is_err());
}

#[test]
fn document_declarations_enable_only_process_action_data_bindings() {
    for with in [
        "secret_refs: {$document: file, access: read}",
        "command: {$document: file, access: read}",
    ] {
        let yaml = format!(
            "schema: transist.flow.v1\nid: demo\nversion: 0.1.0\nplugins: {{shell: 0.1.0}}\ndocuments:\n  file: {{kind: file, max_bytes: 32}}\nsteps:\n  - id: invoke\n    uses: process:shell/echo\n    with:\n      {with}\n"
        );
        let flow: Flow = yaml_serde::from_str(&yaml).unwrap();
        assert!(
            compile_flow(&flow)
                .unwrap_err()
                .to_string()
                .contains("$document")
        );
    }
    let flow: Flow = yaml_serde::from_str("schema: transist.flow.v1\nid: demo\nversion: 0.1.0\nplugins: {shell: 0.1.0}\ndocuments: {file: {kind: file, max_bytes: 32}}\nsteps:\n  - id: invoke\n    uses: process:shell/echo\n    with: {nested: [{value: {$document: file, access: read}}]}\n").unwrap();
    let recipe = compile_flow(&flow).unwrap();
    let bindings = transist_protocol::step_document_bindings(&recipe.steps[0]).unwrap();
    assert_eq!(bindings[0].path, "/nested/0/value");
    assert_eq!(
        bindings[0].binding.access,
        transist_protocol::DocumentAccess::Read
    );
    let reviewed = "schema: transist.flow.v1\nid: demo\nversion: 0.1.0\nplugins: {shell: 0.1.0}\ndocuments: {file: {kind: file, max_bytes: 32}}\nsteps:\n  - id: gate\n    control: control.gate.manual\n    with: {review_documents: [file]}\n  - id: invoke\n    uses: process:shell/echo\n    with: {nested: [{value: {$document: file, access: read, reviewed_by: gate}}]}\n";
    let recipe = compile_flow(&yaml_serde::from_str(reviewed).unwrap()).unwrap();
    let reviewed_binding = transist_protocol::step_document_bindings(&recipe.steps[1]).unwrap();
    assert_eq!(reviewed_binding[0].path, "/nested/0/value");
    assert_eq!(
        reviewed_binding[0].binding.reviewed_by.as_deref(),
        Some("gate")
    );
}

#[test]
fn document_declarations_do_not_conflate_action_documents_data_with_root_storage() {
    let flow: Flow = yaml_serde::from_str("schema: transist.flow.v1\nid: demo\nversion: 0.1.0\nplugins: {shell: 0.1.0}\nsteps:\n  - id: invoke\n    uses: process:shell/echo\n    with:\n      documents: {name: ordinary_action_data}\n      message: '$document is literal text'\n").unwrap();
    let recipe = compile_flow(&flow).unwrap();
    assert!(recipe.documents.is_empty());
    assert_eq!(
        serde_json::to_value(&recipe.steps[0].with["documents"]).unwrap(),
        serde_json::json!({"name":"ordinary_action_data"})
    );
}
