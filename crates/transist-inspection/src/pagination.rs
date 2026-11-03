// SPDX-License-Identifier: MPL-2.0
//! Pagination and query-normalization helpers for inspection APIs.

/// Splits a workflow reference of the form `<id>@<version>`.
pub fn workflow_ref_parts(raw: &str) -> Option<(String, String)> {
    let (workflow_id, workflow_version) = raw.split_once('@')?;
    let workflow_id = workflow_id.trim();
    let workflow_version = workflow_version.trim();
    if workflow_id.is_empty() || workflow_version.is_empty() {
        return None;
    }
    Some((workflow_id.to_string(), workflow_version.to_string()))
}

pub(crate) fn normalize_limit(limit: usize, default: usize, max: usize) -> usize {
    match limit {
        0 => default,
        value if value > max => max,
        value => value,
    }
}
