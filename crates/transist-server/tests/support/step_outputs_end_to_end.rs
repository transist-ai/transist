// SPDX-License-Identifier: MPL-2.0
//! Integrated persisted-output inspection through local storage and Axum only.

use super::step_outputs_api::{record, state};
use super::*;

#[tokio::test]
async fn step_outputs_end_to_end_failed_retry_preview_selected_html_and_complete_json_are_read_only()
 {
    let mut fixture = Fixture::new();
    let failed = json!({"response_text": "Fictional diagnostic advice\nTry a different approach"});
    let succeeded = json!({
        "response_text": format!("Fictional revised advice\n{}\nComplete output tail", "🦀".repeat(20_000)),
        "trace": {"/tmp/private/key": "/home/testuser/private"},
    });
    record(&fixture, 1, "failed", Some(failed.clone()));
    record(&fixture, 2, "success", Some(succeeded.clone()));
    state(&mut fixture, 2, KernelStepStatus::Succeeded, None);
    let mut sanitized_success = succeeded;
    sanitized_success["trace"] = json!({"[path redacted]": "[path redacted]"});
    let home_before = snapshot_tree(&fixture.home_dir);
    let workspace_before = snapshot_tree(&fixture.repo_root);

    let run = fixture
        .request("GET", "/inspect/runs/inspect-run?tab=steps", Body::empty())
        .await;
    assert_eq!(run.status(), StatusCode::OK);
    let run_html = read_text(run).await;
    let preview = run_html
        .split("<tr class=\"step-output-preview\">")
        .skip(1)
        .map(|row| row.split("</tr>").next().unwrap())
        .find(|row| row.contains("<span class=\"mono\">collect</span>"))
        .expect("collect preview");
    let pretty = serde_json::to_string_pretty(&sanitized_success).unwrap();
    let bounded: String = pretty.chars().take(512).collect();
    let escaped = askama::filters::escape(&bounded, askama::filters::Html)
        .unwrap()
        .to_string();
    assert!(preview.contains(&format!("<pre class=\"output-text\">{escaped}</pre>")));
    assert!(preview.contains("Output execution: 2"));
    assert!(preview.contains("status-success\">success"));
    assert!(preview.contains("Preview truncated"));
    assert!(!preview.contains("Fictional diagnostic advice"));
    assert!(!preview.contains("Complete output tail"));
    let detail_link = "/inspect/runs/inspect-run/steps/collect?execution=2";
    assert!(preview.contains(&format!("href=\"{detail_link}\"")));

    for (ordinal, status, expected, excluded) in [
        (1, "failed", &failed, "Fictional revised advice"),
        (
            2,
            "success",
            &sanitized_success,
            "Fictional diagnostic advice",
        ),
    ] {
        let link = format!("/inspect/runs/inspect-run/steps/collect?execution={ordinal}");
        let response = fixture.request("GET", &link, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let html = read_text(response).await;
        let outputs = html
            .split("<section class=\"step-outputs\"")
            .nth(1)
            .unwrap()
            .split("</section>")
            .next()
            .unwrap();
        assert!(outputs.contains(&format!("Selected output execution: {ordinal}")));
        assert!(outputs.contains(&format!("status-{status}\">{status}")));
        assert!(outputs.contains("<code>execution_record</code>"));
        assert!(outputs.contains("Output execution"));
        assert!(outputs.contains(&format!("<option value=\"{ordinal}\" selected>")));
        assert!(!outputs.contains(excluded));
        assert!(html.contains("Latest step output summary"));
        assert!(html.contains("<span class=\"status-badge status-success\">success</span>"));
        if ordinal == 1 {
            assert!(outputs.contains("<h3 class=\"output-field\">response_text</h3>"));
            assert!(outputs.contains(expected["response_text"].as_str().unwrap()));
            assert!(!outputs.contains("Output truncated"));
        } else {
            assert!(outputs.contains("Output truncated"));
            assert!(!outputs.contains("Complete output tail"));
            let mut end = 65_536;
            while !pretty.is_char_boundary(end) {
                end -= 1;
            }
            let escaped = askama::filters::escape(&pretty[..end], askama::filters::Html)
                .unwrap()
                .to_string();
            assert!(outputs.contains(&format!("<pre class=\"output-value\">{escaped}</pre>")));
        }

        // Follow the selected HTML view's complete-JSON link, including the
        // oversized successful output's tail that neither HTML surface shows.
        let api_link = format!("/api/v1/runs/inspect-run/steps/collect?execution={ordinal}");
        assert!(outputs.contains(&format!("href=\"{api_link}\"")));
        let response = fixture.request("GET", &api_link, Body::empty()).await;
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_contract_http_response(
            &load_openapi_contract(),
            "GET",
            &api_link,
            axum::response::Response::from_parts(parts, Body::from(bytes)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(json["status"], "success");
        assert_eq!(json["output_executions"], json!([1, 2]));
        assert_eq!(
            json["output"],
            json!({
                "execution": ordinal, "status": status, "source": "execution_record",
                "result_present": true, "value": expected,
            })
        );
        assert_eq!(snapshot_tree(&fixture.home_dir), home_before);
        assert_eq!(snapshot_tree(&fixture.repo_root), workspace_before);
    }
}
