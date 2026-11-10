// SPDX-License-Identifier: MPL-2.0
//! Prospective validation must inspect work reachable through a bounded exit.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use transist::{
    RuntimeOperatingMode, TransistContext, ValidationStatus, WorkflowAuthorizationStatus,
    init_home_dir_with_mode,
};

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    context: TransistContext,
    recipe: transist::Recipe,
}

impl Fixture {
    fn new(blocker: &str, nested: bool, condition: &str, cap: u32) -> Self {
        let symbolic = condition.contains("inputs.flag");
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        if blocker == "authority" {
            fs::create_dir_all(home.join("policy/workflow-authorizations")).unwrap();
            fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
        }
        let context = TransistContext::with_home(&workspace, &home);
        if blocker == "schema" {
            context
                .install_plugin_file(
                    &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
                )
                .unwrap();
        }
        let post = match blocker {
            "authority" => "- id: post\n  control: control.gate.manual\n",
            "schema" => {
                "- id: post\n  uses: process:shell/echo\n  with: {args: {invalid: object}}\n"
            }
            "prerequisite" => "- id: post\n  module: blocked\n",
            _ => unreachable!(),
        };
        let modules = if blocker == "prerequisite" {
            fs::write(root.join("blocked.yaml"), "schema: transist.module.v1\nid: blocked\nversion: 0.1.0\nprerequisites:\n  paths:\n    - {path: absent-required-file, kind: file, required: true}\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
            "modules:\n  blocked: {path: blocked.yaml}\n"
        } else {
            ""
        };
        let plugins = if blocker == "schema" {
            "plugins:\n  shell: 0.1.0\n"
        } else {
            ""
        };
        let body = if symbolic {
            "control.gate.manual"
        } else {
            "control.noop"
        };
        let steps = format!(
            "steps:\n- id: repeat\n  control: control.noop\n  loop:\n    {condition}\n    max_iters: {cap}\n    body: [{{ref: body}}]\n- id: body\n  control: {body}\n{post}- id: finish\n  control: control.finish\n"
        );
        let root_scope = format!(
            "workspace: {{path: '{}', access: read_only}}\nnetwork: {{mode: none}}\n{plugins}",
            workspace.display()
        );
        let source = if nested {
            let inputs = if symbolic {
                "inputs:\n  flag: {type: boolean, required: true}\n"
            } else {
                ""
            };
            fs::write(
                root.join("inner.yaml"),
                format!("schema: transist.module.v1\nid: inner\nversion: 0.1.0\n{inputs}{modules}{steps}"),
            )
            .unwrap();
            let calls = if symbolic {
                "- id: seed\n  control: control.noop\n- id: inner\n  module:\n    ref: inner\n    inputs: {flag: '{{steps.seed.out.flag}}'}\n"
            } else {
                "- id: inner\n  module: inner\n"
            };
            format!("{root_scope}modules:\n  inner: {{path: inner.yaml}}\nsteps:\n{calls}")
        } else {
            format!("{root_scope}{modules}{steps}")
        };
        let path = root.join("workflow.yaml");
        fs::write(
            &path,
            format!("schema: transist.flow.v1\nid: prospective\nversion: 0.1.0\n{source}"),
        )
        .unwrap();
        let compiled = context.compile_flow_file(&path).unwrap();
        let recipe = yaml_serde::from_str(&fs::read_to_string(compiled).unwrap()).unwrap();
        Self {
            _temp: temp,
            home,
            context,
            recipe,
        }
    }

    fn assert_birth_denied_without_effects(&self) {
        let (result, probes) = transist::capture_preflight_measurements(|| {
            self.context
                .start_recipe(&self.recipe, Some("denied"), BTreeMap::new())
        });
        assert!(result.is_err());
        // Kernel preflight may record an empty probe batch after static schema
        // rejection. Only nonempty groups represent live participant probes.
        assert!(
            !probes
                .iter()
                .any(|record| matches!(record, transist::PreflightMeasurement::ProbeBatch { probe_group_count, remote_probe_count, .. } if *probe_group_count > 0 || *remote_probe_count > 0)),
            "birth blockers must precede live probes: {probes:#?}"
        );
        assert!(self.context.runs_list().unwrap().is_empty());
        let authorizations = self.home.join("policy/workflow-authorizations");
        if authorizations.exists() {
            assert!(fs::read_dir(authorizations).unwrap().next().is_none());
        }
    }
}

#[test]
fn loop_cap_routing_prospective_symbolic_module_predicates_keep_body_and_exit_authority() {
    for condition in [
        "until: 'inputs.flag == true'",
        "while: 'inputs.flag == true'",
    ] {
        let f = Fixture::new("authority", true, condition, 2);
        let report = f
            .context
            .validate_bound_workflow("prospective@0.1.0", BTreeMap::new(), false, false)
            .unwrap();
        for target in ["body", "post"] {
            assert!(
                report
                    .required_capabilities
                    .iter()
                    .any(|request| request.capability.as_str() == "approval"
                        && request.resource.ends_with(&format!("/{target}"))),
                "{report:#?}"
            );
        }
        f.assert_birth_denied_without_effects();
    }
}

#[test]
fn loop_cap_routing_prospective_exit_requires_downstream_approval_authority() {
    for nested in [false, true] {
        for condition in ["until: 'false'", "while: 'true'"] {
            for cap in [1, 2] {
                let f = Fixture::new("authority", nested, condition, cap);
                let report = f
                    .context
                    .validate_bound_workflow("prospective@0.1.0", BTreeMap::new(), false, false)
                    .unwrap();
                assert!(
                    report
                        .checks
                        .iter()
                        .any(|c| c.code == "capability_not_granted"
                            && c.step_path.as_deref().is_some_and(|p| p.ends_with("post"))),
                    "{report:#?}"
                );
                for operation in ["request", "decide"] {
                    assert!(
                        report
                            .required_capabilities
                            .iter()
                            .any(|r| r.capability.as_str() == "approval"
                                && r.operations == [operation]),
                        "{report:#?}"
                    );
                }
                let authorization = f
                    .context
                    .authorize_workflow_summary("prospective@0.1.0", BTreeMap::new(), None, false)
                    .unwrap();
                assert_eq!(
                    authorization.status,
                    WorkflowAuthorizationStatus::Planned,
                    "{authorization:#?}"
                );
                f.assert_birth_denied_without_effects();
            }
        }
    }
}

#[test]
fn loop_cap_routing_prospective_exit_validates_downstream_action_schema() {
    for nested in [false, true] {
        for condition in ["until: 'false'", "while: 'true'"] {
            for cap in [1, 2] {
                let f = Fixture::new("schema", nested, condition, cap);
                let (report, probes) = transist::capture_preflight_measurements(|| {
                    f.context
                        .workflow_validation_report(
                            "prospective@0.1.0",
                            BTreeMap::new(),
                            false,
                            false,
                        )
                        .unwrap()
                });
                assert!(
                    report.checks.iter().any(|c| c.code == "input_schema"
                        && c.status == ValidationStatus::Failed
                        && c.step_path.as_deref().is_some_and(|p| p.ends_with("post"))),
                    "{report:#?}"
                );
                assert!(!probes.iter().any(|record| matches!(
                    record,
                    transist::PreflightMeasurement::ProbeBatch { .. }
                )));
                let authorization = f
                    .context
                    .authorize_workflow_summary("prospective@0.1.0", BTreeMap::new(), None, false)
                    .unwrap();
                assert_eq!(authorization.status, WorkflowAuthorizationStatus::Blocked);
                f.assert_birth_denied_without_effects();
            }
        }
    }
}

#[test]
fn loop_cap_routing_prospective_exit_validates_downstream_module_prerequisites() {
    for nested in [false, true] {
        for condition in ["until: 'false'", "while: 'true'"] {
            for cap in [1, 2] {
                let f = Fixture::new("prerequisite", nested, condition, cap);
                let report = f
                    .context
                    .workflow_validation_report("prospective@0.1.0", BTreeMap::new(), false, false)
                    .unwrap();
                assert!(
                    report
                        .checks
                        .iter()
                        .any(|c| c.code.starts_with("workflow_prerequisite")
                            && c.status == ValidationStatus::Failed
                            && c.step_path.as_deref().is_some_and(|p| p.ends_with("post"))),
                    "{report:#?}"
                );
                assert_eq!(
                    f.context
                        .authorize_workflow_summary(
                            "prospective@0.1.0",
                            BTreeMap::new(),
                            None,
                            false
                        )
                        .unwrap()
                        .status,
                    WorkflowAuthorizationStatus::Blocked
                );
                f.assert_birth_denied_without_effects();
            }
        }
    }
}
