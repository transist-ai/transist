// SPDX-License-Identifier: MPL-2.0
//! Offline JSON Schema compilation and action input/output validation.

use crate::manifest::ActionDescriptor;
use anyhow::{Context, Result, bail};
use jsonschema::{Draft, JSONSchema, SchemaResolver, SchemaResolverError};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use url::Url;

const PACKAGE_SCHEMA_SCHEME: &str = "transist-plugin";

#[path = "schema_partial.rs"]
mod partial;

#[derive(Default)]
pub(crate) struct SchemaCache {
    roots: std::collections::BTreeMap<(PathBuf, String), JSONSchema>,
    documents: std::collections::BTreeMap<(PathBuf, String), Vec<(String, Value)>>,
    partial: partial::PartialCache,
}

impl SchemaCache {
    /// Validates known portions with the same operation-scoped compiled root.
    pub(crate) fn validate_partial(
        &mut self,
        dir: &Path,
        relative: &str,
        instance: &Value,
        deferred_paths: &[String],
    ) -> Result<()> {
        crate::record_validation_work(crate::ValidationWork::InputValidation);
        self.compile(dir, relative, "input")?;
        let key = (dir.to_path_buf(), relative.to_string());
        partial::validate(
            &self.roots[&key],
            dir,
            &self.documents[&key],
            instance,
            deferred_paths,
            &mut self.partial,
        )
    }
    /// Compiles a package-local root once and returns its operation-scoped validator.
    pub(crate) fn compile(
        &mut self,
        dir: &Path,
        relative: &str,
        kind: &str,
    ) -> Result<&JSONSchema> {
        let key = (dir.to_path_buf(), relative.to_string());
        if !self.roots.contains_key(&key) {
            let documents = load_schema_documents(dir, relative, kind)?;
            self.roots
                .insert(key.clone(), compile_documents(dir, &documents, kind)?);
            self.documents.insert(key.clone(), documents);
        }
        Ok(self.roots.get(&key).expect("compiled schema"))
    }

    /// Checks a fresh instance against a reused root; diagnostics contain paths only.
    pub(crate) fn validate(
        &mut self,
        dir: &Path,
        relative: &str,
        kind: &str,
        instance: &Value,
    ) -> Result<()> {
        crate::record_validation_work(crate::ValidationWork::InputValidation);
        let root = self.compile(dir, relative, kind)?;
        if let Err(mut errors) = root.validate(instance) {
            let error = errors.next().expect("nonempty validation errors");
            bail!(
                "{kind} schema validation failed at instance path '{}' and schema path '{}'",
                error.instance_path,
                error.schema_path
            );
        }
        Ok(())
    }
}

/// Compiles both schemas referenced by an action without accessing the network.
pub fn validate_action_schemas(plugin_dir: &Path, action: &ActionDescriptor) -> Result<()> {
    compile_schema_documents(plugin_dir, &action.input_schema, "input")?;
    compile_schema_documents(plugin_dir, &action.output_schema, "output")?;
    Ok(())
}

/// Returns every package-relative schema path loaded by an action, including
/// transitive local `$ref` targets.
pub(crate) fn action_schema_paths(
    plugin_dir: &Path,
    action: &ActionDescriptor,
) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for (relative_path, _) in load_schema_documents(plugin_dir, &action.input_schema, "input")? {
        paths.insert(relative_path);
    }
    for (relative_path, _) in load_schema_documents(plugin_dir, &action.output_schema, "output")? {
        paths.insert(relative_path);
    }
    Ok(paths.into_iter().collect())
}

/// Validates materialized action input against its package-local input schema.
pub fn validate_action_input(
    plugin_dir: &Path,
    action: &ActionDescriptor,
    instance: &Value,
) -> Result<()> {
    validate_action_instance(plugin_dir, &action.input_schema, "input", instance)
}

/// Validates a successful action output against its package-local output schema.
pub fn validate_action_output(
    plugin_dir: &Path,
    action: &ActionDescriptor,
    instance: &Value,
) -> Result<()> {
    validate_action_instance(plugin_dir, &action.output_schema, "output", instance)
}

/// Validates the statically known portion of an authored action input.
/// `deferred_paths` are JSON pointers to real unresolved bindings, never fabricated
/// instances. Only constraints whose outcome can depend on those values are deferred.
/// This performs no transport work; dispatch must still validate the full instance.
pub fn validate_action_input_partial(
    plugin_dir: &Path,
    action: &ActionDescriptor,
    instance: &Value,
    deferred_paths: &[String],
) -> Result<()> {
    crate::record_validation_work(crate::ValidationWork::InputValidation);
    let documents = load_schema_documents(plugin_dir, &action.input_schema, "input")?;
    let root = compile_documents(plugin_dir, &documents, "input")?;
    partial::validate(
        &root,
        plugin_dir,
        &documents,
        instance,
        deferred_paths,
        &mut partial::PartialCache::default(),
    )
}

/// Conservatively infers possible JSON types at a schema object/array selector.
/// Handles local transitive references, unions, null, and intersections; unsupported
/// constraints produce an unconstrained type set rather than a false incompatibility.
pub fn schema_types_at_path(
    plugin_dir: &Path,
    relative: &str,
    path: &[String],
) -> Result<BTreeSet<String>> {
    let documents = load_schema_documents(plugin_dir, relative, "dataflow")?;
    Ok(infer_schema_types(
        &documents[0].1,
        &documents[0].0,
        path,
        &documents,
        0,
    ))
}

fn all_json_types() -> BTreeSet<String> {
    ["null", "boolean", "number", "string", "array", "object"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn infer_schema_types(
    schema: &Value,
    document: &str,
    path: &[String],
    documents: &[(String, Value)],
    depth: usize,
) -> BTreeSet<String> {
    if depth > 32 {
        return all_json_types();
    }
    let mut types = all_json_types();
    if schema == &Value::Bool(false) {
        return BTreeSet::new();
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let resolved = package_uri(document)
            .and_then(|base| base.join(reference).map_err(anyhow::Error::from));
        if let Ok(url) = resolved {
            let relative = url.path().trim_start_matches('/');
            if let Some((name, root)) = documents.iter().find(|(name, _)| name == relative) {
                let target = match url.fragment() {
                    None | Some("") => Some(root),
                    Some(pointer) if pointer.starts_with('/') => root.pointer(pointer),
                    _ => None,
                };
                if let Some(target) = target {
                    types = infer_schema_types(target, name, path, documents, depth + 1);
                }
            }
        }
    }
    let direct = if let Some((first, rest)) = path.split_first() {
        let containers = infer_schema_types(schema, document, &[], documents, depth + 1);
        let mut selected = BTreeSet::new();
        if containers.contains("object") {
            selected.extend(
                schema
                    .get("properties")
                    .and_then(|properties| properties.get(first))
                    .map(|property| {
                        infer_schema_types(property, document, rest, documents, depth + 1)
                    })
                    .unwrap_or_else(all_json_types),
            );
        }
        if containers.contains("array")
            && first.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(index) = first.parse::<usize>()
        {
            selected.extend(
                schema
                    .get("prefixItems")
                    .and_then(|items| items.get(index))
                    .or_else(|| schema.get("items"))
                    .map(|item| infer_schema_types(item, document, rest, documents, depth + 1))
                    .unwrap_or_else(all_json_types),
            );
        }
        selected
    } else {
        let mut direct = all_json_types();
        if let Some(kind) = schema.get("type") {
            let names: Vec<_> = match kind {
                Value::String(s) => vec![s.as_str()],
                Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if !names.is_empty() {
                direct = names
                    .into_iter()
                    .map(|s| if s == "integer" { "number" } else { s })
                    .map(str::to_string)
                    .collect();
            }
        }
        let literals = schema
            .get("enum")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| schema.get("const").map(|v| vec![v.clone()]));
        if let Some(literals) = literals {
            let literal_types = literals
                .iter()
                .map(|v| match v {
                    Value::Null => "null",
                    Value::Bool(_) => "boolean",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                })
                .map(str::to_string)
                .collect();
            direct = direct.intersection(&literal_types).cloned().collect();
        }
        direct
    };
    types = types.intersection(&direct).cloned().collect();
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            let inferred = infer_schema_types(branch, document, path, documents, depth + 1);
            types = types.intersection(&inferred).cloned().collect();
        }
    }
    for keyword in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let union = branches
                .iter()
                .flat_map(|branch| infer_schema_types(branch, document, path, documents, depth + 1))
                .collect();
            types = types.intersection(&union).cloned().collect();
        }
    }
    types
}

fn validate_action_instance(
    plugin_dir: &Path,
    relative_schema: &str,
    schema_kind: &str,
    instance: &Value,
) -> Result<()> {
    if schema_kind == "input" {
        crate::record_validation_work(crate::ValidationWork::InputValidation);
    }
    let documents = load_schema_documents(plugin_dir, relative_schema, schema_kind)?;
    let root = compile_documents(plugin_dir, &documents, schema_kind)?;
    let Some(mut errors) = root.validate(instance).err() else {
        return Ok(());
    };
    let error = errors
        .next()
        .expect("jsonschema returned an empty error iterator");
    bail!(
        "{schema_kind} schema validation failed at instance path '{}' and schema path '{}'",
        error.instance_path,
        error.schema_path
    );
}

fn compile_schema_documents(
    plugin_dir: &Path,
    relative_schema: &str,
    schema_kind: &str,
) -> Result<()> {
    let documents = load_schema_documents(plugin_dir, relative_schema, schema_kind)?;
    compile_documents(plugin_dir, &documents, schema_kind).map(|_| ())
}

fn compile_documents(
    plugin_dir: &Path,
    documents: &[(String, Value)],
    schema_kind: &str,
) -> Result<JSONSchema> {
    crate::record_validation_work(crate::ValidationWork::SchemaCompilation);
    let root = documents
        .first()
        .expect("schema document collection always contains its root document");
    let resolver = PackageSchemaResolver {
        plugin_dir: fs::canonicalize(plugin_dir).with_context(|| {
            format!(
                "failed canonicalizing plugin directory '{}'",
                plugin_dir.display()
            )
        })?,
    };
    let mut options = JSONSchema::options();
    options
        .with_draft(Draft::Draft202012)
        .with_resolver(resolver);

    let root_schema = compile_one(&mut options, &root.1, schema_kind, &root.0)?;
    for (relative_path, document) in documents.iter().skip(1) {
        compile_one(&mut options, document, schema_kind, relative_path)?;
    }
    Ok(root_schema)
}

fn compile_one(
    options: &mut jsonschema::CompilationOptions,
    document: &Value,
    schema_kind: &str,
    relative_path: &str,
) -> Result<JSONSchema> {
    options.compile(document).map_err(|error| {
        anyhow::anyhow!(
            "invalid {schema_kind} JSON Schema '{relative_path}' at schema path '{}'",
            error.schema_path
        )
    })
}

fn load_schema_documents(
    plugin_dir: &Path,
    relative_schema: &str,
    schema_kind: &str,
) -> Result<Vec<(String, Value)>> {
    let root = fs::canonicalize(plugin_dir).with_context(|| {
        format!(
            "failed canonicalizing plugin directory '{}'",
            plugin_dir.display()
        )
    })?;
    let root_relative = validate_relative_path(relative_schema, schema_kind)?;
    let mut visited = BTreeSet::new();
    let mut documents = Vec::new();
    collect_schema_documents(
        &root,
        &root_relative,
        schema_kind,
        &mut visited,
        &mut documents,
    )?;
    Ok(documents)
}

fn collect_schema_documents(
    plugin_root: &Path,
    relative_path: &Path,
    schema_kind: &str,
    visited: &mut BTreeSet<String>,
    documents: &mut Vec<(String, Value)>,
) -> Result<()> {
    let relative = relative_path.to_string_lossy().replace('\\', "/");
    if !visited.insert(relative.clone()) {
        return Ok(());
    }
    let path = package_file(plugin_root, relative_path, schema_kind)?;
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed reading {schema_kind} schema '{relative}'"))?;
    let source: Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in {schema_kind} schema '{relative}'"))?;
    let normalized = normalize_document(&source, &relative, schema_kind)?;
    documents.push((relative.clone(), normalized));

    let mut references = Vec::new();
    collect_external_references(&source, &relative, schema_kind, &mut references)?;
    for reference in references {
        collect_schema_documents(plugin_root, &reference, schema_kind, visited, documents)?;
    }
    Ok(())
}

fn collect_external_references(
    value: &Value,
    document_path: &str,
    schema_kind: &str,
    references: &mut Vec<PathBuf>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let reference = reference.as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid {schema_kind} schema '{document_path}': $ref must be a string"
                    )
                })?;
                if !reference.is_empty() && !reference.starts_with('#') {
                    references.push(resolve_reference_path(
                        document_path,
                        reference,
                        schema_kind,
                    )?);
                }
            }
            for (key, child) in object {
                if key != "enum" && key != "const" {
                    collect_external_references(child, document_path, schema_kind, references)?;
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_external_references(child, document_path, schema_kind, references)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn resolve_reference_path(
    document_path: &str,
    reference: &str,
    schema_kind: &str,
) -> Result<PathBuf> {
    let (path_part, _) = reference.split_once('#').unwrap_or((reference, ""));
    if path_part.is_empty() {
        return Ok(PathBuf::from(document_path));
    }
    if path_part.contains('\\')
        || path_part.starts_with('/')
        || path_part.starts_with("//")
        || Url::parse(path_part).is_ok()
    {
        bail!(
            "{schema_kind} schema reference '{reference}' in '{document_path}' must be a relative package path"
        );
    }
    let path = Path::new(path_part);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!(
            "{schema_kind} schema reference '{reference}' in '{document_path}' contains a forbidden path component"
        );
    }
    let resolved = Path::new(document_path)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(path);
    Ok(resolved)
}

fn validate_relative_path(relative: &str, schema_kind: &str) -> Result<PathBuf> {
    if relative.trim().is_empty()
        || relative.contains('\\')
        || Path::new(relative).is_absolute()
        || Url::parse(relative).is_ok()
    {
        bail!("{schema_kind} schema path '{relative}' must be a relative package path");
    }
    let path = Path::new(relative);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("{schema_kind} schema path '{relative}' contains a forbidden path component");
    }
    Ok(path.to_path_buf())
}

fn package_file(plugin_root: &Path, relative_path: &Path, schema_kind: &str) -> Result<PathBuf> {
    let path = plugin_root.join(relative_path);
    let canonical = fs::canonicalize(&path).with_context(|| {
        format!(
            "failed resolving {schema_kind} schema '{}', which must remain inside the plugin package",
            relative_path.display()
        )
    })?;
    if !canonical.starts_with(plugin_root) {
        bail!(
            "{schema_kind} schema '{}' escapes the plugin package",
            relative_path.display()
        );
    }
    if !fs::metadata(&canonical)
        .with_context(|| format!("failed reading schema metadata '{}'", canonical.display()))?
        .is_file()
    {
        bail!(
            "{schema_kind} schema '{}' is not a regular file",
            relative_path.display()
        );
    }
    Ok(canonical)
}

fn normalize_document(source: &Value, relative_path: &str, schema_kind: &str) -> Result<Value> {
    let base = package_uri(relative_path)?;
    validate_nested_ids(source, true, schema_kind)?;
    let Value::Object(object) = source else {
        return Ok(source.clone());
    };
    let mut normalized = object.clone();
    if let Some(id) = object.get("$id") {
        let id = id
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{schema_kind} schema $id must be a string"))?;
        resolve_local_id(&base, id, schema_kind)?;
    }
    normalized.insert("$id".to_string(), Value::String(base.as_str().to_string()));
    Ok(Value::Object(normalized))
}

fn validate_nested_ids(value: &Value, root: bool, schema_kind: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            if !root && object.contains_key("$id") {
                bail!(
                    "{schema_kind} schema nested $id is unsupported; use package-local $ref paths"
                );
            }
            for (key, child) in object {
                if key != "enum" && key != "const" {
                    validate_nested_ids(child, false, schema_kind)?;
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                validate_nested_ids(child, false, schema_kind)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn resolve_local_id(base: &Url, id: &str, schema_kind: &str) -> Result<Url> {
    if id.contains('\\') || id.starts_with('/') || id.starts_with("//") || Url::parse(id).is_ok() {
        bail!("{schema_kind} schema $id '{id}' must be a relative package identifier");
    }
    let path_part = id.split_once('#').map(|(path, _)| path).unwrap_or(id);
    let path = Path::new(path_part);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("{schema_kind} schema $id '{id}' contains a forbidden path component");
    }
    let resolved = base
        .join(id)
        .with_context(|| format!("invalid {schema_kind} schema $id '{id}'"))?;
    if resolved.scheme() != PACKAGE_SCHEMA_SCHEME {
        bail!("{schema_kind} schema $id '{id}' is outside the plugin package");
    }
    Ok(resolved)
}

fn package_uri(relative_path: &str) -> Result<Url> {
    Url::parse(&format!("{PACKAGE_SCHEMA_SCHEME}:///{relative_path}"))
        .context("failed creating package-local schema URI")
}

struct PackageSchemaResolver {
    plugin_dir: PathBuf,
}

impl SchemaResolver for PackageSchemaResolver {
    fn resolve(
        &self,
        _root_schema: &Value,
        url: &Url,
        original_reference: &str,
    ) -> Result<Arc<Value>, SchemaResolverError> {
        if url.scheme() != PACKAGE_SCHEMA_SCHEME || url.host_str().is_some() {
            bail!("schema reference '{original_reference}' resolved outside the plugin package");
        }
        let relative = url.path().trim_start_matches('/');
        let relative = validate_relative_path(relative, "referenced")?;
        let path = package_file(&self.plugin_dir, &relative, "referenced")?;
        let raw = fs::read_to_string(&path).with_context(|| {
            format!("failed reading referenced schema '{}'", relative.display())
        })?;
        let source: Value = serde_json::from_str(&raw).with_context(|| {
            format!("invalid JSON in referenced schema '{}'", relative.display())
        })?;
        Ok(Arc::new(normalize_document(
            &source,
            &relative.to_string_lossy().replace('\\', "/"),
            "referenced",
        )?))
    }
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    #[test]
    fn validation_reuse_schema_root_compiles_once_with_local_references() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        fs::write(dir.path().join("input.json"), r#"{"type":"object","properties":{"value":{"$ref":"value.json"}},"required":["value"]}"#).unwrap();
        fs::write(dir.path().join("value.json"), r#"{"type":"string"}"#).unwrap();
        let mut cache = SchemaCache::default();
        cache
            .validate(
                dir.path(),
                "input.json",
                "input",
                &serde_json::json!({"value":"first"}),
            )
            .unwrap();
        cache
            .validate(
                dir.path(),
                "input.json",
                "input",
                &serde_json::json!({"value":"second"}),
            )
            .unwrap();
        assert!(
            cache
                .validate(
                    dir.path(),
                    "input.json",
                    "input",
                    &serde_json::json!({"value":5})
                )
                .is_err()
        );
        assert_eq!(cache.roots.len(), 1);
        let root = cache.compile(dir.path(), "input.json", "input").unwrap() as *const JSONSchema;
        assert_eq!(
            root,
            cache.compile(dir.path(), "input.json", "input").unwrap() as *const JSONSchema
        );
        fs::write(dir.path().join("value.json"), "invalid JSON").unwrap();
        assert!(
            SchemaCache::default()
                .compile(dir.path(), "input.json", "input")
                .is_err()
        );
    }
}
