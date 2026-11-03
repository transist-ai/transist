// SPDX-License-Identifier: MPL-2.0
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::OnceLock;
use transist_protocol::KernelValue;

const FORBIDDEN_KEYS_JSON: &str = include_str!("../../../schemas/inspection/forbidden_keys.json");

#[derive(Debug, Deserialize, Serialize)]
struct ForbiddenKeysFile {
    version: u64,
    forbidden_keys: Vec<String>,
}

/// Returns the shared set of forbidden metadata keys for inspection snapshots.
#[must_use]
pub fn forbidden_keys() -> &'static BTreeSet<&'static str> {
    static FORBIDDEN_KEYS: OnceLock<BTreeSet<&'static str>> = OnceLock::new();

    FORBIDDEN_KEYS.get_or_init(|| {
        parse_forbidden_keys_file()
            .forbidden_keys
            .into_iter()
            .map(|key| Box::leak(key.into_boxed_str()) as &'static str)
            .collect()
    })
}

/// Returns the path segments to the first forbidden key found in a nested kernel value.
#[must_use]
pub fn contains_forbidden_key_recursive(value: &KernelValue) -> Option<Vec<String>> {
    contains_forbidden_key_recursive_at(value, &mut Vec::new())
}

fn parse_forbidden_keys_file() -> ForbiddenKeysFile {
    serde_json::from_str(FORBIDDEN_KEYS_JSON).expect("forbidden_keys.json must be valid")
}

fn contains_forbidden_key_recursive_at(
    value: &KernelValue,
    path: &mut Vec<String>,
) -> Option<Vec<String>> {
    match value {
        KernelValue::Array(values) => {
            for (index, item) in values.iter().enumerate() {
                path.push(format!("[{index}]"));
                let violation = contains_forbidden_key_recursive_at(item, path);
                path.pop();
                if violation.is_some() {
                    return violation;
                }
            }
            None
        }
        KernelValue::Object(values) => {
            for (key, item) in values {
                path.push(key.clone());
                if forbidden_keys().contains(key.as_str()) {
                    return Some(path.clone());
                }

                let violation = contains_forbidden_key_recursive_at(item, path);
                path.pop();
                if violation.is_some() {
                    return violation;
                }
            }
            None
        }
        KernelValue::Null
        | KernelValue::Bool(_)
        | KernelValue::Number(_)
        | KernelValue::String(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FORBIDDEN_KEYS_JSON, ForbiddenKeysFile, contains_forbidden_key_recursive, forbidden_keys,
    };
    use std::collections::BTreeMap;
    use transist_protocol::KernelValue;

    #[test]
    fn redaction_detects_shallow_forbidden_key() {
        let value = KernelValue::Object(BTreeMap::from([(
            "prompt".to_string(),
            KernelValue::String("secret".to_string()),
        )]));

        assert_eq!(
            contains_forbidden_key_recursive(&value),
            Some(vec!["prompt".to_string()])
        );
    }

    #[test]
    fn redaction_detects_deeply_nested_forbidden_key() {
        let value = KernelValue::Object(BTreeMap::from([(
            "summary".to_string(),
            KernelValue::Object(BTreeMap::from([(
                "details".to_string(),
                KernelValue::Object(BTreeMap::from([(
                    "messages".to_string(),
                    KernelValue::Array(vec![KernelValue::String("secret".to_string())]),
                )])),
            )])),
        )]));

        assert_eq!(
            contains_forbidden_key_recursive(&value),
            Some(vec![
                "summary".to_string(),
                "details".to_string(),
                "messages".to_string(),
            ])
        );
    }

    #[test]
    fn redaction_detects_forbidden_key_inside_array_of_objects() {
        let value = KernelValue::Object(BTreeMap::from([(
            "events".to_string(),
            KernelValue::Array(vec![KernelValue::Object(BTreeMap::from([(
                "body".to_string(),
                KernelValue::String("secret".to_string()),
            )]))]),
        )]));

        assert_eq!(
            contains_forbidden_key_recursive(&value),
            Some(vec![
                "events".to_string(),
                "[0]".to_string(),
                "body".to_string(),
            ])
        );
    }

    #[test]
    fn redaction_ignores_safe_values() {
        let value = KernelValue::Object(BTreeMap::from([
            ("status".to_string(), KernelValue::String("ok".to_string())),
            (
                "items".to_string(),
                KernelValue::Array(vec![KernelValue::Object(BTreeMap::from([(
                    "step_id".to_string(),
                    KernelValue::String("compile".to_string()),
                )]))]),
            ),
        ]));

        assert_eq!(contains_forbidden_key_recursive(&value), None);
    }

    #[test]
    fn redaction_forbidden_keys_json_round_trips() {
        let parsed: ForbiddenKeysFile =
            serde_json::from_str(FORBIDDEN_KEYS_JSON).expect("parse forbidden keys file");
        let json = serde_json::to_string(&parsed).expect("serialize forbidden keys file");
        let reparsed: ForbiddenKeysFile =
            serde_json::from_str(&json).expect("reparse forbidden keys file");

        assert_eq!(parsed.version, reparsed.version);
        assert_eq!(parsed.forbidden_keys, reparsed.forbidden_keys);
        assert_eq!(forbidden_keys().len(), parsed.forbidden_keys.len());
    }
}
