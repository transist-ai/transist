// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use transist::{
    KernelValue, RuntimeOperatingMode, TransistContext, ValidationStatus, init_home_dir_with_mode,
};

fn write_allowlist_config(home: &Path) {
    fs::write(
        home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n",
    )
    .expect("allowlist config");
}

#[test]
fn authored_validation_reports_required_inputs_without_binding_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let workflow = temp.path().join("authored.workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: authored.demo\nversion: v1\ninputs:\n  zebra:\n    type: string\n    required: true\n  alpha:\n    type: string\n    required: true\n  optional:\n    type: string\nsteps:\n  - id: finish\n    control: control.finish\n",
    )
    .expect("workflow");
    let context = TransistContext::with_home(temp.path(), &home);

    let validation = context
        .validate_flow_file(&workflow)
        .expect("authored validation does not bind inputs");

    assert!(validation.is_valid());
    assert_eq!(validation.workflow_ref, "authored.demo@v1");
    assert_eq!(validation.status, ValidationStatus::NotEvaluated);
    assert_eq!(
        validation
            .checks
            .iter()
            .map(|check| (check.code.as_str(), check.status, check.message.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                "authored_definition",
                ValidationStatus::Passed,
                "captured workflow definition passed authored validation",
            ),
            (
                "required_input",
                ValidationStatus::NotEvaluated,
                "required workflow input 'alpha' is not evaluated in authored mode",
            ),
            (
                "required_input",
                ValidationStatus::NotEvaluated,
                "required workflow input 'zebra' is not evaluated in authored mode",
            ),
        ]
    );
}

#[test]
fn authored_validation_rejects_a_missing_nested_asset() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let workflow = temp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: missing.asset\nversion: v1\nmodules:\n  child:\n    path: nested/child.yaml\nsteps:\n  - id: child\n    module: child\n",
    )
    .expect("workflow");

    let error = TransistContext::with_home(temp.path(), &home)
        .validate_flow_file(&workflow)
        .expect_err("missing nested asset must fail authored validation");

    assert!(
        error.to_string().contains("workflow module asset"),
        "{error:#}"
    );
}

#[cfg(unix)]
#[test]
fn authored_validation_rejects_a_symlinked_nested_asset() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    let home = temp.path().join("home");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let workflow = temp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: symlinked.asset\nversion: v1\nmodules:\n  child:\n    path: nested/child.yaml\nsteps:\n  - id: child\n    module: child\n",
    )
    .expect("workflow");
    fs::write(
        outside.path().join("child.yaml"),
        "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    )
    .expect("child");
    std::os::unix::fs::symlink(outside.path(), temp.path().join("nested")).expect("nested symlink");

    let error = TransistContext::with_home(temp.path(), &home)
        .validate_flow_file(&workflow)
        .expect_err("symlinked nested asset must fail authored validation");

    assert!(
        error.to_string().contains("workflow module asset"),
        "{error:#}"
    );
}

#[test]
fn bound_workflow_validation_materializes_installed_actions_without_creating_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml");
    context
        .install_plugin_file(&plugin)
        .expect("install shell plugin");

    let workflow = temp.path().join("static.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: static.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nworkspace:\n  path: {}\n  access: full\nsteps:\n  - id: echo\n    uses: process:shell/echo\n    with:\n      args: [static]\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");
    let runs_before = home.join("runs").exists();

    let validation = context
        .validate_bound_workflow("static.demo@v1", BTreeMap::new(), false, false)
        .expect("static validation");

    assert!(validation.is_valid(), "{:#?}", validation.checks);
    assert_eq!(validation.status, ValidationStatus::NotEvaluated);
    assert_eq!(
        validation.workspace_dir,
        fs::canonicalize(&workspace).unwrap()
    );
    assert!(
        validation
            .checks
            .iter()
            .any(|check| check.code == "input_schema")
    );
    assert!(validation.checks.iter().any(|check| {
        check.code == "external_state" && check.status == ValidationStatus::NotEvaluated
    }));
    assert_eq!(home.join("runs").exists(), runs_before);
    assert!(!home.join("runs/run_static-validation").exists());
}

#[test]
fn bound_validation_marks_secret_values_not_evaluated_without_resolving_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml");
    context
        .install_plugin_file(&plugin)
        .expect("install shell plugin");
    let workflow = temp.path().join("secret.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: secret.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: consume\n    uses: process:shell/exec_with_secret_env\n    with:\n      command: env\n      secret_refs: [MY_API_TOKEN]\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("secret.demo@v1", BTreeMap::new(), false, false)
        .expect("validation must not require the secret value");

    assert!(validation.checks.iter().any(|check| {
        check.code == "secret_value"
            && check.status == ValidationStatus::NotEvaluated
            && check.step_path.as_deref() == Some("consume")
    }));
}

#[test]
fn bound_validation_applies_local_only_policy_to_reachable_external_steps() {
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
    let workflow = temp.path().join("local-only.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: local.only.demo\nversion: v1\nplugins:\n  remote.echo: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: remote\n    uses: process:remote.echo/echo\n    with:\n      message: static\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("local.only.demo@v1", BTreeMap::new(), false, true)
        .expect("validation");

    assert!(validation.checks.iter().any(|check| {
        check.code == "local_only_denied_external"
            && check.status == ValidationStatus::Failed
            && check.step_path.as_deref() == Some("remote")
    }));
}

#[test]
fn governed_bound_validation_enables_trust_without_an_explicit_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let workflow = temp.path().join("governed.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: governed.demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: finish\n    control: control.finish\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");
    fs::write(
        home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n",
    )
    .expect("governed config");

    let error = context
        .validate_bound_workflow("governed.demo@v1", BTreeMap::new(), false, false)
        .expect_err("governed mode must require trusted execution");

    assert!(
        error
            .to_string()
            .contains("trusted execution requires workflows install --trusted-execution --approve")
    );
}

#[test]
fn bound_validation_marks_mutable_variable_bindings_deferred() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml");
    context
        .install_plugin_file(&plugin)
        .expect("install shell plugin");
    let workflow = temp.path().join("mutable.workflow.yaml");
    fs::write(
        &workflow,
        format!(
                "schema: transist.flow.v1\nid: mutable.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: produce\n    uses: process:shell/echo\n    with:\n      args: [seed]\n    set:\n      dynamic: '{{{{steps.produce.out.stdout}}}}'\n  - id: consume\n    uses: process:shell/echo\n    with:\n      args: ['{{{{vars.dynamic}}}}']\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("mutable.demo@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert!(validation.checks.iter().any(|check| {
        check.code == "mutable_variable"
            && check.status == ValidationStatus::DeferredToDispatch
            && check.step_path.as_deref() == Some("consume")
    }));
    assert!(validation.checks.iter().any(|check| {
        check.code == "deferred_to_dispatch"
            && check.status == ValidationStatus::DeferredToDispatch
            && check.step_path.as_deref() == Some("consume")
    }));
    assert!(
        validation
            .checks
            .iter()
            .all(|check| check.code != "step_output")
    );
}

#[test]
fn bound_validation_marks_runtime_dependent_prompt_fields_deferred() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    for plugin in [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/goose.cli/plugin.yaml"),
    ] {
        context
            .install_plugin_file(&plugin)
            .expect("install plugin");
    }

    for (suffix, prompt_field) in [
        ("inline", "prompt: 'Review {{vars.dynamic}}'"),
        ("file", "prompt_file: prompt.md"),
    ] {
        let workflow_id = format!("dynamic.prompt.{suffix}");
        let workflow = temp.path().join(format!("{suffix}.workflow.yaml"));
        fs::write(temp.path().join("prompt.md"), "Static prompt").expect("prompt file");
        fs::write(
            &workflow,
            format!(
                "schema: transist.flow.v1\nid: {workflow_id}\nversion: v1\nplugins:\n  shell: 0.1.0\n  goose.cli: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: produce\n    uses: process:shell/echo\n    with:\n      args: [prompt.md]\n    set:\n      dynamic: '{{{{steps.produce.out.stdout}}}}'\n  - id: consume\n    uses: process:goose.cli/run_task\n    {prompt_field}\n",
                workspace.display()
            ),
        )
        .expect("workflow");
        let recipe_path = context
            .compile_flow_file(&workflow)
            .expect("compile workflow");
        if suffix == "file" {
            let mut recipe: transist::Recipe = yaml_serde::from_str(
                &fs::read_to_string(&recipe_path).expect("read compiled recipe"),
            )
            .expect("parse compiled recipe");
            recipe.steps[1]
                .prompt_binding
                .as_mut()
                .expect("prompt binding")
                .source = transist_protocol::RecipePromptSource::File {
                path: "{{vars.dynamic}}".to_string(),
            };
            fs::write(
                &recipe_path,
                yaml_serde::to_string(&recipe).expect("serialize compiled recipe"),
            )
            .expect("write compiled recipe");
        }

        let validation = context
            .validate_bound_workflow(&format!("{workflow_id}@v1"), BTreeMap::new(), false, false)
            .expect("validation");

        assert!(validation.checks.iter().any(|check| {
            check.code == "deferred_to_dispatch"
                && check.status == ValidationStatus::DeferredToDispatch
                && check.step_path.as_deref() == Some("consume")
        }));
        assert!(validation.checks.iter().any(|check| {
            check.code == "input_schema"
                && check.status == ValidationStatus::DeferredToDispatch
                && check.step_path.as_deref() == Some("consume")
        }));
        assert!(validation.checks.iter().all(|check| {
            check.step_path.as_deref() != Some("consume")
                || check.code != "request_preparation"
                || check.status != ValidationStatus::Failed
        }));
    }
}

#[test]
fn bound_validation_preserves_non_blocking_prerequisite_warnings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let workflow = temp.path().join("warning.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: warning.demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\nprerequisites:\n  paths:\n    - path: optional-missing\n      kind: file\n      required: false\nsteps:\n  - id: finish\n    control: control.finish\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("warning.demo@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert_eq!(validation.status, ValidationStatus::Warning);
    assert!(validation.checks.iter().any(|check| {
        check.code == "workflow_prerequisite.path" && check.status == ValidationStatus::Warning
    }));
}

#[test]
fn bound_validation_uses_reachability_for_approval_capabilities() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    write_allowlist_config(&home);
    let context = TransistContext::with_home(&workspace, &home);
    let workflow = temp.path().join("reachable.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: reachable.demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\ninputs:\n  require_review:\n    type: boolean\n    required: true\nsteps:\n  - id: choose\n    control: control.noop\n    if:\n      guard: inputs.require_review == true\n      then:\n        - ref: review\n      else:\n        - ref: finish\n  - id: review\n    control: control.gate.manual\n  - id: finish\n    control: control.finish\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow(
            "reachable.demo@v1",
            BTreeMap::from([("require_review".to_string(), KernelValue::Bool(false))]),
            false,
            false,
        )
        .expect("validation");

    assert!(validation.required_capabilities.is_empty());
    assert!(
        validation
            .checks
            .iter()
            .all(|check| check.step_path.as_deref() != Some("review"))
    );
}

#[test]
fn bound_validation_suggests_authority_for_runtime_conditional_subflows_and_gate_decisions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    write_allowlist_config(&home);
    let context = TransistContext::with_home(&workspace, &home);
    context
        .install_plugin_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
        )
        .expect("install shell plugin");
    let child = temp.path().join("modules/child.workflow.yaml");
    fs::create_dir_all(child.parent().expect("module parent")).expect("module parent");
    fs::write(
        &child,
        "schema: transist.module.v1\nid: child.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nsteps:\n  - id: nested_action\n    uses: process:shell/exec\n    with:\n      command: printf\n      args: [nested]\n  - id: nested_review\n    control: control.gate.manual\n    with:\n      gate_id: nested-review\n",
    )
    .expect("child workflow");
    let workflow = temp.path().join("root.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: root.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nmodules:\n  nested:\n    path: modules/child.workflow.yaml\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: inspect\n    uses: process:shell/echo\n    with:\n      args: [inspect]\n  - id: choose\n    control: control.noop\n    if:\n      guard: steps.inspect.out.continue == true\n      then:\n        - ref: nested\n  - id: nested\n    module: nested\n  - id: finish\n    control: control.finish\n",
            workspace.display()
        ),
    )
    .expect("root workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("root.demo@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "action" && capability.resource == "shell@0.1.0/exec"
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval"
            && capability.resource == "root.demo-module-nested/nested-review"
            && capability.operations == ["request"]
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval"
            && capability.resource == "root.demo-module-nested/nested-review"
            && capability.operations == ["decide"]
    }));
    let suggestion = &validation.policy_suggestion.grant_sets["suggested-root-demo"];
    assert!(
        suggestion
            .plugin_grants
            .iter()
            .any(|grant| { grant.plugin == "shell@0.1.0" && grant.actions.contains("exec") })
    );
    assert!(suggestion.capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval"
            && capability.resource == "root.demo-module-nested/nested-review"
            && capability.operations == ["decide"]
    }));
}

#[test]
fn bound_validation_recurses_prerequisites_and_approval_with_root_policy_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    write_allowlist_config(&home);
    let context = TransistContext::with_home(&workspace, &home);
    let child = temp.path().join("modules/child.workflow.yaml");
    fs::create_dir_all(child.parent().expect("module parent")).expect("module parent");
    fs::write(
        &child,
        "schema: transist.module.v1\nid: child.demo\nversion: v1\nsteps:\n  - id: review\n    control: control.gate.manual\n    with:\n      gate_id: child-review\n",
    )
    .expect("child");
    let workflow = temp.path().join("root.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: root.demo\nversion: v1\nmodules:\n  nested:\n    path: modules/child.workflow.yaml\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: nested\n    module: nested\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("root.demo@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert_eq!(validation.status, ValidationStatus::Failed);
    assert!(validation.checks.iter().any(|check| {
        check.step_path.as_deref() == Some("nested/review")
            && check.code == "capability_not_granted"
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval"
            && capability.resource == "root.demo-module-nested/child-review"
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval" && capability.operations == ["request"]
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "approval" && capability.operations == ["decide"]
    }));
    assert_eq!(
        validation.policy_suggestion.workflow_grants[0].workflow,
        "root.demo@v1"
    );
}

#[test]
fn bound_validation_accounts_for_grants_per_step_descriptor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    fs::write(
        home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  allow:\n    capabilities:\n      - capability: plugin\n        resource: shell@0.1.0\n        operations: [load]\n      - capability: action\n        resource: shell@0.1.0/exec\n        operations: [invoke]\n    plugin_grants:\n      - plugin: shell@0.1.0\n        actions: [echo]\n",
    )
    .expect("partial policy");
    fs::write(
        home.join("settings.yaml"),
        "network_policy:\n  mode: none\n",
    )
    .expect("network policy");
    let context = TransistContext::with_home(&workspace, &home);
    context
        .install_plugin_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
        )
        .expect("install shell plugin");
    let workflow = temp.path().join("descriptor-grants.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: descriptor.grants\nversion: v1\nplugins:\n  shell: 0.1.0\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: covered\n    uses: process:shell/echo\n    with:\n      args: [covered]\n  - id: uncovered\n    uses: process:shell/exec\n    with:\n      command: printf\n      args: [uncovered]\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("descriptor.grants@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert!(validation.missing_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "adapter" && capability.resource == "process"
    }));
    assert!(validation.missing_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "provider" && capability.resource == "local"
    }));
    assert!(validation.granted_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "adapter" && capability.resource == "process"
    }));
    let suggestion = &validation.policy_suggestion.grant_sets["suggested-descriptor-grants"];
    assert!(suggestion.plugin_grants.is_empty());
    assert!(suggestion.capabilities.iter().any(|capability| {
        capability.capability.as_str() == "adapter" && capability.resource == "process"
    }));
    assert!(suggestion.capabilities.iter().any(|capability| {
        capability.capability.as_str() == "provider" && capability.resource == "local"
    }));

    fs::create_dir(home.join("policy")).expect("policy directory");
    fs::write(
        home.join("policy/suggestion.yaml"),
        yaml_serde::to_string(&validation.policy_suggestion).expect("serialize suggestion"),
    )
    .expect("policy suggestion");
    fs::write(
        home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [suggestion.yaml]\n  allow:\n    capabilities:\n      - capability: plugin\n        resource: shell@0.1.0\n        operations: [load]\n      - capability: action\n        resource: shell@0.1.0/exec\n        operations: [invoke]\n    plugin_grants:\n      - plugin: shell@0.1.0\n        actions: [echo]\n",
    )
    .expect("policy with suggestion");

    let promoted_validation = context
        .validate_bound_workflow("descriptor.grants@v1", BTreeMap::new(), false, false)
        .expect("validation after applying suggestion");
    assert!(promoted_validation.is_valid(), "{promoted_validation:#?}");
    assert!(promoted_validation.missing_capabilities.is_empty());
}

#[test]
fn bound_validation_stops_nested_steps_at_blocking_workflow_prerequisites() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let child = temp.path().join("modules/child.workflow.yaml");
    fs::create_dir_all(child.parent().expect("module parent")).expect("module parent");
    fs::write(
        &child,
        "schema: transist.module.v1\nid: child.prereq\nversion: v1\nprerequisites:\n  paths:\n    - path: definitely-missing\n      kind: file\n      required: true\nsteps:\n  - id: review\n    control: control.gate.manual\n",
    )
    .expect("child");
    let workflow = temp.path().join("root.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: root.prereq\nversion: v1\nmodules:\n  nested:\n    path: modules/child.workflow.yaml\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: nested\n    module: nested\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("root.prereq@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert!(validation.checks.iter().any(|check| {
        check.code == "workflow_prerequisite.path"
            && check.status == ValidationStatus::Failed
            && check.step_path.as_deref() == Some("nested")
    }));
    assert!(validation.required_capabilities.is_empty());
}

#[test]
fn bound_validation_uses_root_owned_module_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);
    let child = temp.path().join("modules/child.workflow.yaml");
    fs::create_dir_all(child.parent().expect("module parent")).expect("module parent");
    fs::write(
        &child,
        "schema: transist.module.v1\nid: cycle.demo\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    )
    .expect("child");
    let workflow = temp.path().join("root.workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "schema: transist.flow.v1\nid: cycle.demo\nversion: v1\nmodules:\n  nested:\n    path: modules/child.workflow.yaml\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: nested\n    module: nested\n",
            workspace.display()
        ),
    )
    .expect("workflow");
    context
        .compile_flow_file(&workflow)
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("cycle.demo@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert_eq!(validation.status, ValidationStatus::Passed);
    assert!(
        validation
            .checks
            .iter()
            .all(|check| check.code != "subflow_preflight.cycle_detected")
    );
}

#[test]
fn bound_validation_limits_subflow_depth_like_runtime_preflight() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).expect("initialize home");
    let context = TransistContext::with_home(&workspace, &home);

    for depth in (0..=10).rev() {
        let schema = if depth == 0 {
            "transist.flow.v1"
        } else {
            "transist.module.v1"
        };
        let steps = if depth == 10 {
            "steps:\n  - id: finish\n    control: control.finish\n".to_string()
        } else {
            format!(
                "modules:\n  nested-{depth}:\n    path: depth-{}.workflow.yaml\nsteps:\n  - id: nested-{depth}\n    module: nested-{depth}\n",
                depth + 1
            )
        };
        let workspace_binding = if depth == 0 {
            format!(
                "workspace:\n  path: {}\n  access: read_only\n",
                workspace.display()
            )
        } else {
            String::new()
        };
        fs::write(
            temp.path().join(format!("depth-{depth}.workflow.yaml")),
            format!("schema: {schema}\nid: depth.{depth}\nversion: v1\n{workspace_binding}{steps}"),
        )
        .expect("workflow");
    }
    context
        .compile_flow_file(&temp.path().join("depth-0.workflow.yaml"))
        .expect("compile workflow");

    let validation = context
        .validate_bound_workflow("depth.0@v1", BTreeMap::new(), false, false)
        .expect("validation");

    assert!(validation.checks.iter().any(|check| {
        check.code == "subflow_preflight.depth_exceeded"
            && check.status == ValidationStatus::Warning
    }));
}
