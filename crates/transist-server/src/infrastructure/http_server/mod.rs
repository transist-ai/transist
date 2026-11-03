// SPDX-License-Identifier: MPL-2.0
//! Read-only HTTP transport for the companion server.

mod handlers;
mod params;
mod responses;
mod router;
pub mod routes;
mod transport;
mod views;

pub use router::{ServerConfig, app_for_paths, app_with_query, app_with_services, run_server};
pub use routes::{ApiRoute, COMPANION_API_ROUTES, INSPECTION_GET_ROUTES};

#[cfg(test)]
mod transport_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_route_is_available() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app_for_paths(tmp.path().to_path_buf(), tmp.path().join("home"));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
