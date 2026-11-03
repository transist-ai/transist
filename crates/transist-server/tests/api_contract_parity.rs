// SPDX-License-Identifier: MPL-2.0
//! Bidirectional parity between the companion API route inventory, the
//! registered Axum router, and the published OpenAPI contract, plus the
//! read-only method proof for every `/api/v1` route.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use std::collections::BTreeSet;
use std::path::PathBuf;
use tower::ServiceExt;
use transist_server::{COMPANION_API_ROUTES, INSPECTION_GET_ROUTES, app_for_paths};
use yaml_serde::Value;

fn openapi_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/http/companion-api.v1.openapi.yaml")
}

fn load_openapi() -> Value {
    let raw = std::fs::read_to_string(openapi_path()).expect("read companion OpenAPI contract");
    yaml_serde::from_str(&raw).expect("parse companion OpenAPI contract")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteKey {
    method: String,
    path: String,
}

fn openapi_route_keys(doc: &Value) -> Vec<(RouteKey, String)> {
    let mut out = Vec::new();
    let paths = doc
        .get("paths")
        .and_then(Value::as_mapping)
        .expect("OpenAPI paths object");
    for (path, methods) in paths {
        let path = path.as_str().expect("path key").to_string();
        let method_map = methods.as_mapping().expect("path item mapping");
        for (method, op) in method_map {
            let operation_id = op
                .get("operationId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push((
                RouteKey {
                    method: method.as_str().expect("method key").to_ascii_uppercase(),
                    path: path.clone(),
                },
                operation_id,
            ));
        }
    }
    out
}

fn inventory_route_keys() -> Vec<(RouteKey, String)> {
    COMPANION_API_ROUTES
        .iter()
        .map(|route| {
            (
                RouteKey {
                    method: route.method.as_str().to_ascii_uppercase(),
                    path: route.path.to_string(),
                },
                route.operation_id.to_string(),
            )
        })
        .collect()
}

#[test]
fn inventory_and_openapi_have_bidirectional_route_parity() {
    let openapi = load_openapi();
    let openapi_keys: BTreeSet<RouteKey> = openapi_route_keys(&openapi)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let inventory_keys: BTreeSet<RouteKey> =
        inventory_route_keys().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        inventory_keys, openapi_keys,
        "companion API route inventory and OpenAPI contract must agree exactly"
    );
}

#[test]
fn inventory_and_openapi_operation_ids_match() {
    let openapi = load_openapi();
    let openapi_by_path: std::collections::BTreeMap<String, String> = openapi_route_keys(&openapi)
        .into_iter()
        .map(|(k, op)| (k.path, op))
        .collect();
    for (key, operation_id) in inventory_route_keys() {
        let mapped = openapi_by_path
            .get(&key.path)
            .unwrap_or_else(|| panic!("OpenAPI missing route {}", key.path));
        assert_eq!(
            mapped, &operation_id,
            "operationId mismatch for route {}",
            key.path
        );
    }
}

#[test]
fn companion_api_exposes_only_read_only_methods() {
    for route in COMPANION_API_ROUTES {
        assert_eq!(
            route.method,
            Method::GET,
            "companion API route {} must be GET (read-only)",
            route.path
        );
    }
    let openapi = load_openapi();
    for (key, _) in openapi_route_keys(&openapi) {
        assert_eq!(
            key.method, "GET",
            "OpenAPI operation on {} must be GET (read-only)",
            key.path
        );
    }
}

#[test]
fn workflow_file_id_schema_matches_the_lowercase_parser_contract() {
    let openapi = load_openapi();
    let pattern = openapi["components"]["parameters"]["WorkflowFileId"]["schema"]["pattern"]
        .as_str()
        .expect("workflow file id pattern");
    assert_eq!(pattern, r"^wf_[0-9a-f]{64}$");
}

#[test]
fn workflow_file_locator_kind_schema_matches_emitted_values() {
    let openapi = load_openapi();
    let expected = BTreeSet::from(["repository_state", "run", "workspace"]);
    for schema in ["WorkflowFileOutputRef", "WorkflowFileMetadata"] {
        let actual = openapi["components"]["schemas"][schema]["properties"]["locator_kind"]["enum"]
            .as_sequence()
            .expect("locator_kind enum")
            .iter()
            .map(|value| value.as_str().expect("locator kind string"))
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "OpenAPI locator kinds for {schema}");
    }
}

#[test]
fn workflow_text_endpoints_document_invalid_text_encoding() {
    let openapi = load_openapi();
    for path in [
        "/api/v1/workflows/{workflow_ref}/source",
        "/api/v1/workflows/{workflow_ref}/compiled-recipe",
    ] {
        let response = &openapi["paths"][path]["get"]["responses"]["422"];
        assert_eq!(
            response["$ref"].as_str(),
            Some("#/components/responses/Problem"),
            "{path} must document the standard 422 problem response"
        );
    }
}

#[test]
fn router_builds_every_api_inventory_route_without_panicking() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Building the app walks every registered handler; an
    // unmapped operation_id would hit `unreachable!` and fail this test.
    let _app = app_for_paths(tmp.path().to_path_buf(), tmp.path().join("home"));
}

#[test]
fn registered_get_route_inventory_has_no_duplicates_and_router_builds_from_it() {
    let routes = COMPANION_API_ROUTES
        .iter()
        .chain(INSPECTION_GET_ROUTES.iter());
    let keys: BTreeSet<RouteKey> = routes
        .clone()
        .map(|route| RouteKey {
            method: route.method.as_str().to_ascii_uppercase(),
            path: route.path.to_string(),
        })
        .collect();
    assert_eq!(
        keys.len(),
        COMPANION_API_ROUTES.len() + INSPECTION_GET_ROUTES.len()
    );
    assert!(
        COMPANION_API_ROUTES
            .iter()
            .chain(INSPECTION_GET_ROUTES.iter())
            .all(|route| route.method == Method::GET)
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let _app = app_for_paths(tmp.path().to_path_buf(), tmp.path().join("home"));
}

#[tokio::test]
async fn mutating_methods_are_rejected_on_registered_routes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = app_for_paths(tmp.path().to_path_buf(), tmp.path().join("home"));
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri("/api/v1/runs")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /api/v1/runs must be rejected as read-only"
        );
    }
}
