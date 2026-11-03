// SPDX-License-Identifier: MPL-2.0
//! Shared sanitized JSON presentation for step outputs and run previews.

use crate::infrastructure::http_server::transport::sanitize_json;
use serde_json::Value;

const INLINE_LIMIT_BYTES: usize = 65_536;
const PREVIEW_LIMIT_SCALARS: usize = 512;

/// Sanitized, bounded pretty JSON for a run-page preview, escaped by templates.
#[derive(Debug, Default)]
pub(crate) struct OutputPreview {
    pub text: Option<String>,
    pub truncated: bool,
}

/// Formats an optional output after transport sanitization, retaining at most
/// 512 Unicode scalar values and marking truncation only when content is omitted.
pub(crate) fn present_preview(value: Option<&Value>) -> OutputPreview {
    let Some(value) = value else {
        return OutputPreview::default();
    };
    let (_, pretty) = sanitized_output(value);
    let end = pretty
        .char_indices()
        .nth(PREVIEW_LIMIT_SCALARS)
        .map(|(offset, _)| offset);
    OutputPreview {
        text: Some(pretty[..end.unwrap_or(pretty.len())].to_string()),
        truncated: end.is_some(),
    }
}

/// Plain template text for one root value or alphabetically ordered object field.
#[derive(Debug)]
pub(crate) struct OutputField {
    pub label: Option<String>,
    pub text: String,
    pub expandable: bool,
}

/// Bounded step-page presentation; templates must HTML-escape all text.
#[derive(Debug, Default)]
pub(crate) struct OutputView {
    pub fields: Vec<OutputField>,
    pub truncated_json: Option<String>,
}

/// Sanitizes persisted JSON using the API transport rules before formatting.
/// Returns the sanitized value and its complete pretty JSON for shared previews.
pub(crate) fn sanitized_output(value: &Value) -> (Value, String) {
    let value = sanitize_json(value.clone());
    let pretty = serde_json::to_string_pretty(&value).expect("JSON values serialize");
    (value, pretty)
}

/// Presents an optional persisted value without conflating null with no result.
/// Oversized sanitized JSON is cut at a valid UTF-8 boundary; no files are read.
pub(crate) fn present_output(value: Option<&Value>) -> OutputView {
    let Some(value) = value else {
        return OutputView::default();
    };
    let (value, pretty) = sanitized_output(value);
    if pretty.len() > INLINE_LIMIT_BYTES {
        let mut end = INLINE_LIMIT_BYTES;
        while !pretty.is_char_boundary(end) {
            end -= 1;
        }
        return OutputView {
            fields: Vec::new(),
            truncated_json: Some(pretty[..end].to_string()),
        };
    }
    let fields = match &value {
        Value::Object(object) if !object.is_empty() => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            entries
                .into_iter()
                .map(|(key, value)| output_field(Some(key.clone()), value))
                .collect()
        }
        _ => vec![output_field(None, &value)],
    };
    OutputView {
        fields,
        truncated_json: None,
    }
}

fn output_field(label: Option<String>, value: &Value) -> OutputField {
    OutputField {
        label,
        text: match value {
            Value::String(text) if !text.is_empty() => text.clone(),
            _ => serde_json::to_string_pretty(value).expect("JSON values serialize"),
        },
        expandable: value.is_object() || value.is_array(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn step_outputs_presenter_every_root_type_and_presence() {
        assert!(present_output(None).fields.is_empty());
        for (value, text, expandable) in [
            (Value::Null, "null", false),
            (json!(false), "false", false),
            (json!(true), "true", false),
            (json!(0), "0", false),
            (json!(-1.5), "-1.5", false),
            (json!(""), "\"\"", false),
            (json!("First\nSecond"), "First\nSecond", false),
            (json!({}), "{}", true),
            (json!([]), "[]", true),
            (json!([1, null]), "[\n  1,\n  null\n]", true),
        ] {
            let view = present_output(Some(&value));
            assert!(view.truncated_json.is_none());
            assert_eq!(view.fields.len(), 1);
            assert_eq!(view.fields[0].label, None);
            assert_eq!(view.fields[0].text, text);
            assert_eq!(view.fields[0].expandable, expandable);
        }
    }

    #[test]
    fn step_outputs_presenter_sorted_fields_and_nested_types() {
        let value = json!({"z": [], "a": "Fictional advice\nSecond line", "b": {"n": 1}, "c": null, "d": false, "e": 0, "f": ""});
        let view = present_output(Some(&value));
        assert_eq!(
            view.fields
                .iter()
                .map(|f| f.label.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e", "f", "z"]
        );
        assert_eq!(view.fields[0].text, "Fictional advice\nSecond line");
        assert!(view.fields[1].expandable);
        assert_eq!(view.fields[1].text, "{\n  \"n\": 1\n}");
        assert_eq!(view.fields[5].text, "\"\"");
    }

    #[test]
    fn step_outputs_presenter_redaction_matches_transport_before_formatting() {
        let value = json!({"/tmp/private/key": {"nested": ["/home/testuser/data", "../private", "C:\\private\\file", "~/private"]}, "url": "https://example.test/a?path=/tmp/private&x=1&x=2#frag"});
        let (sanitized, pretty) = sanitized_output(&value);
        assert_eq!(sanitized, sanitize_json(value.clone()));
        assert_eq!(serde_json::from_str::<Value>(&pretty).unwrap(), sanitized);
        let view = present_output(Some(&value));
        assert_eq!(view.fields[0].label.as_deref(), Some("[path redacted]"));
        assert!(!view.fields[0].text.contains("private"));
        assert_eq!(view.fields[1].text, value["url"].as_str().unwrap());
        // A large raw path becomes small before deciding whether to truncate.
        assert!(
            present_output(Some(&json!(format!("/tmp/{}", "x".repeat(70_000)))))
                .truncated_json
                .is_none()
        );
    }

    #[test]
    fn step_outputs_presenter_exact_inline_byte_boundaries() {
        for size in [65_535, 65_536, 65_537] {
            let value = json!("a".repeat(size - 2));
            let view = present_output(Some(&value));
            assert_eq!(view.truncated_json.is_some(), size > INLINE_LIMIT_BYTES);
            if let Some(text) = view.truncated_json {
                assert_eq!(text.len(), INLINE_LIMIT_BYTES);
                assert!(view.fields.is_empty());
            }
        }
        // The threshold counts the entire pretty object, including keys and
        // indentation, rather than only the rendered field text.
        for size in [65_536, 65_537] {
            let overhead = serde_json::to_string_pretty(&json!({"response_text": ""}))
                .unwrap()
                .len();
            let value = json!({"response_text": "a".repeat(size - overhead)});
            assert_eq!(sanitized_output(&value).1.len(), size);
            let view = present_output(Some(&value));
            assert_eq!(view.truncated_json.is_some(), size > INLINE_LIMIT_BYTES);
            assert_eq!(view.fields.len(), usize::from(size <= INLINE_LIMIT_BYTES));
        }
    }

    #[test]
    fn step_outputs_presenter_multibyte_cut_is_exact_valid_prefix() {
        for prefix in 0..4 {
            let value = json!(format!("{}{}", "a".repeat(prefix), "🦀".repeat(20_000)));
            let (_, pretty) = sanitized_output(&value);
            let view = present_output(Some(&value));
            let text = view.truncated_json.unwrap();
            assert!((65_533..=65_536).contains(&text.len()));
            assert!(pretty.starts_with(&text));
            assert!(
                text.len() + pretty[text.len()..].chars().next().unwrap().len_utf8()
                    > INLINE_LIMIT_BYTES
            );
        }
    }
}
