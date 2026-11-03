// SPDX-License-Identifier: MPL-2.0
//! Hermetic step-output HTML tests against the real inspection storage and Axum.

use super::step_outputs_api::{execution_path, record, state};
use super::*;

const PAGE: &str = "/inspect/runs/inspect-run/steps/collect";
const API: &str = "/api/v1/runs/inspect-run/steps/collect";

async fn page(fixture: &Fixture, suffix: &str, status: StatusCode) -> String {
    let response = fixture
        .request("GET", &format!("{PAGE}{suffix}"), Body::empty())
        .await;
    assert_eq!(response.status(), status);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    read_text(response).await
}

fn outputs(html: &str) -> &str {
    html.split("<section class=\"step-outputs\"")
        .nth(1)
        .unwrap()
        .split("</section>")
        .next()
        .unwrap()
}

fn escape(text: &str) -> String {
    askama::filters::escape(text, askama::filters::Html)
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn step_outputs_ui_selected_history_latest_status_and_read_only() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional diagnostic advice\nTry a different approach"})),
    );
    record(
        &fixture,
        2,
        "success",
        Some(json!({"response_text": "Fictional revised advice"})),
    );
    state(&mut fixture, 2, KernelStepStatus::Succeeded, None);
    let before = snapshot_tree(&fixture.home_dir);
    for suffix in ["", "?execution=latest", "?execution=2", "?execution=0002"] {
        let html = page(&fixture, suffix, StatusCode::OK).await;
        let output = outputs(&html);
        assert!(output.contains("Selected output execution: 2"));
        assert!(output.contains("status-success\">success"));
        assert!(output.contains("<h3 class=\"output-field\">response_text</h3>"));
        assert!(output.contains("Fictional revised advice"));
        assert!(!output.contains("Fictional diagnostic advice"));
        let selector = suffix.strip_prefix("?execution=").unwrap_or("latest");
        assert!(output.contains(&format!("href=\"{API}?execution={selector}\"")));
        assert!(output.contains(&format!("<option value=\"{selector}\" selected>")));
    }
    let html = page(&fixture, "?execution=1", StatusCode::OK).await;
    let output = outputs(&html);
    assert!(output.contains("Selected output execution: 1"));
    assert!(output.contains("status-failed\">failed"));
    assert!(output.contains("Fictional diagnostic advice\nTry a different approach"));
    assert!(!output.contains("Fictional revised advice"));
    assert!(html.contains("<span class=\"status-badge status-success\">success</span>"));
    assert!(html.contains("Latest step input summary"));
    assert!(html.contains("Latest step output summary"));
    assert!(html.contains("(no metrics)"));
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_ui_all_json_types_sorted_fields_and_escaped_text() {
    let fixture = Fixture::new();
    let attack = "<script>alert(1)</script>\n<img src=x onerror=\"alert(2)\"> **Markdown** [link](https://example.test) & text";
    let value = json!({"z": [1, null], "a": attack, "b": {"nested": attack}, "c": null, "d": false, "e": 0, "f": "", "g": {}, "h": []});
    record(&fixture, 1, "success", Some(value));
    let html = page(&fixture, "", StatusCode::OK).await;
    let output = outputs(&html);
    let positions = ["a", "b", "c", "d", "e", "f", "g", "h", "z"].map(|key| {
        output
            .find(&format!("<h3 class=\"output-field\">{key}</h3>"))
            .unwrap()
    });
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    let api = read_json(fixture.request("GET", API, Body::empty()).await).await;
    assert!(output.contains(&escape(api["output"]["value"]["a"].as_str().unwrap())));
    assert!(output.contains(&escape("<script>")));
    assert!(output.contains(&escape("<img src=x onerror=\"alert(2)\">")));
    assert!(output.contains("**Markdown**"));
    assert!(!output.contains("<script>"));
    assert!(!output.contains("<img"));
    assert!(!output.contains("<strong>Markdown</strong>"));
    assert!(output.contains("<details><summary>Show JSON</summary>"));
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!(false),
        json!(true),
        json!(0),
        json!(-1.5),
        json!(""),
        json!(attack),
        json!([attack]),
    ] {
        record(&fixture, 1, "failed", Some(value.clone()));
        let html = page(&fixture, "?execution=1", StatusCode::OK).await;
        let output = outputs(&html);
        let api = read_json(fixture.request("GET", API, Body::empty()).await).await;
        let sanitized = &api["output"]["value"];
        let expected = match sanitized {
            Value::String(text) if !text.is_empty() => text.clone(),
            _ => serde_json::to_string_pretty(sanitized).unwrap(),
        };
        assert!(output.contains(&format!(
            "<pre class=\"output-value\">{}</pre>",
            escape(&expected)
        )));
        assert!(!output.contains("No persisted result"));
    }
}

#[tokio::test]
async fn step_outputs_ui_redaction_parity_keys_nested_paths_urls_and_escaping() {
    let fixture = Fixture::new();
    let value = json!({"/tmp/private/key": {"paths": ["/home/testuser/private", "C:\\private\\key", "~/private", "../private"]}, "C:\\private\\key": "secret /tmp/private/data", "<img onerror=\"alert(1)\">": "Fictional advice", "url": "https://example.test/a?path=/tmp/private&x=1&x=2#frag", "http": "http://example.test/a"});
    record(&fixture, 1, "success", Some(value));
    let before = snapshot_tree(&fixture.home_dir);
    let api = read_json(fixture.request("GET", API, Body::empty()).await).await;
    let html = page(&fixture, "", StatusCode::OK).await;
    let output = outputs(&html);
    for (key, value) in api["output"]["value"].as_object().unwrap() {
        assert!(output.contains(&format!("<h3 class=\"output-field\">{}</h3>", escape(key))));
        let text = value
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap());
        assert!(output.contains(&escape(&text)));
    }
    for path in [
        "/home/testuser",
        "C:\\private",
        "~/private",
        "../private",
        "/tmp/private/key",
        "/tmp/private/data",
    ] {
        assert!(!output.contains(path));
    }
    assert!(!output.contains("<img"));
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_ui_pending_inflight_no_result_and_legacy() {
    let mut fixture = Fixture::new();
    fs::remove_file(execution_path(&fixture, 1)).unwrap();
    state(&mut fixture, 0, KernelStepStatus::Pending, None);
    let html = page(&fixture, "", StatusCode::OK).await;
    assert!(outputs(&html).contains("Selected output execution: none"));
    assert!(outputs(&html).contains("status-pending\">pending"));
    assert!(outputs(&html).contains("No persisted result for this execution."));
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional earlier advice"})),
    );
    state(
        &mut fixture,
        2,
        KernelStepStatus::Running,
        Some(sample_collect_result()),
    );
    let html = page(&fixture, "", StatusCode::OK).await;
    assert!(outputs(&html).contains("Selected output execution: 2"));
    assert!(outputs(&html).contains("status-running\">running"));
    assert!(outputs(&html).contains("No persisted result for this execution."));
    assert!(!outputs(&html).contains("Fictional earlier advice"));
    assert!(!outputs(&html).contains("<option value=\"2\""));
    page(&fixture, "?execution=2", StatusCode::NOT_FOUND).await;
    record(&fixture, 2, "interrupted", None);
    let html = page(&fixture, "?execution=2", StatusCode::OK).await;
    assert!(outputs(&html).contains("status-interrupted\">interrupted"));
    assert!(outputs(&html).contains("No persisted result for this execution."));
    let mut result = sample_collect_result();
    result.execution = 2;
    result.output = json!({"response_text": "Fictional legacy advice"});
    state(
        &mut fixture,
        2,
        KernelStepStatus::Failed,
        Some(result.clone()),
    );
    let html = page(&fixture, "?execution=2", StatusCode::OK).await;
    assert!(outputs(&html).contains("No persisted result for this execution."));
    assert!(outputs(&html).contains("status-interrupted\">interrupted"));
    assert!(!outputs(&html).contains("Fictional legacy advice"));
    fs::remove_file(execution_path(&fixture, 2)).unwrap();
    let html = page(&fixture, "?execution=2", StatusCode::OK).await;
    assert!(outputs(&html).contains("Fictional legacy advice"));
    assert!(outputs(&html).contains("<code>state_snapshot</code>"));
    assert!(outputs(&html).contains("status-failed\">failed"));
    result.step_id = "other".into();
    state(&mut fixture, 2, KernelStepStatus::Failed, Some(result));
    let html = page(&fixture, "", StatusCode::OK).await;
    assert!(outputs(&html).contains("No persisted result for this execution."));
    assert!(!outputs(&html).contains("Fictional legacy advice"));
}

#[tokio::test]
async fn step_outputs_ui_selector_errors_missing_resources_and_storage_errors() {
    let fixture = Fixture::new();
    let before = snapshot_tree(&fixture.home_dir);
    for selector in [
        "",
        "%20",
        "1%20",
        "%091",
        "+1",
        "%2B1",
        "-1",
        "0",
        "000",
        "one",
        "LATEST",
        "1.0",
        "4294967296",
        "99999999999999999999999",
        "%D9%A1",
        "latest&execution=1",
    ] {
        let html = page(
            &fixture,
            &format!("?execution={selector}"),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert!(html.contains("invalid_request"));
    }
    for selector in ["3", "4294967295"] {
        assert!(
            page(
                &fixture,
                &format!("?execution={selector}"),
                StatusCode::NOT_FOUND
            )
            .await
            .contains("resource_not_found")
        );
    }
    for path in [
        "/inspect/runs/missing/steps/collect",
        "/inspect/runs/inspect-run/steps/missing",
    ] {
        let response = fixture.request("GET", path, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
    fs::write(
        execution_path(&fixture, 1),
        b"malformed /tmp/private/storage",
    )
    .unwrap();
    let before = snapshot_tree(&fixture.home_dir);
    let html = page(&fixture, "", StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(html.contains("storage_query_failure"));
    assert!(html.contains("failed to load inspection data"));
    assert!(!html.contains("/tmp/private"));
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_ui_inline_boundaries_utf8_and_complete_exact_selector_link() {
    let fixture = Fixture::new();
    for value in [
        json!("a".repeat(65_533)),
        json!("a".repeat(65_534)),
        json!("a".repeat(65_535)),
        json!(format!("aa{}", "🦀".repeat(20_000))),
        json!({"response_text": "🦀".repeat(20_000)}),
    ] {
        record(&fixture, 1, "failed", Some(value.clone()));
        let before = snapshot_tree(&fixture.home_dir);
        let html = page(&fixture, "?execution=0001&iteration=all", StatusCode::OK).await;
        let output = outputs(&html);
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        assert_eq!(output.contains("Output truncated"), pretty.len() > 65_536);
        if pretty.len() > 65_536 {
            let mut end = 65_536;
            while !pretty.is_char_boundary(end) {
                end -= 1;
            }
            assert!(output.contains(&format!(
                "<pre class=\"output-value\">{}</pre>",
                escape(&pretty[..end])
            )));
            assert!(!output.contains("<h3 class=\"output-field\">"));
        }
        let link = format!("{API}?execution=0001");
        assert!(output.contains(&format!("href=\"{link}\"")));
        let api = read_json(fixture.request("GET", &link, Body::empty()).await).await;
        assert_eq!(api["output"]["value"], value);
        assert_eq!(api["output"]["status"], "failed");
        assert_eq!(snapshot_tree(&fixture.home_dir), before);
    }
    record(
        &fixture,
        1,
        "success",
        Some(json!(format!("/tmp/{}", "a".repeat(70_000)))),
    );
    let html = page(&fixture, "", StatusCode::OK).await;
    assert!(outputs(&html).contains("[path redacted]"));
    assert!(!outputs(&html).contains("Output truncated"));
}

#[tokio::test]
async fn step_outputs_ui_forms_preserve_iteration_and_existing_resource_links() {
    let fixture = Fixture::new();
    for iteration in ["all", "last", "1", "0001"] {
        let html = page(
            &fixture,
            &format!("?execution=0001&iteration={iteration}"),
            StatusCode::OK,
        )
        .await;
        assert!(outputs(&html).contains("<form method=\"get\""));
        assert!(outputs(&html).contains(&format!(
            "<input type=\"hidden\" name=\"iteration\" value=\"{iteration}\">"
        )));
        assert!(html.contains("<input type=\"hidden\" name=\"execution\" value=\"0001\">"));
        assert!(html.contains("Artifact/evidence iteration filter"));
        assert!(html.contains("Show artifacts/evidence from"));
        assert!(html.contains("Workflow files"));
        assert!(html.contains("/inspect/runs/inspect-run/workflow-files/"));
        assert!(html.contains("/inspect/runs/inspect-run/artifacts/"));
        assert!(html.contains("/inspect/runs/inspect-run/evidence/"));
        assert!(!outputs(&html).contains("onclick"));
        assert!(!outputs(&html).contains("onchange"));
    }
    let html = page(&fixture, "", StatusCode::OK).await;
    assert!(outputs(&html).contains("name=\"iteration\" value=\"last\""));
    assert!(outputs(&html).contains("<option value=\"latest\" selected>"));
}

#[tokio::test]
async fn step_outputs_ui_selectable_options_sorted_deduplicated_and_u32_max() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        10,
        "failed",
        Some(json!({"response_text": "Fictional advice"})),
    );
    record(&fixture, u32::MAX, "success", Some(Value::Null));
    fs::copy(
        execution_path(&fixture, 1),
        execution_path(&fixture, 1).with_file_name("execution_0001.json"),
    )
    .unwrap();
    state(&mut fixture, 1, KernelStepStatus::Succeeded, None);
    let before = snapshot_tree(&fixture.home_dir);
    let html = page(
        &fixture,
        "?execution=0004294967295&iteration=all",
        StatusCode::OK,
    )
    .await;
    let output = outputs(&html);
    let values = ["latest", "1", "10", "0004294967295"].map(|value| {
        let option = format!("<option value=\"{value}\"");
        assert_eq!(output.matches(&option).count(), 1);
        output.find(&option).unwrap()
    });
    assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(output.contains("Selected output execution: 4294967295"));
    assert!(output.contains("<option value=\"0004294967295\" selected>"));
    let link = format!("{API}?execution=0004294967295");
    assert!(output.contains(&format!("href=\"{link}\"")));
    let response = fixture.request("GET", &link, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = read_json(response).await;
    assert_eq!(json["output"]["execution"], u32::MAX);
    assert_eq!(json["output"]["value"], Value::Null);
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}
