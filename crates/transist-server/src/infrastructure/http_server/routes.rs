// SPDX-License-Identifier: MPL-2.0
//! Canonical inventory of the companion server GET HTTP surface.
//!
//! `COMPANION_API_ROUTES` and `INSPECTION_GET_ROUTES` are the sources used by
//! the HTTP router for every registered GET route, including health, metrics,
//! and the server-rendered inspection UI. The OpenAPI contract covers the
//! `/api/v1` subset.
//!
//! Every entry uses the `GET` method: the companion API is strictly read-only
//! and must not expose mutating routes.

use axum::http::Method;

/// One registered companion server GET route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRoute {
    /// HTTP method. Every current companion server route is `GET`.
    pub method: Method,
    /// Axum path template, e.g. `/api/v1/runs/{run_id}`.
    pub path: &'static str,
    /// Stable operation identifier mapped to a handler in `router`.
    pub operation_id: &'static str,
}

/// Every `/api/v1` route exposed by the companion server, in stable order.
pub static COMPANION_API_ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs",
        operation_id: "listRuns",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}",
        operation_id: "getRun",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/trust",
        operation_id: "getRunTrust",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/locality",
        operation_id: "getRunLocality",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/events",
        operation_id: "listRunEvents",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/steps",
        operation_id: "listRunSteps",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/steps/{step_id}",
        operation_id: "getRunStep",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/gates",
        operation_id: "listRunGates",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/documents/{revision_id}",
        operation_id: "getRunDocument",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/documents/{revision_id}/content",
        operation_id: "getRunDocumentContent",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/failures",
        operation_id: "listRunFailures",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/failures/{failure_id}",
        operation_id: "getRunFailure",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/artifacts",
        operation_id: "listRunArtifacts",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/artifacts/{artifact_id}",
        operation_id: "getRunArtifact",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/artifacts/{artifact_id}/content",
        operation_id: "getRunArtifactContent",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/evidence",
        operation_id: "listRunEvidence",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/evidence/{evidence_id}",
        operation_id: "getRunEvidence",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/evidence/{evidence_id}/content",
        operation_id: "getRunEvidenceContent",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}",
        operation_id: "getRunWorkflowFile",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content",
        operation_id: "getRunWorkflowFileContent",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/workflows",
        operation_id: "listWorkflows",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/workflows/{workflow_ref}",
        operation_id: "getWorkflow",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/workflows/{workflow_ref}/source",
        operation_id: "getWorkflowSource",
    },
    ApiRoute {
        method: Method::GET,
        path: "/api/v1/workflows/{workflow_ref}/compiled-recipe",
        operation_id: "getWorkflowCompiledRecipe",
    },
];

/// Every non-API GET route exposed by the companion server, in stable order.
pub static INSPECTION_GET_ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: Method::GET,
        path: "/health",
        operation_id: "health",
    },
    ApiRoute {
        method: Method::GET,
        path: "/metrics",
        operation_id: "metrics",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect",
        operation_id: "inspectRoot",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs",
        operation_id: "inspectRuns",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}",
        operation_id: "inspectRun",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}/failures",
        operation_id: "inspectRunFailures",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}/steps/{step_id}",
        operation_id: "inspectStep",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}/artifacts/{artifact_id}",
        operation_id: "inspectArtifact",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}/evidence/{evidence_id}",
        operation_id: "inspectEvidence",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/runs/{run_id}/workflow-files/{workflow_file_id}",
        operation_id: "inspectWorkflowFile",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/workflows",
        operation_id: "inspectWorkflows",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/workflows/{workflow_ref}",
        operation_id: "inspectWorkflow",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/workflows/{workflow_ref}/source",
        operation_id: "inspectWorkflowSource",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/workflows/{workflow_ref}/compiled",
        operation_id: "inspectWorkflowCompiled",
    },
    ApiRoute {
        method: Method::GET,
        path: "/inspect/static/{file}",
        operation_id: "inspectStatic",
    },
];
