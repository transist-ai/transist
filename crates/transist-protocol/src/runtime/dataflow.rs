// SPDX-License-Identifier: MPL-2.0
//! Explicit compiled data dependencies, independent of compiler and plugin code.

use crate::{KernelValue, RecipePromptSource, RecipeStep};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Compiler-owned `with` metadata; removed before action input is delivered.
pub const DEFERRED_BINDINGS_KEY: &str = "__transist_deferred_bindings";
/// Compiler-owned module call metadata, including its named export bindings.
pub const SUBFLOW_BINDINGS_KEY: &str = "__transist_subflow";

/// One runtime-dependent binding retained in a compiled recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredBinding {
    /// JSON pointer into action data (or child inputs).
    pub path: String,
    /// Authored selector, without template braces.
    pub selector: String,
    /// Whether the complete value preserves its source JSON type.
    pub whole_value: bool,
}

/// Checks selector syntax without recipe identities, returning the first `.out.` split.
/// Use `split_output_selector_for_ids` for authoritative dotted-identity resolution.
pub fn split_output_selector(selector: &str) -> Result<(&str, &str), String> {
    let reference = selector
        .strip_prefix("steps.")
        .ok_or("expected steps selector")?;
    let (id, path) = reference
        .split_once(".out.")
        .ok_or("expected steps.<id>.out.<field>")?;
    crate::validate_identifier(crate::IdentifierKind::StepId, id)
        .map_err(|error| error.to_string())?;
    if path.split('.').any(|s| {
        s.is_empty()
            || s.chars()
                .any(|c| c.is_whitespace() || matches!(c, '{' | '}' | '[' | ']'))
    }) {
        return Err(format!("malformed output selector '{selector}'"));
    }
    Ok((id, path))
}

/// Resolves a selector against exact recipe-local identities, including dotted IDs.
/// Unknown identities and selectors matching more than one identity are rejected;
/// external data never chooses an implicit interpretation of an ambiguous selector.
pub fn split_output_selector_for_ids<'a, 'b>(
    selector: &'a str,
    ids: impl IntoIterator<Item = &'b str>,
) -> Result<(&'a str, &'a str), String> {
    split_output_selector(selector)?;
    let reference = selector.strip_prefix("steps.").expect("validated prefix");
    let mut selected = None;
    for id in ids {
        if let Some(path) = reference
            .strip_prefix(id)
            .and_then(|suffix| suffix.strip_prefix(".out."))
        {
            if selected.is_some() {
                return Err(format!("ambiguous output selector '{selector}'"));
            }
            selected = Some((&reference[..id.len()], path));
        }
    }
    selected.ok_or_else(|| format!("unknown step in output selector '{selector}'"))
}

/// Parses template selectors and rejects incomplete or nested template syntax.
pub fn binding_selectors(raw: &str) -> Result<Vec<&str>, String> {
    let mut selectors = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find("{{") {
        if rest[..start].contains("}}") {
            return Err("unmatched template closing delimiter".into());
        }
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or("unclosed binding template")?;
        let selector = after[..end].trim();
        if selector.is_empty() || selector.contains(['{', '}']) {
            return Err("malformed binding template".into());
        }
        selectors.push(selector);
        rest = &after[end + 2..];
    }
    if rest.contains("}}") {
        return Err("unmatched template closing delimiter".into());
    }
    Ok(selectors)
}

/// Escapes an object key for a JSON pointer.
pub fn binding_pointer_segment(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Collects ordinary post-birth dependencies recursively, retaining exact locations.
pub fn collect_deferred_bindings(
    value: &KernelValue,
    path: &str,
) -> Result<Vec<DeferredBinding>, String> {
    let mut bindings = Vec::new();
    match value {
        KernelValue::String(raw) => {
            let selectors = binding_selectors(raw)?;
            for selector in &selectors {
                if selector.starts_with("steps.") {
                    split_output_selector(selector)?;
                }
                if selector.starts_with("steps.") || selector.starts_with("vars.") {
                    bindings.push(DeferredBinding {
                        path: path.into(),
                        selector: (*selector).into(),
                        whole_value: selectors.len() == 1
                            && raw.trim() == format!("{{{{{selector}}}}}"),
                    });
                }
            }
        }
        KernelValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                bindings.extend(collect_deferred_bindings(
                    value,
                    &format!("{path}/{index}"),
                )?);
            }
        }
        KernelValue::Object(values) => {
            for (key, value) in values {
                bindings.extend(collect_deferred_bindings(
                    value,
                    &format!("{path}/{}", binding_pointer_segment(key)),
                )?);
            }
        }
        _ => {}
    }
    Ok(bindings)
}

/// Returns current authored dependency locations, excluding compiler metadata.
pub fn step_deferred_bindings(step: &RecipeStep) -> Result<Vec<DeferredBinding>, String> {
    let values: BTreeMap<_, _> = step
        .with
        .iter()
        .filter(|(key, _)| !key.starts_with("__transist_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut bindings = collect_deferred_bindings(&KernelValue::Object(values), "")?;
    if let Some(prompt) = &step.prompt_binding {
        let value = match &prompt.source {
            RecipePromptSource::Inline { value } => value,
            RecipePromptSource::File { path } => path,
        };
        bindings.extend(collect_deferred_bindings(
            &KernelValue::String(value.clone()),
            &format!("/{}", binding_pointer_segment(&prompt.inline_field)),
        )?);
    }
    Ok(bindings)
}
