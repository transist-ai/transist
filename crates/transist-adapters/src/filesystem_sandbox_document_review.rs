// SPDX-License-Identifier: MPL-2.0
//! Independent-review regressions for private document confinement.
use super::*;

struct Fixture {
    _base: tempfile::TempDir,
    _active: tempfile::TempDir,
    context: CanonicalSandboxContext,
}

impl Fixture {
    fn new() -> Self {
        let base = tempfile::tempdir().unwrap();
        let base_path = std::fs::canonicalize(base.path()).unwrap();
        let home = base_path.join("home");
        let workspace = base_path.join("workspace");
        let plugin = home.join("plugins/fixture/0.1.0");
        let staging = home.join(".transist-staging/invoke-current");
        for path in [
            &workspace,
            &plugin,
            &staging.join("artifacts"),
            &staging.join("evidence"),
            &staging.join("source"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(staging.join("source/captured.txt"), "captured bytes").unwrap();
        let active = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .unwrap();
        std::fs::create_dir_all(active.path().join("documents/read")).unwrap();
        std::fs::create_dir_all(active.path().join("documents/write")).unwrap();
        let context = CanonicalSandboxContext {
            home_dir: Some(home),
            workspace_dir: workspace,
            artifacts_dir: staging.join("artifacts"),
            evidence_dir: staging.join("evidence"),
            staging_dir: Some(staging),
            runtime_temp_dir: Some(active.path().canonicalize().unwrap()),
            plugin_dir: plugin,
            recipe_source_dir: None,
        };
        Self {
            _base: base,
            _active: active,
            context,
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn review2_native_home_aliases_preserve_exact_resources_and_isolation() {
    const CHILD_BASE: &str = "TRANSIST_REVIEW2_ALIAS_CHILD_BASE";
    if let Some(base) = std::env::var_os(CHILD_BASE) {
        let base = PathBuf::from(base);
        let home = base.join("home");
        let staging = home.join(".transist-staging/invoke-current");
        let context = SandboxContext {
            home_dir: Some(home.clone()),
            workspace_dir: base.join("workspace"),
            plugin_dir: home.join("plugins/fixture/0.1.0"),
            artifacts_dir: staging.join("artifacts"),
            evidence_dir: staging.join("evidence"),
            staging_dir: Some(staging),
            runtime_temp_dir: None,
            recipe_source_dir: None,
        };
        let policy = FilesystemPolicy {
            read_allow_paths: vec![
                "~/.gitconfig".into(),
                "~/.ssh".into(),
                "~/linked/settings.conf".into(),
            ],
            write_allow_paths: vec!["~/approved-out".into()],
            ..Default::default()
        };
        let plan = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &policy,
            &NetworkPolicy {
                mode: NetworkAccessMode::Any,
                ..Default::default()
            },
            &context,
        )
        .expect("authorized aliases have a valid preflight plan");
        let script = r#"
test "$(cat "$HOME/.gitconfig")" = git-fixture
test "$(cat "$HOME/.ssh/id-test.pub")" = public-fixture
test "$(cat "$HOME/linked/settings.conf")" = settings-fixture
test "$(cat "$CANONICAL_CONFIG")" = git-fixture
if cat "$HOME/private.txt" >/dev/null 2>&1; then exit 91; fi
if cat "$HOME/linked/private.txt" >/dev/null 2>&1; then exit 92; fi
if cat "$PRIVATE_RUNTIME" >/dev/null 2>&1; then exit 93; fi
if touch "$HOME/ungranted-write" 2>/dev/null; then exit 94; fi
if touch "$HOME/.ssh/ungranted-write" 2>/dev/null; then exit 95; fi
printf authorized-write > "$HOME/approved-out/result.txt"
"#;
        let output = make_sandboxed_command(
            Path::new("/bin/sh"),
            &["-eu".into(), "-c".into(), script.into()],
            &plan,
        )
        .unwrap()
        .env("CANONICAL_CONFIG", base.join("operator/dotfiles/gitconfig"))
        .env("PRIVATE_RUNTIME", home.join("secret.txt"))
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "real alias sandbox failed: {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Use a one-test subprocess so HOME is fixed before any test thread starts;
    // no process-global environment mutation races the other sandbox tests.
    let fixture = Fixture::new();
    let base = fixture._base.path().canonicalize().unwrap();
    let operator = base.join("operator");
    for path in [
        operator.join("dotfiles/ssh"),
        operator.join("approved-out"),
        base.join("external"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(operator.join("dotfiles/gitconfig"), "git-fixture").unwrap();
    std::fs::write(operator.join("dotfiles/ssh/id-test.pub"), "public-fixture").unwrap();
    std::fs::write(operator.join("private.txt"), "ungranted operator bytes").unwrap();
    std::fs::write(base.join("external/settings.conf"), "settings-fixture").unwrap();
    std::fs::write(base.join("external/private.txt"), "ungranted parent bytes").unwrap();
    std::fs::write(base.join("home/secret.txt"), "private runtime bytes").unwrap();
    std::os::unix::fs::symlink("dotfiles/gitconfig", operator.join(".gitconfig")).unwrap();
    std::os::unix::fs::symlink("dotfiles/ssh", operator.join(".ssh")).unwrap();
    std::os::unix::fs::symlink(base.join("external"), operator.join("linked")).unwrap();
    std::os::unix::fs::symlink("operator", base.join("operator-alias")).unwrap();
    for home in [&operator, &base.join("operator-alias")] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "filesystem_sandbox::document_review::review2_native_home_aliases_preserve_exact_resources_and_isolation", "--nocapture"])
            .env(CHILD_BASE, &base).env("HOME", home).output().unwrap();
        assert!(
            output.status.success(),
            "native HOME={home:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        std::fs::read_link(operator.join(".gitconfig")).unwrap(),
        Path::new("dotfiles/gitconfig")
    );
    assert_eq!(
        std::fs::read_link(operator.join(".ssh")).unwrap(),
        Path::new("dotfiles/ssh")
    );
    assert_eq!(
        std::fs::read_link(operator.join("linked")).unwrap(),
        base.join("external")
    );
    assert_eq!(
        std::fs::read_to_string(operator.join("approved-out/result.txt")).unwrap(),
        "authorized-write"
    );
    assert!(!operator.join("ungranted-write").exists());
}

#[cfg(unix)]
#[test]
fn review2_alias_destinations_and_sources_cannot_overlap_private_roots() {
    let fixture = Fixture::new();
    let external = fixture
        .context
        .workspace_dir
        .parent()
        .unwrap()
        .join("external-data");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("data.txt"), "ordinary bytes").unwrap();
    for root in [
        fixture.context.home_dir.as_ref().unwrap(),
        fixture.context.staging_dir.as_ref().unwrap(),
        fixture.context.runtime_temp_dir.as_ref().unwrap(),
    ] {
        let alias = root.join("outside-alias");
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        for grant in [&alias, &alias.join("data.txt")] {
            for write in [false, true] {
                let mut policy = FilesystemPolicy::default();
                if write {
                    policy.write_allow_paths.push(grant.display().to_string());
                } else {
                    policy.read_allow_paths.push(grant.display().to_string());
                }
                let error =
                    validate_external_grants_against_protected_roots(&policy, &fixture.context)
                        .expect_err(
                            "a lexical private destination cannot be reopened by an outside source",
                        );
                assert!(error.contains("protected runtime root"), "{error}");
            }
        }
    }
    // The operator may itself reach the private home through another symlink;
    // checking just the final canonical target and literal destination misses it.
    let home_alias = external.join("runtime-home-alias");
    std::os::unix::fs::symlink(fixture.context.home_dir.as_ref().unwrap(), &home_alias).unwrap();
    let policy = FilesystemPolicy {
        read_allow_paths: vec![
            home_alias
                .join("outside-alias/data.txt")
                .display()
                .to_string(),
        ],
        ..Default::default()
    };
    assert!(validate_external_grants_against_protected_roots(&policy, &fixture.context).is_err());
    let alias = external.join("private-source");
    std::os::unix::fs::symlink(fixture.context.home_dir.as_ref().unwrap(), &alias).unwrap();
    let policy = FilesystemPolicy {
        read_allow_paths: vec![alias.display().to_string()],
        ..Default::default()
    };
    assert!(validate_external_grants_against_protected_roots(&policy, &fixture.context).is_err());
    let traversal = external.join("..").join("external-data/data.txt");
    let policy = FilesystemPolicy {
        read_allow_paths: vec![traversal.display().to_string()],
        ..Default::default()
    };
    assert!(
        validate_external_grants_against_protected_roots(&policy, &fixture.context)
            .unwrap_err()
            .contains("..")
    );
}

#[cfg(unix)]
#[test]
fn review2_workspace_alias_destination_cannot_widen_read_only_workspace() {
    let fixture = Fixture::new();
    let outside = fixture
        .context
        .home_dir
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join("outside-workspace");
    std::fs::create_dir(&outside).unwrap();
    let alias = fixture.context.workspace_dir.join("alias");
    std::os::unix::fs::symlink(outside, &alias).unwrap();
    for write in [false, true] {
        let mut policy = FilesystemPolicy::default();
        if write {
            policy.write_allow_paths.push(alias.display().to_string());
        } else {
            policy.read_allow_paths.push(alias.display().to_string());
        }
        assert!(
            validate_external_grants_against_workspace(&policy, &fixture.context.workspace_dir)
                .is_err()
        );
    }
}

#[test]
fn review_step4_workspace_and_external_aliases_reject_both_backend_plans() {
    for exposed in ["workspace", "external", "plugin", "staging"] {
        let mut fixture = Fixture::new();
        let exposed_root = fixture.context.workspace_dir.join(exposed);
        let root = exposed_root.join("tmp/transist-runtime-temp-alias");
        std::fs::create_dir_all(root.join("documents/read")).unwrap();
        std::fs::create_dir_all(root.join("documents/write")).unwrap();
        fixture.context.runtime_temp_dir = Some(root);
        let mut policy = FilesystemPolicy::default();
        match exposed {
            "workspace" => {}
            "external" => {
                fixture.context.workspace_dir = fixture.context.plugin_dir.clone();
                policy
                    .write_allow_paths
                    .push(exposed_root.display().to_string());
            }
            "plugin" => {
                fixture.context.workspace_dir = fixture.context.plugin_dir.clone();
                fixture.context.plugin_dir = exposed_root;
            }
            "staging" => {
                fixture.context.workspace_dir = fixture.context.plugin_dir.clone();
                fixture.context.staging_dir = Some(exposed_root);
            }
            _ => unreachable!(),
        }
        assert!(
            build_bwrap_plan(
                &fixture.context,
                &policy,
                &NetworkPolicy::default(),
                RuntimeMountPolicy::ValidateOnly
            )
            .is_err(),
            "Linux exposed {exposed} alias was accepted"
        );
        assert!(
            build_sandbox_exec_plan(&fixture.context, &policy, &NetworkPolicy::default()).is_err(),
            "macOS exposed {exposed} alias was accepted"
        );
    }
}

#[test]
fn review_step4_source_parents_do_not_reopen_shared_roots_in_plans() {
    let mut fixture = Fixture::new();
    for source in [
        runtime_temp_parent(),
        fixture
            .context
            .home_dir
            .as_ref()
            .unwrap()
            .join(INVOCATION_STAGING_DIR),
    ] {
        fixture.context.recipe_source_dir = Some(source.clone());
        let linux = build_bwrap_plan(
            &fixture.context,
            &FilesystemPolicy::default(),
            &NetworkPolicy::default(),
            RuntimeMountPolicy::ValidateOnly,
        )
        .unwrap();
        assert!(
            !linux
                .bwrap_args
                .windows(3)
                .any(|args| args[0] == "--ro-bind" && args[1] == source.display().to_string()),
            "broad source mount: {source:?}"
        );
    }
}

#[test]
fn review_step4_native_source_cannot_read_another_live_document_copy() {
    let mut fixture = Fixture::new();
    let other = tempfile::Builder::new()
        .prefix(RUNTIME_TEMP_PREFIX)
        .tempdir_in(runtime_temp_parent())
        .unwrap();
    let other_doc = other
        .path()
        .join("documents/read/binding-00000000000000000009/member.txt");
    std::fs::create_dir_all(other_doc.parent().unwrap()).unwrap();
    std::fs::write(&other_doc, "other invocation private document").unwrap();
    let staging_parent = fixture
        .context
        .home_dir
        .as_ref()
        .unwrap()
        .join(INVOCATION_STAGING_DIR);
    let other_stage = tempfile::Builder::new()
        .prefix("invoke-other-")
        .tempdir_in(&staging_parent)
        .unwrap();
    let other_staged_doc = other_stage.path().join("private-document.txt");
    std::fs::write(&other_staged_doc, "other staging private bytes").unwrap();
    for source in [runtime_temp_parent(), staging_parent] {
        fixture.context.recipe_source_dir = Some(source);
        let policy = FilesystemPolicy::default();
        let network = NetworkPolicy::default();
        #[cfg(target_os = "macos")]
        let plan = build_sandbox_exec_plan(&fixture.context, &policy, &network).unwrap();
        #[cfg(target_os = "linux")]
        let plan = build_bwrap_plan(
            &fixture.context,
            &policy,
            &network,
            RuntimeMountPolicy::Create,
        )
        .unwrap();
        let script = format!(
            "if cat '{}' >/dev/null 2>&1; then exit 91; fi; if cat '{}' >/dev/null 2>&1; then exit 92; fi; test \"$(cat '{}')\" = 'captured bytes'",
            other_doc.display(),
            other_staged_doc.display(),
            fixture
                .context
                .staging_dir
                .as_ref()
                .unwrap()
                .join("source/captured.txt")
                .display()
        );
        let mut command =
            make_sandboxed_command(Path::new("/bin/sh"), &["-c".into(), script], &plan).unwrap();
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "source exception leaked sibling bytes: status={:?}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn review_step4_exposed_temp_parent_is_rejected_before_staging_creation() {
    let fixture = Fixture::new();
    for external in [false, true] {
        let home = fixture._base.path().join(if external {
            "creation-external-home"
        } else {
            "creation-workspace-home"
        });
        std::fs::create_dir(&home).unwrap();
        let workspace = if external {
            fixture.context.workspace_dir.clone()
        } else {
            runtime_temp_parent()
        };
        let filesystem = if external {
            serde_json::json!({"write_allow_paths":[runtime_temp_parent()]})
        } else {
            serde_json::json!({})
        };
        let request: ExecutionRequest = serde_json::from_value(serde_json::json!({
            "type":"tep.invoke.request.v1", "protocol_version":"tep.v1", "invoke_id":"test", "run_id":"test", "workflow_id":"test", "step":{"id":"test","execution":1}, "adapter_kind":"process", "action":"test", "input":{},
            "paths":{"run_root":home.join("runs/run-test"), "workspace_dir":workspace, "artifacts_dir":home.join("artifacts"), "evidence_dir":home.join("evidence")}, "requested_capabilities":[], "policy":{"filesystem":filesystem}, "metadata":{}
        })).unwrap();
        assert!(
            transist_storage_local::invocation_staging::InvocationStaging::new(&home, &request)
                .is_err()
        );
        assert!(crate::preflight::private_preflight_request(&request).is_err());
        assert!(
            !home.join(".transist-staging").exists(),
            "unsafe placement created staging before rejection"
        );
    }
}
