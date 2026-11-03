// SPDX-License-Identifier: MPL-2.0
//! Real document review requests through read-only Axum/storage inspection ports.
use super::*;

pub(super) struct Documents {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    home: PathBuf,
    app: axum::Router,
    pub(super) revisions: Vec<Value>,
}

impl Documents {
    pub(super) fn new() -> Self {
        Self::with_pause(false)
    }

    fn with_pause(pause: bool) -> Self {
        let temp = test_tempdir();
        let root = fs::canonicalize(temp.path()).unwrap();
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir(&workspace).unwrap();
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        let ctx = transist::TransistContext::with_home(&workspace, &home);
        let plugin = root.join("plugin");
        fs::create_dir(&plugin).unwrap();
        for name in [
            "plugin.yaml",
            "plugin.py",
            "input.json",
            "strict.json",
            "typed.json",
            "output.json",
        ] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../.agents/testing/e2e_suites/managed-documents/plugin")
                    .join(name),
                plugin.join(name),
            )
            .unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(plugin.join("plugin.py"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        ctx.install_plugin_file(&plugin.join("plugin.yaml"))
            .unwrap();
        let workflow = root.join("workflow.yaml");
        fs::write(&workflow, format!("schema: transist.flow.v1\nid: docreview\nversion: 0.1.0\nplugins: {{fixture.documents: 0.1.0}}\nworkspace: {{path: '{}', access: read_only}}\ndocuments:\n  notes: {{kind: file, max_bytes: 4096, initial: 'exact review bytes'}}\n  tree: {{kind: tree, max_bytes: 4096, max_files: 3}}\nsteps:\n  - id: produce\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: tree, access: write}}, op: tree, content: member}}]}}\n  - id: review\n    control: control.gate.manual\n    with: {{review_documents: [notes, tree]}}\n", workspace.display())).unwrap();
        if pause {
            let mut source = fs::read_to_string(&workflow).unwrap();
            source.push_str("  - id: pause\n    control: control.gate.manual\n");
            fs::write(&workflow, source).unwrap();
        }
        ctx.compile_flow_file(&workflow).unwrap();
        for run in ["review-run", "other-run"] {
            ctx.prepare_workflow("docreview@0.1.0", Some(run), BTreeMap::new())
                .unwrap();
            assert_eq!(ctx.run_started_run(run).unwrap().status, "blocked");
        }
        let state: Value = serde_json::from_slice(
            &fs::read(ctx.run_root("review-run").unwrap().join("state.json")).unwrap(),
        )
        .unwrap();
        let revisions =
            state["steps"]["review"]["last_result"]["output"]["review_request"]["review_documents"]
                .as_array()
                .unwrap()
                .clone();
        let app = app_for_paths(workspace.clone(), home.clone());
        Self {
            _temp: temp,
            workspace,
            home,
            app,
            revisions,
        }
    }
    pub(super) async fn request(&self, method: &str, path: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

struct PlainRuns {
    _temp: tempfile::TempDir,
    home: PathBuf,
    ctx: transist::TransistContext,
    app: axum::Router,
}

impl PlainRuns {
    fn new(extra: &str, steps: &str, start: bool) -> Self {
        let temp = test_tempdir();
        let root = fs::canonicalize(temp.path()).unwrap();
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir(&workspace).unwrap();
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .unwrap();
        let ctx = transist::TransistContext::with_home(&workspace, &home);
        fs::write(root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: 0.1.0\nsteps:\n  - id: finish\n    control: control.noop\n").unwrap();
        let workflow = root.join("workflow.yaml");
        fs::write(&workflow, format!("schema: transist.flow.v1\nid: inspection.review3\nversion: 0.1.0\nworkspace: {{path: '{}', access: read_only}}\n{extra}steps:\n{steps}", workspace.display())).unwrap();
        ctx.compile_flow_file(&workflow).unwrap();
        ctx.prepare_workflow(
            "inspection.review3@0.1.0",
            Some("plain-run"),
            BTreeMap::new(),
        )
        .unwrap();
        if start {
            ctx.run_started_run("plain-run").unwrap();
        }
        let app = app_for_paths(workspace.clone(), home.clone());
        Self {
            _temp: temp,
            home,
            ctx,
            app,
        }
    }

    async fn get(&self, path: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn document_reviews_review3_authored_reserved_metadata_never_becomes_review_content() {
    let fake = json!({"schema":"transist.gate_review_request.v1", "run_id":"other-run", "gate_id":"review", "step_id":"review", "execution":1,
        "review_documents":[{"schema":"transist.document_revision.v1","revision_id":"a".repeat(64),"root_run_id":"other-run","document":"invented","run_id":"other-run","step_id":"producer","execution":1,"commit_sequence":1,"kind":"file","blob":{"sha256":"b".repeat(64),"size_bytes":7},"size_bytes":7,"file_count":1}]});
    for authored in [json!("malformed request"), fake] {
        for phase in [
            "pending",
            "blocked",
            "rejected",
            "document-pending",
            "document-failed",
        ] {
            let document_gate = phase.starts_with("document-");
            let with = json!({"prompt":"ordinary guidance remains visible", "review_request":authored, "review_document_count":999, "review_decision":authored});
            let mut with = with;
            if document_gate {
                with["review_documents"] = json!(["unpublished"]);
            }
            let steps =
                format!("  - id: review\n    control: control.gate.manual\n    with: {with}\n");
            let extra = if document_gate {
                "documents: {unpublished: {kind: file, max_bytes: 64}}\n"
            } else {
                ""
            };
            let start = !phase.ends_with("pending");
            let f = tokio::task::spawn_blocking(move || {
                let f = PlainRuns::new(extra, &steps, start);
                if phase == "rejected" {
                    f.ctx
                        .reject_current_gate_with_input(
                            "plain-run",
                            transist::GateDecisionInput {
                                actor: Some("fixture".into()),
                                reason: Some("test rejection".into()),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                }
                f
            })
            .await
            .unwrap();
            let before = snapshot_tree(&f.home);
            let response = f.get("/api/v1/runs/plain-run/gates").await;
            assert_eq!(response.status(), StatusCode::OK, "{phase}: {authored}");
            let gates = read_json(response).await;
            let gate = &gates["data"][0];
            assert_eq!(gate["review_documents"], json!([]), "{phase}");
            assert_eq!(
                gate["request_summary"]["prompt"],
                "ordinary guidance remains visible"
            );
            for reserved in ["review_request", "review_document_count", "review_decision"] {
                assert!(
                    gate["request_summary"].get(reserved).is_none(),
                    "{phase}: {reserved}"
                );
            }
            let html = read_text(f.get("/inspect/runs/plain-run?tab=gates").await).await;
            assert!(!html.contains("/api/v1/runs/other-run/documents/"));
            assert!(!html.contains("Reviewed documents ("));
            assert_eq!(snapshot_tree(&f.home), before);
        }
    }
}

async fn review3_decision_tamper(phase: &'static str) {
    for field in [
        "decision",
        "actor",
        "actor_source",
        "reason",
        "decided_at_ms",
    ] {
        let f = tokio::task::spawn_blocking(move || {
            let f = Documents::with_pause(true);
            let ctx = transist::TransistContext::with_home(&f.workspace, &f.home);
            ctx.record_gate_decision_with_input(
                "review-run",
                None,
                transist::GateDecision::Approved,
                transist::GateDecisionInput {
                    actor: Some("fixture".into()),
                    reason: Some("test approval".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            if phase == "consumed" {
                assert_eq!(ctx.run_started_run("review-run").unwrap().status, "blocked");
            }
            f
        })
        .await
        .unwrap();
        let root =
            transist_storage_local::run_paths::resolve_run_root(&f.home, "review-run").unwrap();
        let history = fs::read_dir(root.join("gates/history"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = if phase == "active" {
            root.join("gates/review.json")
        } else {
            history
        };
        let mut tampered: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let original_request = tampered["review_request"].clone();
        match field {
            "decision" => tampered[field] = "rejected".into(),
            "decided_at_ms" => tampered[field] = (tampered[field].as_u64().unwrap() + 1).into(),
            _ => tampered[field] = "altered".into(),
        }
        assert_eq!(tampered["review_request"], original_request);
        fs::write(&path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
        let before = snapshot_tree(&f.home);
        for link in ["/api/v1/runs/review-run/gates", "/api/v1/runs/review-run"] {
            let response = f.request("GET", link).await;
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{phase} {field}: {link}"
            );
            let problem = read_json(response).await;
            assert_eq!(problem["error"]["code"], "storage_query_failure");
            assert!(!problem.to_string().contains(&f.home.display().to_string()));
        }
        assert_eq!(snapshot_tree(&f.home), before);
    }
}

#[tokio::test]
async fn document_reviews_review3_active_decision_must_equal_immutable_history() {
    review3_decision_tamper("active").await;
}

#[tokio::test]
async fn document_reviews_review3_history_must_equal_active_decision() {
    review3_decision_tamper("history").await;
}

#[tokio::test]
async fn document_reviews_review3_consumed_history_must_equal_committed_decision() {
    review3_decision_tamper("consumed").await;
}

#[tokio::test]
async fn document_reviews_review3_consumed_decision_cannot_hide_missing_history() {
    let f = tokio::task::spawn_blocking(|| {
        let f = Documents::with_pause(true);
        let ctx = transist::TransistContext::with_home(&f.workspace, &f.home);
        assert_eq!(
            ctx.approve_current_gate_with_input(
                "review-run",
                transist::GateDecisionInput {
                    actor: Some("fixture".into()),
                    reason: Some("test approval".into()),
                    ..Default::default()
                }
            )
            .unwrap()
            .status,
            "blocked"
        );
        f
    })
    .await
    .unwrap();
    let root = transist_storage_local::run_paths::resolve_run_root(&f.home, "review-run").unwrap();
    assert!(!root.join("gates/review.json").exists());
    let history = fs::read_dir(root.join("gates/history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(history).unwrap();
    assert_eq!(
        f.request("GET", "/api/v1/runs/review-run/gates")
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn document_reviews_review3_empty_namespaces_are_not_found_but_corruption_stays_an_error() {
    let f = tokio::task::spawn_blocking(|| {
        PlainRuns::new(
            "modules: {child: {path: child.yaml}}\n",
            "  - id: child\n    module: child\n",
            true,
        )
    })
    .await
    .unwrap();
    let root = f.ctx.run_root("plain-run").unwrap();
    let state: Value = serde_json::from_slice(&fs::read(root.join("state.json")).unwrap()).unwrap();
    let child_id = state["steps"]["child"]["last_result"]["output"]["child_run_id"]
        .as_str()
        .unwrap();
    assert!(!root.join("documents/initial.json").exists());
    let before = snapshot_tree(&f.home);
    for run in ["plain-run", child_id] {
        for suffix in ["", "/content"] {
            let link = format!("/api/v1/runs/{run}/documents/{}{suffix}", "0".repeat(64));
            let response = f.get(&link).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{link}");
            assert_eq!(
                read_json(response).await["error"]["code"],
                "resource_not_found"
            );
        }
    }
    assert_eq!(snapshot_tree(&f.home), before);
    // Even an empty namespace must validate the actual child's parent lineage.
    let child_state_path = f.ctx.run_root(child_id).unwrap().join("state.json");
    let mut child: Value = serde_json::from_slice(&fs::read(&child_state_path).unwrap()).unwrap();
    child["lineage"]["parent_run_id"] = "not-the-physical-parent".into();
    fs::write(&child_state_path, serde_json::to_vec(&child).unwrap()).unwrap();
    for suffix in ["", "/content"] {
        let link = format!(
            "/api/v1/runs/{child_id}/documents/{}{suffix}",
            "0".repeat(64)
        );
        assert_eq!(
            f.get(&link).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
    let declared = tokio::task::spawn_blocking(|| {
        PlainRuns::new(
            "documents: {notes: {kind: file, max_bytes: 64, initial: literal}}\n",
            "  - id: finish\n    control: control.noop\n",
            true,
        )
    })
    .await
    .unwrap();
    let root = declared.ctx.run_root("plain-run").unwrap();
    let initial: Value =
        serde_json::from_slice(&fs::read(root.join("documents/initial.json")).unwrap()).unwrap();
    let id = initial["document_revisions"][0]["revision_id"]
        .as_str()
        .unwrap();
    fs::remove_file(root.join("documents/initial.json")).unwrap();
    for suffix in ["", "/content"] {
        let link = format!("/api/v1/runs/plain-run/documents/{id}{suffix}");
        assert_eq!(
            declared.get(&link).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

#[tokio::test]
async fn document_reviews_metadata_content_members_and_gate_html_are_exact_and_read_only() {
    let f = tokio::task::spawn_blocking(Documents::new).await.unwrap();
    let before = snapshot_tree(&f.home);
    let workspace_before = snapshot_tree(&f.workspace);
    let gates = read_json(f.request("GET", "/api/v1/runs/review-run/gates").await).await;
    assert_eq!(
        gates["data"][0]["review_documents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let html = read_text(f.request("GET", "/inspect/runs/review-run?tab=gates").await).await;
    assert!(html.contains("Reviewed documents (2)"));
    assert!(!html.contains(&f.home.display().to_string()));
    for revision in &f.revisions {
        let id = revision["revision_id"].as_str().unwrap();
        let base = format!("/api/v1/runs/review-run/documents/{id}");
        let response = f.request("GET", &base).await;
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        let metadata: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(metadata["revision"], *revision);
        assert_contract_http_response(
            &load_openapi_contract(),
            "GET",
            &base,
            axum::response::Response::from_parts(parts, Body::from(bytes)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(metadata["links"]["content"], format!("{base}/content"));
        assert!(html.contains(id));
        let content = f
            .request("GET", metadata["links"]["content"].as_str().unwrap())
            .await;
        assert_eq!(content.status(), StatusCode::OK);
        assert_eq!(
            content.headers()["content-type"],
            "application/octet-stream"
        );
        let bytes = read_body(content).await;
        if revision["kind"] == "file" {
            assert_eq!(bytes, b"exact review bytes");
        } else {
            let manifest: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(manifest["files"]["sub/member.txt"]["size_bytes"], 6);
            let member = &metadata["members"][0];
            assert_eq!(member["relative_path"], "sub/member.txt");
            let link = member["content"].as_str().unwrap();
            assert!(link.contains("?member_id="));
            assert!(!link.contains("member.txt"));
            let response = f.request("GET", link).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(read_body(response).await, b"member");
        }
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            assert_eq!(
                f.request(method, &base).await.status(),
                StatusCode::METHOD_NOT_ALLOWED
            );
        }
        assert_eq!(
            f.request(
                "GET",
                &format!("{base}/content?member_id={}", "0".repeat(64))
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            f.request("GET", &format!("{base}/content?path=../../config.yaml"))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            f.request("GET", &format!("/api/v1/runs/other-run/documents/{id}"))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        f.request("GET", "/api/v1/runs/review-run/documents/not-an-id")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot_tree(&f.home), before);
    assert_eq!(snapshot_tree(&f.workspace), workspace_before);
}

#[tokio::test]
async fn document_reviews_fail_closed_on_corrupt_blobs_symlinks_and_forged_root_lineage() {
    let f = tokio::task::spawn_blocking(Documents::new).await.unwrap();
    let id = f.revisions[0]["revision_id"].as_str().unwrap();
    let link = format!("/api/v1/runs/review-run/documents/{id}/content");
    let root = transist_storage_local::run_paths::resolve_run_root(&f.home, "review-run").unwrap();
    let blob = root
        .join("documents/blobs")
        .join(f.revisions[0]["blob"]["sha256"].as_str().unwrap());
    let original = fs::read(&blob).unwrap();
    fs::write(&blob, "corrupt").unwrap();
    let response = f.request("GET", &link).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        read_json(response).await["error"]["message"],
        "failed to load inspection data"
    );
    fs::write(&blob, original).unwrap();
    #[cfg(unix)]
    {
        fs::remove_file(&blob).unwrap();
        std::os::unix::fs::symlink(f.home.join("config.yaml"), &blob).unwrap();
        assert_eq!(
            f.request("GET", &link).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        fs::remove_file(&blob).unwrap();
        fs::write(&blob, b"exact review bytes").unwrap();
    }
    let state_path = root.join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["lineage"]["root_run_id"] = "other-run".into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        f.request("GET", &link).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn document_reviews_decision_history_exposes_the_exact_typed_request() {
    let f = tokio::task::spawn_blocking(Documents::new).await.unwrap();
    let workspace = f.workspace.clone();
    let home = f.home.clone();
    tokio::task::spawn_blocking(move || {
        let ctx = transist::TransistContext::with_home(workspace, home);
        ctx.record_gate_decision_with_input(
            "review-run",
            None,
            transist::GateDecision::Approved,
            transist::GateDecisionInput {
                actor: Some("test-reviewer".into()),
                reason: Some("offline fixture".into()),
                ..Default::default()
            },
        )
        .unwrap();
    })
    .await
    .unwrap();
    let before = snapshot_tree(&f.home);
    let path = "/api/v1/runs/review-run/gates";
    let response = f.request("GET", path).await;
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let gates: Value = serde_json::from_slice(&bytes).unwrap();
    let decision = &gates["data"][0]["decision"];
    assert_eq!(
        decision["review_request"]["review_documents"],
        json!(f.revisions)
    );
    assert_eq!(decision, &gates["data"][0]["decision_history"][0]);
    assert_contract_http_response(
        &load_openapi_contract(),
        "GET",
        path,
        axum::response::Response::from_parts(parts, Body::from(bytes)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(snapshot_tree(&f.home), before);
    let root = transist_storage_local::run_paths::resolve_run_root(&f.home, "review-run").unwrap();
    let history = fs::read_dir(root.join("gates/history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut tampered: Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    tampered["review_request"]["execution"] = 99.into();
    fs::write(&history, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert_eq!(
        f.request("GET", path).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn document_reviews_review3_html_escapes_guidance_and_member_ids_are_revision_scoped() {
    let f = tokio::task::spawn_blocking(|| {
        let f = Documents::new();
        let ctx = transist::TransistContext::with_home(&f.workspace, &f.home);
        let flow = f.home.parent().unwrap().join("scope.yaml");
        fs::write(&flow, format!("schema: transist.flow.v1\nid: content.scope\nversion: 0.1.0\nplugins: {{fixture.documents: 0.1.0}}\nworkspace: {{path: '{}', access: read_only}}\ndocuments:\n  first: {{kind: tree, max_bytes: 4096, max_files: 3}}\n  second: {{kind: tree, max_bytes: 4096, max_files: 3}}\nsteps:\n  - id: produce\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: first, access: write}}, op: tree, content: same}}, {{path: {{$document: second, access: write}}, op: tree, content: same}}]}}\n  - id: review\n    control: control.gate.manual\n    with: {{review_documents: [first, second], prompt: '<script>alert(1)</script>'}}\n", f.workspace.display())).unwrap();
        ctx.compile_flow_file(&flow).unwrap();
        ctx.prepare_workflow("content.scope@0.1.0", Some("scope-run"), BTreeMap::new()).unwrap();
        assert_eq!(ctx.run_started_run("scope-run").unwrap().status, "blocked");
        f
    }).await.unwrap();
    let before = snapshot_tree(&f.home);
    let html = read_text(f.request("GET", "/inspect/runs/scope-run?tab=gates").await).await;
    let escaped = askama::filters::escape("<script>alert(1)</script>", askama::filters::Html)
        .unwrap()
        .to_string();
    assert!(html.contains(&escaped));
    assert!(!html.contains("<script>alert(1)</script>"));
    let gates = read_json(f.request("GET", "/api/v1/runs/scope-run/gates").await).await;
    let mut metadata = Vec::new();
    for row in gates["data"][0]["review_documents"].as_array().unwrap() {
        metadata.push(
            read_json(
                f.request("GET", row["links"]["metadata"].as_str().unwrap())
                    .await,
            )
            .await,
        );
    }
    assert_eq!(metadata.len(), 2);
    assert_eq!(
        metadata[0]["revision"]["blob"],
        metadata[1]["revision"]["blob"]
    );
    assert_ne!(
        metadata[0]["revision"]["revision_id"],
        metadata[1]["revision"]["revision_id"]
    );
    let member = &metadata[0]["members"][0];
    assert_eq!(
        member["relative_path"],
        metadata[1]["members"][0]["relative_path"]
    );
    assert_ne!(member["member_id"], metadata[1]["members"][0]["member_id"]);
    assert_eq!(
        read_body(f.request("GET", member["content"].as_str().unwrap()).await).await,
        b"same"
    );
    let other_revision = format!(
        "{}?member_id={}",
        metadata[1]["links"]["content"].as_str().unwrap(),
        member["member_id"].as_str().unwrap()
    );
    assert_eq!(
        f.request("GET", &other_revision).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(snapshot_tree(&f.home), before);
}
