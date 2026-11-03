// SPDX-License-Identifier: MPL-2.0
//! Explicit action-data projection at the adapter boundary.

use anyhow::Result;
use serde_json::Value;
use transist_plugins::ActionDescriptor;

/// Clones input for schema validation and transport, removing only top-level
/// `secret_refs` when explicitly opted in. The authority-bearing original is
/// never mutated, and absent bindings preserve the legacy value exactly.
pub(crate) fn project_action_input(action: &ActionDescriptor, input: &Value) -> Result<Value> {
    let enabled = action.action_data_projection_enabled()?;
    let mut projected = input.clone();
    if enabled && let Some(object) = projected.as_object_mut() {
        object.remove("secret_refs");
    }
    Ok(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(binding: Option<Value>) -> ActionDescriptor {
        let mut action: ActionDescriptor = serde_json::from_value(json!({
            "name": "query", "input_schema": "input.json", "output_schema": "output.json"
        }))
        .unwrap();
        if let Some(binding) = binding {
            action.bindings.insert("input_projection".into(), binding);
        }
        action
    }

    #[test]
    fn legacy_projection_preserves_serialized_bytes_for_every_input_shape() {
        for input in [
            json!({"secret_refs": ["KEY"], "unknown": {"secret_refs": ["nested"]}}),
            json!([{"secret_refs": []}]),
            json!(null),
            json!(" text "),
            json!(42),
            json!(false),
        ] {
            let projected = project_action_input(&action(None), &input).unwrap();
            assert_eq!(
                serde_json::to_vec(&projected).unwrap(),
                serde_json::to_vec(&input).unwrap()
            );
        }
    }

    #[test]
    fn opt_in_is_immutable_top_level_only_and_retains_unknown_and_injected_fields() {
        let input = json!({"secret_refs": ["KEY"], "query": " text ",
            "unknown": {"secret_refs": ["nested"]}, "prompt": "prompt", "skills": [],
            "gate_decision": {"decision": "approve"}});
        let original = serde_json::to_vec(&input).unwrap();
        let projected =
            project_action_input(&action(Some(json!("action_data_v1"))), &input).unwrap();
        let mut expected = input.clone();
        expected.as_object_mut().unwrap().remove("secret_refs");
        assert_eq!(projected, expected);
        assert_eq!(serde_json::to_vec(&input).unwrap(), original);
        for input in [json!([{"secret_refs": []}]), json!(null), json!("text")] {
            assert_eq!(
                project_action_input(&action(Some(json!("action_data_v1"))), &input).unwrap(),
                input
            );
        }
    }

    #[test]
    fn malformed_and_unknown_projection_bindings_fail_closed() {
        for binding in [
            json!(null),
            json!(true),
            json!(1),
            json!([]),
            json!({}),
            json!(""),
            json!("action_data_v2"),
            json!(" action_data_v1"),
            json!("action_data_v1 "),
        ] {
            assert!(project_action_input(&action(Some(binding)), &json!({})).is_err());
        }
    }
}
