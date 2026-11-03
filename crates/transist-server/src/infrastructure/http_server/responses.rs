// SPDX-License-Identifier: MPL-2.0
use super::transport::{sanitize_text, sanitized_json};
use crate::application::{ApiProblem, BlobContentResponse, InlineContentResponse};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};

pub(crate) fn blob_content_response(payload: BlobContentResponse) -> Response {
    let mut response = payload.bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&payload.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", payload.file_name))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    response
}

pub(crate) fn inline_content_response(
    payload: InlineContentResponse,
) -> Result<Response, ApiProblem> {
    let mut response = sanitize_text(payload.bytes)
        .map_err(|_| ApiProblem::invalid_text_encoding())?
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&payload.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("text/plain; charset=utf-8")),
    );
    Ok(response)
}

pub(crate) fn api_problem(problem: ApiProblem) -> Response {
    (problem.status, sanitized_json(problem.into_envelope())).into_response()
}
