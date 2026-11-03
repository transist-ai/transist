// SPDX-License-Identifier: MPL-2.0
//! Evaluation annotations for partial unevaluatedProperties/unevaluatedItems checks.

use super::{Assessment, Context, resolve_reference};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Default)]
struct Coverage {
    certain: BTreeSet<String>,
    possible: BTreeSet<String>,
}

impl Coverage {
    fn include(&mut self, other: Self) {
        self.certain.extend(other.certain);
        self.possible.extend(other.possible);
    }

    fn include_possible(&mut self, other: Self) {
        self.possible.extend(other.certain);
        self.possible.extend(other.possible);
    }
}

fn members(instance: &Value) -> Vec<(String, &Value)> {
    match instance {
        Value::Object(object) => object
            .iter()
            .map(|(name, value)| (name.clone(), value))
            .collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value))
            .collect(),
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)] // Schema and instance locations are independent.
pub(super) fn validate_unevaluated(
    context: &mut Context<'_>,
    schema: &Value,
    document: &str,
    schema_path: &str,
    instance: &Value,
    path: &str,
    depth: usize,
) -> Result<Assessment> {
    let keyword = match instance {
        Value::Object(_) => "unevaluatedProperties",
        Value::Array(_) => "unevaluatedItems",
        _ => return Ok(Assessment::Valid),
    };
    let Some(constraint) = schema.get(keyword) else {
        return Ok(Assessment::Valid);
    };
    let coverage = evaluated(
        context,
        schema,
        document,
        schema_path,
        instance,
        path,
        depth,
        false,
    )?;
    let mut result = Assessment::Valid;
    for (name, value) in members(instance) {
        if coverage.certain.contains(&name) {
            continue;
        }
        if coverage.possible.contains(&name) {
            if constraint != &Value::Bool(true) {
                result = result.and(Assessment::Deferred);
            }
        } else {
            result = result.and(context.assess(
                constraint,
                document,
                &format!("{schema_path}/{keyword}"),
                value,
                &format!(
                    "{path}/{}",
                    transist_protocol::binding_pointer_segment(&name)
                ),
                depth + 1,
            )?);
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)] // Annotation collection follows both schema and instance scopes.
fn evaluated(
    context: &mut Context<'_>,
    schema: &Value,
    document: &str,
    schema_path: &str,
    instance: &Value,
    path: &str,
    depth: usize,
    include_own_unevaluated: bool,
) -> Result<Coverage> {
    let mut coverage = Coverage::default();
    if depth > 64 || context.whole_unknown(path) {
        coverage
            .possible
            .extend(members(instance).into_iter().map(|(name, _)| name));
        return Ok(coverage);
    }
    // Coverage is conditional on this schema succeeding. Its unconditional
    // applicators must succeed in every valid completion, even when their
    // value-dependent assertions are not yet decidable.
    if let Value::Object(object) = instance {
        for name in object.keys() {
            let mut matched = schema
                .get("properties")
                .and_then(|properties| properties.get(name))
                .is_some();
            if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
                for pattern in patterns.keys() {
                    matched |= context
                        .validator(&json!({"pattern":pattern}), document)?
                        .is_valid(&Value::String(name.clone()));
                }
            }
            if matched || schema.get("additionalProperties").is_some() {
                coverage.certain.insert(name.clone());
            }
        }
        if let Some(dependencies) = schema.get("dependentSchemas").and_then(Value::as_object) {
            for (name, dependency) in dependencies {
                if object.contains_key(name) {
                    coverage.include(evaluated(
                        context,
                        dependency,
                        document,
                        &format!(
                            "{schema_path}/dependentSchemas/{}",
                            transist_protocol::binding_pointer_segment(name)
                        ),
                        instance,
                        path,
                        depth + 1,
                        true,
                    )?);
                }
            }
        }
    }
    if let Value::Array(items) = instance {
        let prefix = schema
            .get("prefixItems")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        for index in 0..items.len() {
            if index < prefix || schema.get("items").is_some() {
                coverage.certain.insert(index.to_string());
            }
        }
        if let Some(contains) = schema.get("contains") {
            let mut candidates = BTreeSet::new();
            for (index, value) in items.iter().enumerate() {
                match context.assess(
                    contains,
                    document,
                    &format!("{schema_path}/contains"),
                    value,
                    &format!("{path}/{index}"),
                    depth + 1,
                )? {
                    Assessment::Valid => {
                        coverage.certain.insert(index.to_string());
                    }
                    Assessment::Deferred => {
                        candidates.insert(index.to_string());
                    }
                    Assessment::Invalid { .. } => {}
                }
            }
            coverage.possible.extend(candidates);
        }
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if let Some((target, target_document, target_path)) =
            resolve_reference(context.documents, document, reference)?
        {
            coverage.include(evaluated(
                context,
                target,
                target_document,
                &target_path,
                instance,
                path,
                depth + 1,
                true,
            )?);
        } else {
            coverage
                .possible
                .extend(members(instance).into_iter().map(|(name, _)| name));
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let mut results = Vec::new();
            for (index, branch) in branches.iter().enumerate() {
                let branch_path = format!("{schema_path}/{keyword}/{index}");
                let assessment =
                    context.assess(branch, document, &branch_path, instance, path, depth + 1)?;
                if !matches!(assessment, Assessment::Invalid { .. }) {
                    results.push((
                        assessment,
                        evaluated(
                            context,
                            branch,
                            document,
                            &branch_path,
                            instance,
                            path,
                            depth + 1,
                            true,
                        )?,
                    ));
                }
            }
            let forced_exclusive = keyword == "oneOf"
                && results
                    .iter()
                    .any(|(result, _)| matches!(result, Assessment::Valid));
            for (assessment, annotations) in results {
                if keyword == "allOf" || matches!(assessment, Assessment::Valid) {
                    coverage.include(annotations);
                } else if !forced_exclusive {
                    coverage.include_possible(annotations);
                }
            }
        }
    }
    if let Some(condition) = schema.get("if") {
        match context.assess(
            condition,
            document,
            &format!("{schema_path}/if"),
            instance,
            path,
            depth + 1,
        )? {
            Assessment::Valid => {
                coverage.include(evaluated(
                    context,
                    condition,
                    document,
                    &format!("{schema_path}/if"),
                    instance,
                    path,
                    depth + 1,
                    true,
                )?);
                if let Some(then) = schema.get("then") {
                    coverage.include(evaluated(
                        context,
                        then,
                        document,
                        &format!("{schema_path}/then"),
                        instance,
                        path,
                        depth + 1,
                        true,
                    )?);
                }
            }
            Assessment::Invalid { .. } => {
                if let Some(otherwise) = schema.get("else") {
                    coverage.include(evaluated(
                        context,
                        otherwise,
                        document,
                        &format!("{schema_path}/else"),
                        instance,
                        path,
                        depth + 1,
                        true,
                    )?);
                }
            }
            Assessment::Deferred => {
                let then_viable = !matches!(
                    context.assess(
                        schema.get("then").unwrap_or(&Value::Bool(true)),
                        document,
                        &format!("{schema_path}/then"),
                        instance,
                        path,
                        depth + 1
                    )?,
                    Assessment::Invalid { .. }
                );
                let else_viable = !matches!(
                    context.assess(
                        schema.get("else").unwrap_or(&Value::Bool(true)),
                        document,
                        &format!("{schema_path}/else"),
                        instance,
                        path,
                        depth + 1
                    )?,
                    Assessment::Invalid { .. }
                );
                let mut then = evaluated(
                    context,
                    condition,
                    document,
                    &format!("{schema_path}/if"),
                    instance,
                    path,
                    depth + 1,
                    true,
                )?;
                if let Some(branch) = schema.get("then") {
                    then.include(evaluated(
                        context,
                        branch,
                        document,
                        &format!("{schema_path}/then"),
                        instance,
                        path,
                        depth + 1,
                        true,
                    )?);
                }
                let otherwise = if let Some(branch) = schema.get("else") {
                    evaluated(
                        context,
                        branch,
                        document,
                        &format!("{schema_path}/else"),
                        instance,
                        path,
                        depth + 1,
                        true,
                    )?
                } else {
                    Coverage::default()
                };
                match (then_viable, else_viable) {
                    (true, true) => {
                        coverage
                            .certain
                            .extend(then.certain.intersection(&otherwise.certain).cloned());
                        coverage.include_possible(then);
                        coverage.include_possible(otherwise);
                    }
                    (true, false) => coverage.include(then),
                    (false, true) => coverage.include(otherwise),
                    _ => {}
                }
            }
        }
    }
    let unevaluated = if instance.is_object() {
        "unevaluatedProperties"
    } else {
        "unevaluatedItems"
    };
    if include_own_unevaluated && schema.get(unevaluated).is_some() {
        coverage
            .certain
            .extend(members(instance).into_iter().map(|(name, _)| name));
    }
    Ok(coverage)
}
