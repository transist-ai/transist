// SPDX-License-Identifier: MPL-2.0
use super::{ExplanationChange, ExplanationComparison, WorkflowExplanation};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeSet;
use transist_protocol::binding_pointer_segment;

/// Compares compiled explanation content in deterministic JSON-pointer order.
/// Arrays retain declared order; no semantic equivalence or opaque-effect claim is made.
pub fn compare_explanations(
    previous: &WorkflowExplanation,
    current: &WorkflowExplanation,
) -> Result<ExplanationComparison> {
    let mut before = serde_json::to_value(previous)?;
    let mut after = serde_json::to_value(current)?;
    before
        .as_object_mut()
        .expect("typed view")
        .remove("comparison");
    after
        .as_object_mut()
        .expect("typed view")
        .remove("comparison");
    let mut changes = Vec::new();
    diff(Some(&before), Some(&after), "", &mut changes);
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ExplanationComparison {
        previous_definition_sha256: previous.definition_sha256.clone(),
        notice: "Structural content comparison only; no semantic equivalence or inferred external effects.".into(),
        changes,
    })
}

fn diff(
    before: Option<&Value>,
    after: Option<&Value>,
    path: &str,
    changes: &mut Vec<ExplanationChange>,
) {
    if before == after {
        return;
    }
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        let keys = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
        for key in keys {
            diff(
                before.get(key),
                after.get(key),
                &format!("{path}/{}", binding_pointer_segment(key)),
                changes,
            );
        }
    } else {
        let category =
            if path.starts_with("/pins/") || path.contains("/pin") || path.contains("/contract") {
                "pins"
            } else if path.starts_with("/assets/") || path.contains("/prompt/") {
                "assets"
            } else if path.contains("/authority_requests/") {
                "authority"
            } else if path.contains("/gate")
                || path.contains("review_documents")
                || path.contains("reviewed_by")
                || path.contains("approval")
            {
                "gates"
            } else if path.contains("/loop_limits/") || path.contains("max_iterations") {
                "limits"
            } else if path.ends_with("/action") {
                "actions"
            } else if path.contains("/bindings/")
                || path.contains("/bound_bindings/")
                || path.contains("/documents")
                || path.contains("/variable_assignments/")
                || path.contains("/outputs/")
            {
                "bindings"
            } else {
                "structure"
            };
        changes.push(ExplanationChange {
            category: category.into(),
            path: path.into(),
            before: before.cloned(),
            after: after.cloned(),
        });
    }
}
