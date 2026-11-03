// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use transist_protocol::KernelValue;

/// Runtime input map used by lenient placeholder rendering.
pub type Inputs = BTreeMap<String, KernelValue>;

/// One discovered input reference from a template-like string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRef {
    /// Referenced input key.
    pub key: String,
}

/// Collects `{{inputs.<key>}}` and bare `inputs.<key>` references from text.
#[must_use]
pub fn collect_input_refs_from(value: &str) -> Vec<InputRef> {
    let mut refs: Vec<InputRef> = Vec::new();

    let mut rest = value;
    while let Some(start) = rest.find("{{inputs.") {
        let after = &rest[start + "{{inputs.".len()..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let key = after[..end].trim();
        if !key.is_empty() && !refs.iter().any(|item| item.key == key) {
            refs.push(InputRef {
                key: key.to_string(),
            });
        }
        rest = &after[end + "}}".len()..];
    }

    let mut search_from = 0;
    while let Some(found) = value[search_from..].find("inputs.") {
        let start = search_from + found;
        let preceded_by_identifier = value[..start]
            .chars()
            .next_back()
            .map(is_input_ref_identifier_char)
            .unwrap_or(false);
        let after = &value[start + "inputs.".len()..];
        let key_len = after
            .chars()
            .take_while(|ch| is_input_ref_identifier_char(*ch))
            .map(char::len_utf8)
            .sum::<usize>();
        if !preceded_by_identifier && key_len > 0 {
            let key = &after[..key_len];
            if !refs.iter().any(|item| item.key == key) {
                refs.push(InputRef {
                    key: key.to_string(),
                });
            }
        }
        search_from = start + "inputs.".len();
    }

    refs
}

/// Renders `{{inputs.<key>}}` placeholders while preserving unresolved values.
#[must_use]
pub fn render_input_placeholders_lenient(raw: &str, workflow_inputs: &Inputs) -> String {
    if !raw.contains("{{inputs.") {
        return raw.to_string();
    }
    let mut rendered = String::new();
    let mut rest = raw;
    while let Some(start_idx) = rest.find("{{inputs.") {
        rendered.push_str(&rest[..start_idx]);
        let after = &rest[start_idx + "{{inputs.".len()..];
        let Some(end_idx) = after.find("}}") else {
            rendered.push_str(&rest[start_idx..]);
            return rendered;
        };
        let key = after[..end_idx].trim();
        let placeholder_end = start_idx + "{{inputs.".len() + end_idx + 2;
        match workflow_inputs.get(key) {
            Some(KernelValue::String(value)) => rendered.push_str(value),
            Some(KernelValue::Bool(value)) => rendered.push_str(&value.to_string()),
            Some(KernelValue::Number(value)) => rendered.push_str(&value.to_string()),
            Some(KernelValue::Array(_) | KernelValue::Object(_))
            | Some(KernelValue::Null)
            | None => rendered.push_str(&rest[start_idx..placeholder_end]),
        }
        rest = &rest[placeholder_end..];
    }
    rendered.push_str(rest);
    rendered
}

fn is_input_ref_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}
