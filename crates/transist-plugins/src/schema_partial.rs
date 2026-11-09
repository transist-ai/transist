// SPDX-License-Identifier: MPL-2.0
//! Dependency-aware partial validation. Unknown values are not schema instances.

use super::{PackageSchemaRetriever, package_uri};
use anyhow::{Result, bail};
use jsonschema::{Draft, Validator};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "schema_coverage.rs"]
mod coverage;

#[derive(Default)]
pub(super) struct PartialCache {
    validators: BTreeMap<(PathBuf, String, String), Validator>,
}

enum Assessment {
    Valid,
    Deferred,
    Invalid {
        instance_path: String,
        schema_path: String,
    },
}

impl Assessment {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (invalid @ Self::Invalid { .. }, _) | (_, invalid @ Self::Invalid { .. }) => invalid,
            (Self::Deferred, _) | (_, Self::Deferred) => Self::Deferred,
            _ => Self::Valid,
        }
    }

    fn invalid(path: &str, schema_path: &str) -> Self {
        Self::Invalid {
            instance_path: path.into(),
            schema_path: schema_path.into(),
        }
    }
}

struct Context<'a> {
    root: &'a Validator,
    directory: &'a Path,
    documents: &'a [(String, Value)],
    unknown: &'a [String],
    cache: &'a mut PartialCache,
}

pub(super) fn validate(
    root: &Validator,
    directory: &Path,
    documents: &[(String, Value)],
    instance: &Value,
    unknown: &[String],
    cache: &mut PartialCache,
) -> Result<()> {
    let mut context = Context {
        root,
        directory,
        documents,
        unknown,
        cache,
    };
    let result = context.assess(&documents[0].1, &documents[0].0, "", instance, "", 0)?;
    if let Assessment::Invalid {
        instance_path,
        schema_path,
    } = result
    {
        bail!(
            "input schema validation failed at instance path '{instance_path}' and schema path '{schema_path}'"
        );
    }
    Ok(())
}

impl Context<'_> {
    fn whole_unknown(&self, path: &str) -> bool {
        self.unknown
            .iter()
            .any(|unknown| path == unknown || path.starts_with(&format!("{unknown}/")))
    }

    fn affected(&self, path: &str) -> bool {
        self.whole_unknown(path)
            || self
                .unknown
                .iter()
                .any(|unknown| unknown.starts_with(&format!("{path}/")))
    }

    fn validator(&mut self, schema: &Value, document: &str) -> Result<&Validator> {
        if document == self.documents[0].0 && std::ptr::eq(schema, &self.documents[0].1) {
            return Ok(self.root);
        }
        let key = (
            self.directory.to_path_buf(),
            document.to_string(),
            schema.to_string(),
        );
        if !self.cache.validators.contains_key(&key) {
            let mut fragment = schema.clone();
            absolutize_refs(&mut fragment, document)?;
            if let Some(object) = fragment.as_object_mut() {
                object.remove("$id");
            }
            crate::record_validation_work(crate::ValidationWork::SchemaCompilation);
            let mut options = jsonschema::options();
            options
                .with_draft(Draft::Draft202012)
                .with_retriever(PackageSchemaRetriever {
                    plugin_dir: std::fs::canonicalize(self.directory)?,
                });
            let compiled = options.build(&fragment).map_err(|error| {
                anyhow::anyhow!(
                    "invalid partial input schema at schema path '{}'",
                    error.schema_path
                )
            })?;
            self.cache.validators.insert(key.clone(), compiled);
        }
        Ok(&self.cache.validators[&key])
    }

    fn concrete(
        &mut self,
        schema: &Value,
        document: &str,
        schema_path: &str,
        instance: &Value,
        path: &str,
    ) -> Result<Assessment> {
        if let Err(mut errors) = self.validator(schema, document)?.validate(instance) {
            let error = errors.next().expect("nonempty schema errors");
            return Ok(Assessment::Invalid {
                instance_path: format!("{path}{}", error.instance_path),
                schema_path: format!("{document}#{schema_path}{}", error.schema_path),
            });
        }
        Ok(Assessment::Valid)
    }

    #[allow(clippy::too_many_arguments)] // Schema and instance locations remain separate.
    fn assess(
        &mut self,
        schema: &Value,
        document: &str,
        schema_path: &str,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<Assessment> {
        if schema == &Value::Bool(false) {
            return Ok(Assessment::invalid(
                path,
                &format!("{document}#{schema_path}"),
            ));
        }
        if schema == &Value::Bool(true) {
            return Ok(Assessment::Valid);
        }
        if !self.affected(path) {
            return self.concrete(schema, document, schema_path, instance, path);
        }
        if depth > 64 {
            return Ok(Assessment::Deferred);
        }

        let whole_unknown = self.whole_unknown(path);
        let has_local_assertions = schema.as_object().is_some_and(|object| {
            object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "$id"
                        | "$schema"
                        | "$comment"
                        | "$defs"
                        | "definitions"
                        | "title"
                        | "description"
                        | "default"
                        | "examples"
                        | "deprecated"
                        | "readOnly"
                        | "writeOnly"
                        | "$ref"
                        | "allOf"
                        | "anyOf"
                        | "oneOf"
                        | "not"
                        | "if"
                        | "then"
                        | "else"
                )
            })
        });
        let mut outcome = if whole_unknown && has_local_assertions {
            Assessment::Deferred
        } else {
            Assessment::Valid
        };
        if !whole_unknown {
            // These constraints depend on known container shape, not unknown values.
            // Value-dependent keywords and applicators are evaluated below instead
            // of discarding a composite error because some descendant is unknown.
            let mut atomic = schema.clone();
            if let Some(object) = atomic.as_object_mut() {
                for keyword in [
                    "$ref",
                    "allOf",
                    "anyOf",
                    "oneOf",
                    "not",
                    "if",
                    "then",
                    "else",
                    "properties",
                    "patternProperties",
                    "additionalProperties",
                    "propertyNames",
                    "items",
                    "prefixItems",
                    "contains",
                    "minContains",
                    "maxContains",
                    "dependentSchemas",
                    "unevaluatedProperties",
                    "unevaluatedItems",
                    "enum",
                    "const",
                    "uniqueItems",
                ] {
                    object.remove(keyword);
                }
            }
            outcome = outcome.and(self.concrete(&atomic, document, schema_path, instance, path)?);
            if let Some(literal) = schema.get("const") {
                outcome = outcome.and(self.equal(
                    instance,
                    literal,
                    path,
                    &format!("{document}#{schema_path}/const"),
                ));
            }
            if let Some(literals) = schema.get("enum").and_then(Value::as_array) {
                let alternatives = literals
                    .iter()
                    .map(|literal| self.equal(instance, literal, path, schema_path))
                    .collect::<Vec<_>>();
                outcome = outcome.and(alternative(
                    alternatives,
                    false,
                    path,
                    &format!("{document}#{schema_path}/enum"),
                ));
            }
            outcome =
                outcome.and(self.children(schema, document, schema_path, instance, path, depth)?);
        }

        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            if let Some((target, target_document, target_path)) =
                resolve_reference(self.documents, document, reference)?
            {
                outcome = outcome.and(self.assess(
                    target,
                    target_document,
                    &target_path,
                    instance,
                    path,
                    depth + 1,
                )?);
            } else {
                outcome = outcome.and(Assessment::Deferred);
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                let mut results = Vec::new();
                for (index, branch) in branches.iter().enumerate() {
                    results.push(self.assess(
                        branch,
                        document,
                        &format!("{schema_path}/{keyword}/{index}"),
                        instance,
                        path,
                        depth + 1,
                    )?);
                }
                let result = if keyword == "allOf" {
                    results.into_iter().fold(Assessment::Valid, Assessment::and)
                } else {
                    alternative(
                        results,
                        keyword == "oneOf",
                        path,
                        &format!("{document}#{schema_path}/{keyword}"),
                    )
                };
                outcome = outcome.and(result);
            }
        }
        if let Some(negated) = schema.get("not") {
            let result = match self.assess(
                negated,
                document,
                &format!("{schema_path}/not"),
                instance,
                path,
                depth + 1,
            )? {
                Assessment::Valid => {
                    Assessment::invalid(path, &format!("{document}#{schema_path}/not"))
                }
                Assessment::Invalid { .. } => Assessment::Valid,
                Assessment::Deferred => Assessment::Deferred,
            };
            outcome = outcome.and(result);
        }
        if let Some(condition) = schema.get("if") {
            let condition = self.assess(
                condition,
                document,
                &format!("{schema_path}/if"),
                instance,
                path,
                depth + 1,
            )?;
            let then = self.assess(
                schema.get("then").unwrap_or(&Value::Bool(true)),
                document,
                &format!("{schema_path}/then"),
                instance,
                path,
                depth + 1,
            )?;
            let otherwise = self.assess(
                schema.get("else").unwrap_or(&Value::Bool(true)),
                document,
                &format!("{schema_path}/else"),
                instance,
                path,
                depth + 1,
            )?;
            let result = match condition {
                Assessment::Valid => then,
                Assessment::Invalid { .. } => otherwise,
                Assessment::Deferred => match (then, otherwise) {
                    (Assessment::Valid, Assessment::Valid) => Assessment::Valid,
                    (Assessment::Invalid { .. }, Assessment::Invalid { .. }) => {
                        Assessment::invalid(path, &format!("{document}#{schema_path}/if"))
                    }
                    _ => Assessment::Deferred,
                },
            };
            outcome = outcome.and(result);
        }
        if !whole_unknown && !matches!(outcome, Assessment::Invalid { .. }) {
            outcome = outcome.and(coverage::validate_unevaluated(
                self,
                schema,
                document,
                schema_path,
                instance,
                path,
                depth,
            )?);
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)] // Recursive container/schema locations are independent.
    fn children(
        &mut self,
        schema: &Value,
        document: &str,
        schema_path: &str,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<Assessment> {
        let mut outcome = Assessment::Valid;
        if let Some(object) = instance.as_object() {
            for (name, value) in object {
                let escaped = transist_protocol::binding_pointer_segment(name);
                let child_path = format!("{path}/{escaped}");
                let mut matched = false;
                if let Some(property) = schema
                    .get("properties")
                    .and_then(|properties| properties.get(name))
                {
                    matched = true;
                    outcome = outcome.and(self.assess(
                        property,
                        document,
                        &format!("{schema_path}/properties/{escaped}"),
                        value,
                        &child_path,
                        depth + 1,
                    )?);
                }
                if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
                    for (pattern, property) in patterns {
                        if self
                            .validator(&json!({"pattern":pattern}), document)?
                            .is_valid(&Value::String(name.clone()))
                        {
                            matched = true;
                            outcome = outcome.and(self.assess(
                                property,
                                document,
                                &format!(
                                    "{schema_path}/patternProperties/{}",
                                    transist_protocol::binding_pointer_segment(pattern)
                                ),
                                value,
                                &child_path,
                                depth + 1,
                            )?);
                        }
                    }
                }
                if !matched && let Some(additional) = schema.get("additionalProperties") {
                    outcome = outcome.and(self.assess(
                        additional,
                        document,
                        &format!("{schema_path}/additionalProperties"),
                        value,
                        &child_path,
                        depth + 1,
                    )?);
                }
                if let Some(names) = schema.get("propertyNames") {
                    outcome = outcome.and(self.concrete(
                        names,
                        document,
                        &format!("{schema_path}/propertyNames"),
                        &Value::String(name.clone()),
                        path,
                    )?);
                }
            }
            if let Some(dependencies) = schema.get("dependentSchemas").and_then(Value::as_object) {
                for (name, dependency) in dependencies {
                    if object.contains_key(name) {
                        outcome = outcome.and(self.assess(
                            dependency,
                            document,
                            &format!(
                                "{schema_path}/dependentSchemas/{}",
                                transist_protocol::binding_pointer_segment(name)
                            ),
                            instance,
                            path,
                            depth + 1,
                        )?);
                    }
                }
            }
        }
        if let Some(items) = instance.as_array() {
            for (index, item) in items.iter().enumerate() {
                let prefix = schema
                    .get("prefixItems")
                    .and_then(|prefix| prefix.get(index));
                if let Some(selected) = prefix.or_else(|| schema.get("items")) {
                    outcome = outcome.and(self.assess(
                        selected,
                        document,
                        &if prefix.is_some() {
                            format!("{schema_path}/prefixItems/{index}")
                        } else {
                            format!("{schema_path}/items")
                        },
                        item,
                        &format!("{path}/{index}"),
                        depth + 1,
                    )?);
                }
            }
            if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
                let mut unknown = false;
                for (index, item) in items.iter().enumerate() {
                    if self.affected(&format!("{path}/{index}")) {
                        unknown = true;
                        continue;
                    }
                    for (previous, prior) in items[..index].iter().enumerate() {
                        if !self.affected(&format!("{path}/{previous}"))
                            && json_schema_equal(item, prior)
                        {
                            outcome = outcome.and(Assessment::invalid(
                                path,
                                &format!("{document}#{schema_path}/uniqueItems"),
                            ));
                        }
                    }
                }
                if unknown {
                    outcome = outcome.and(Assessment::Deferred);
                }
            }
            if let Some(contains) = schema.get("contains") {
                let mut valid = 0;
                let mut deferred = 0;
                for (index, item) in items.iter().enumerate() {
                    match self.assess(
                        contains,
                        document,
                        &format!("{schema_path}/contains"),
                        item,
                        &format!("{path}/{index}"),
                        depth + 1,
                    )? {
                        Assessment::Valid => valid += 1,
                        Assessment::Deferred => deferred += 1,
                        Assessment::Invalid { .. } => {}
                    }
                }
                let min = schema
                    .get("minContains")
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
                let max = schema
                    .get("maxContains")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                outcome = outcome.and(if valid > max || valid + deferred < min {
                    Assessment::invalid(path, &format!("{document}#{schema_path}/contains"))
                } else if deferred == 0 {
                    Assessment::Valid
                } else {
                    Assessment::Deferred
                });
            }
        }
        Ok(outcome)
    }

    fn equal(
        &self,
        instance: &Value,
        literal: &Value,
        path: &str,
        schema_path: &str,
    ) -> Assessment {
        if self.whole_unknown(path) {
            return Assessment::Deferred;
        }
        match (instance, literal) {
            (Value::Object(object), Value::Object(expected))
                if object.len() == expected.len()
                    && object.keys().all(|name| expected.contains_key(name)) =>
            {
                object
                    .iter()
                    .fold(Assessment::Valid, |result, (name, value)| {
                        result.and(self.equal(
                            value,
                            &expected[name],
                            &format!(
                                "{path}/{}",
                                transist_protocol::binding_pointer_segment(name)
                            ),
                            schema_path,
                        ))
                    })
            }
            (Value::Array(items), Value::Array(expected)) if items.len() == expected.len() => items
                .iter()
                .zip(expected)
                .enumerate()
                .fold(Assessment::Valid, |result, (index, (value, literal))| {
                    result.and(self.equal(value, literal, &format!("{path}/{index}"), schema_path))
                }),
            _ if json_schema_equal(instance, literal) => Assessment::Valid,
            _ => Assessment::invalid(path, schema_path),
        }
    }
}

// JSON Schema equality is recursive and mathematical for numbers. Convert a
// float to an integer only within the JSON integer range; never round distinct
// large integers through f64 merely to compare their representations.
fn json_schema_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            let integer = |number: &serde_json::Number| {
                number
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| number.as_u64().map(i128::from))
            };
            match (integer(left), integer(right)) {
                (Some(left), Some(right)) => left == right,
                (None, None) => left.as_f64() == right.as_f64(),
                (Some(integer), None) | (None, Some(integer)) => {
                    let float = if left.is_f64() {
                        left.as_f64()
                    } else {
                        right.as_f64()
                    }
                    .expect("finite JSON float");
                    float.fract() == 0.0
                        && float >= i64::MIN as f64
                        && float < u64::MAX as f64
                        && integer == float as i128
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_schema_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, value)| {
                    right
                        .get(name)
                        .is_some_and(|other| json_schema_equal(value, other))
                })
        }
        _ => left == right,
    }
}

fn alternative(
    results: Vec<Assessment>,
    exclusive: bool,
    path: &str,
    schema_path: &str,
) -> Assessment {
    let valid = results
        .iter()
        .filter(|result| matches!(result, Assessment::Valid))
        .count();
    let deferred = results
        .iter()
        .filter(|result| matches!(result, Assessment::Deferred))
        .count();
    if (exclusive && valid > 1) || (valid == 0 && deferred == 0) {
        Assessment::invalid(path, schema_path)
    } else if (!exclusive && valid > 0) || (exclusive && valid == 1 && deferred == 0) {
        Assessment::Valid
    } else {
        Assessment::Deferred
    }
}

fn absolutize_refs(schema: &mut Value, document: &str) -> Result<()> {
    match schema {
        Value::Object(object) => {
            if let Some(reference) = object.get_mut("$ref")
                && let Some(raw) = reference.as_str()
            {
                *reference = Value::String(package_uri(document)?.join(raw)?.to_string());
            }
            for (key, value) in object {
                if key != "const" && key != "enum" {
                    absolutize_refs(value, document)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                absolutize_refs(item, document)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_reference<'a>(
    documents: &'a [(String, Value)],
    document: &str,
    reference: &str,
) -> Result<Option<(&'a Value, &'a str, String)>> {
    let url = package_uri(document)?.join(reference)?;
    let relative = url.path().trim_start_matches('/');
    let Some((document, schema)) = documents.iter().find(|(document, _)| document == relative)
    else {
        return Ok(None);
    };
    let pointer = url.fragment().unwrap_or("");
    if pointer.is_empty() || pointer.starts_with('/') {
        return Ok(schema
            .pointer(pointer)
            .map(|schema| (schema, document.as_str(), pointer.into())));
    }
    Ok(anchor(schema, pointer, "").map(|(schema, path)| (schema, document.as_str(), path)))
}

fn anchor<'a>(value: &'a Value, name: &str, path: &str) -> Option<(&'a Value, String)> {
    if value.get("$anchor").and_then(Value::as_str) == Some(name) {
        return Some((value, path.into()));
    }
    match value {
        Value::Object(object) => object
            .iter()
            .filter(|(key, _)| key.as_str() != "const" && key.as_str() != "enum")
            .find_map(|(key, value)| {
                anchor(
                    value,
                    name,
                    &format!("{path}/{}", transist_protocol::binding_pointer_segment(key)),
                )
            }),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .find_map(|(index, value)| anchor(value, name, &format!("{path}/{index}"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review2_partial_unevaluated_properties_reject_permanent_extras() {
        let temp = tempfile::tempdir().unwrap();
        let cases = [
            (
                json!({"type":"object","properties":{"message":{}},"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"type":"object","properties":{"message":{}},"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}"}),
                true,
            ),
            (
                json!({"allOf":[{"properties":{"message":{}}}],"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"anyOf":[{"properties":{"message":{"type":"string"},"extra":{}}},{"properties":{"message":{"type":"number"}}}],"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                true,
            ),
            (
                json!({"oneOf":[{"properties":{"message":{"type":"string"},"extra":{}}},{"properties":{"message":{"type":"number"}}}],"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                true,
            ),
            (
                json!({"oneOf":[{"required":["message"]},{"properties":{"message":{"type":"string"},"extra":{}}}],"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"anyOf":[{"required":["absent"],"properties":{"extra":{}}},{"properties":{"message":{}}}],"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"if":{"properties":{"message":{"type":"string"}}},"then":{"properties":{"message":{},"extra":{}}},"else":{"properties":{"message":{}}},"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                true,
            ),
            (
                json!({"properties":{"message":{}},"unevaluatedProperties":{"type":"string"}}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"if":{"properties":{"message":{"type":"string"}}},"then":{"required":["absent"],"properties":{"extra":{}}},"else":{"properties":{"message":{}}},"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                false,
            ),
            (
                json!({"properties":{"message":{}},"patternProperties":{"^extra$":{}},"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                true,
            ),
            (
                json!({"properties":{"message":{}},"additionalProperties":true,"unevaluatedProperties":false}),
                json!({"message":"{{steps.produce.out.value}}","extra":1}),
                true,
            ),
        ];
        for (index, (schema, instance, expected)) in cases.into_iter().enumerate() {
            let file = format!("input-{index}.json");
            std::fs::write(temp.path().join(&file), schema.to_string()).unwrap();
            let mut cache = super::super::SchemaCache::default();
            assert_eq!(
                cache
                    .validate_partial(temp.path(), &file, &instance, &["/message".into()])
                    .is_ok(),
                expected,
                "{schema}"
            );
        }
    }

    #[test]
    fn review2_partial_json_numeric_equality_matches_concrete_validation() {
        let temp = tempfile::tempdir().unwrap();
        for keyword in ["const", "enum"] {
            let literal = json!({"n":1.0,"nested":[{"zero":-0.0}],"message":"hello"});
            let mut schema = json!({});
            schema[keyword] = if keyword == "const" {
                literal
            } else {
                json!([literal])
            };
            let file = format!("{keyword}.json");
            std::fs::write(temp.path().join(&file), schema.to_string()).unwrap();
            let instance =
                json!({"n":1,"nested":[{"zero":0}],"message":"{{steps.produce.out.value}}"});
            let concrete = json!({"n":1,"nested":[{"zero":0}],"message":"hello"});
            let mut cache = super::super::SchemaCache::default();
            cache
                .validate(temp.path(), &file, "input", &concrete)
                .expect("pinned validator uses numeric equality");
            cache
                .validate_partial(temp.path(), &file, &instance, &["/message".into()])
                .expect("valid numeric completion cannot be rejected as representation mismatch");
        }
    }

    #[test]
    fn review2_partial_unique_items_detect_known_numeric_duplicates_without_rounding_integers() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("unique.json"), r#"{"uniqueItems":true}"#).unwrap();
        let mut cache = super::super::SchemaCache::default();
        for known in [json!([1, 1.0]), json!([{"n":[1]},{"n":[1.0]}])] {
            let mut partial = known.as_array().unwrap().clone();
            partial.push(json!("{{steps.produce.out.value}}"));
            let mut complete = known.as_array().unwrap().clone();
            complete.push(json!("hello"));
            assert!(
                cache
                    .validate(temp.path(), "unique.json", "input", &json!(complete))
                    .is_err()
            );
            assert!(
                cache
                    .validate_partial(temp.path(), "unique.json", &json!(partial), &["/2".into()])
                    .is_err()
            );
        }
        let distinct = json!([
            9007199254740992_u64,
            9007199254740993_u64,
            "{{steps.produce.out.value}}"
        ]);
        cache
            .validate_partial(temp.path(), "unique.json", &distinct, &["/2".into()])
            .expect("adjacent large integers must remain distinct");
    }

    #[test]
    fn review2_numeric_comparison_preserves_large_integer_precision() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = super::super::SchemaCache::default();
        for (index, (left, right, expected)) in [
            (json!(1), json!(1.0), true),
            (json!(0), json!(-0.0), true),
            (
                json!(9007199254740992_u64),
                json!(9007199254740993_u64),
                false,
            ),
            (
                json!(9007199254740993_u64),
                json!(9007199254740992.0),
                false,
            ),
            (json!(i64::MAX), json!(9223372036854775808.0), false),
            (json!(u64::MAX), json!(18446744073709551616.0), false),
            (json!(i64::MIN), json!(-9223372036854775808.0), true),
            (json!(u64::MAX), json!(u64::MAX), true),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                json_schema_equal(&left, &right),
                expected,
                "{left} vs {right}"
            );
            assert_eq!(json_schema_equal(&right, &left), expected);
            let file = format!("numeric-{index}.json");
            std::fs::write(
                temp.path().join(&file),
                json!({"const":{"n":right,"message":"hello"}}).to_string(),
            )
            .unwrap();
            let partial = json!({"n":left,"message":"{{steps.produce.out.value}}"});
            assert_eq!(
                cache
                    .validate_partial(temp.path(), &file, &partial, &["/message".into()])
                    .is_ok(),
                expected,
                "exact numeric constraint: {left} vs {right}"
            );
        }
    }

    #[test]
    fn review2_unevaluated_coverage_uses_local_references_and_array_applicators() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("properties.json"),
            r#"{"$defs":{"message":{"properties":{"message":{}}}}}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("root.json"),
            r#"{"$ref":"properties.json#/$defs/message","unevaluatedProperties":false}"#,
        )
        .unwrap();
        let mut cache = super::super::SchemaCache::default();
        assert!(
            cache
                .validate_partial(
                    temp.path(),
                    "root.json",
                    &json!({"message":"{{steps.produce.out.value}}","extra":1}),
                    &["/message".into()]
                )
                .is_err()
        );
        cache
            .validate_partial(
                temp.path(),
                "root.json",
                &json!({"message":"{{steps.produce.out.value}}"}),
                &["/message".into()],
            )
            .unwrap();
        std::fs::write(
            temp.path().join("array.json"),
            r#"{"prefixItems":[{}],"unevaluatedItems":false}"#,
        )
        .unwrap();
        assert!(
            cache
                .validate_partial(
                    temp.path(),
                    "array.json",
                    &json!(["{{steps.produce.out.value}}", 1]),
                    &["/0".into()]
                )
                .is_err()
        );
        cache
            .validate_partial(
                temp.path(),
                "array.json",
                &json!(["{{steps.produce.out.value}}"]),
                &["/0".into()],
            )
            .unwrap();
    }

    #[test]
    fn review_partial_composite_constraints_preserve_known_failures_and_possible_completions() {
        let message = json!({"message":"{{steps.produce.out.value}}"});
        let cases = [
            (
                json!({"anyOf":[{"required":["fixed_a"]},{"required":["fixed_b"]}]}),
                false,
            ),
            (
                json!({"oneOf":[{"required":["fixed_a"]},{"required":["fixed_b"]}]}),
                false,
            ),
            (
                json!({"allOf":[{"required":["message"]},{"required":["fixed_a"]}]}),
                false,
            ),
            (
                json!({"oneOf":[{"required":["message"]},{"required":["message"]}]}),
                false,
            ),
            (json!({"not":{"required":["message"]}}), false),
            (
                json!({"if":{"required":["message"]},"then":{"required":["fixed_a"]}}),
                false,
            ),
            (
                json!({"if":{"properties":{"message":{"type":"string"}}},"then":{"required":["fixed_a"]},"else":{"required":["fixed_b"]}}),
                false,
            ),
            (
                json!({"anyOf":[{"required":["fixed_a"]},{"properties":{"message":{"type":"string"}}}]}),
                true,
            ),
            (
                json!({"oneOf":[{"required":["message"]},{"properties":{"message":{"type":"string"}}}]}),
                true,
            ),
            (
                json!({"if":{"properties":{"message":{"type":"string"}}},"then":{"required":["fixed_a"]},"else":{}}),
                true,
            ),
            (
                json!({"additionalProperties":false,"properties":{"fixed_a":{}}}),
                false,
            ),
            (
                json!({"patternProperties":{"^message$":{"type":"string"}},"additionalProperties":false}),
                true,
            ),
            (json!({"propertyNames":{"pattern":"^fixed_"}}), false),
            (
                json!({"dependentSchemas":{"message":{"required":["fixed_a"]}}}),
                false,
            ),
            (json!({"const":{"message":"valid","fixed_a":1}}), false),
            (
                json!({"enum":[{"message":"valid","fixed_a":1},{"message":"valid","fixed_b":1}]}),
                false,
            ),
            (json!({"properties":{"message":false}}), false),
            (
                json!({"properties":{"message":{"anyOf":[false,{"type":"string"}]}}}),
                true,
            ),
        ];
        for (index, (schema, expected)) in cases.into_iter().enumerate() {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("input.json"), schema.to_string()).unwrap();
            let mut cache = super::super::SchemaCache::default();
            for _ in 0..2 {
                assert_eq!(
                    cache
                        .validate_partial(temp.path(), "input.json", &message, &["/message".into()])
                        .is_ok(),
                    expected,
                    "case {index}: {schema}"
                );
            }
        }
    }

    #[test]
    fn review_partial_nested_local_refs_contains_and_unknown_container_shape() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("input.json"),
            r#"{"type":"object","properties":{"payload":{"$ref":"rules.json#/$defs/choices"}}}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("rules.json"), r#"{"$defs":{"choices":{"anyOf":[{"required":["fixed_a"]},{"required":["fixed_b"]}]}}}"#).unwrap();
        let mut cache = super::super::SchemaCache::default();
        let instance = json!({"payload":{"message":"{{steps.produce.out.value}}"}});
        assert!(
            cache
                .validate_partial(
                    temp.path(),
                    "input.json",
                    &instance,
                    &["/payload/message".into()]
                )
                .is_err()
        );
        // An unknown whole object can supply required keys; an unknown member cannot.
        assert!(
            cache
                .validate_partial(temp.path(), "input.json", &instance, &["/payload".into()])
                .is_ok()
        );
        std::fs::write(
            temp.path().join("array.json"),
            r#"{"type":"array","contains":{"required":["fixed_a"]}}"#,
        )
        .unwrap();
        assert!(
            cache
                .validate_partial(
                    temp.path(),
                    "array.json",
                    &json!([{"message":"{{steps.produce.out.value}}"}]),
                    &["/0/message".into()]
                )
                .is_err()
        );
        assert!(
            cache
                .validate_partial(
                    temp.path(),
                    "array.json",
                    &json!(["{{steps.produce.out.value}}"]),
                    &["/0".into()]
                )
                .is_ok()
        );
    }
}
