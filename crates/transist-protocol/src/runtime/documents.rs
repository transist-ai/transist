// SPDX-License-Identifier: MPL-2.0
//! Managed document declarations and immutable, execution-attributed revisions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Detects reserved document-binding objects recursively; literal text is opaque.
pub fn contains_document_binding(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("$document") || map.values().any(contains_document_binding)
        }
        serde_json::Value::Array(values) => values.iter().any(contains_document_binding),
        _ => false,
    }
}

/// Compiler-owned logical document requirements, removed from action input.
pub const DOCUMENT_BINDINGS_KEY: &str = "__transist_document_bindings";

/// Access to an invocation-private copy, never to the durable document store.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentAccess {
    /// Latest committed content, sandboxed read-only.
    Read,
    /// A new, initially absent output location.
    Write,
    /// Latest committed content copied into a writable output location.
    Update,
}

/// One authored reserved binding in invoke action data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentBinding {
    /// Root-lexical document identity.
    #[serde(rename = "$document")]
    pub document: String,
    /// Required access to the private copy.
    pub access: DocumentAccess,
    /// Recipe-local manual gate step whose current approval must cover this exact revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
}

/// Explicit compiled action-input location and its logical access requirement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentBindingLocation {
    /// JSON pointer into action input.
    pub path: String,
    /// Logical binding, independent of staging paths.
    pub binding: DocumentBinding,
}

/// Identifies action fields that select authority, executable assets or a workspace.
/// Document copies are data and cannot supply any of these identities.
pub fn document_authority_field(key: &str) -> bool {
    matches!(
        key,
        "secret_refs"
            | "command"
            | "executable"
            | "source"
            | "source_file"
            | "prompt_file"
            | "repo_path"
            | "workspace"
            | "workspace_dir"
            | "workspace_root"
            | "cwd"
            | "working_dir"
            | "working_directory"
    )
}

/// Parses recursive reserved objects and rejects malformed or authority-bearing bindings.
/// Repeated reads and one writer are allowed; multiple writers of one name fail.
pub fn collect_document_bindings(
    value: &serde_json::Value,
) -> Result<Vec<DocumentBindingLocation>, String> {
    fn walk(
        value: &serde_json::Value,
        path: &str,
        authority: bool,
        found: &mut Vec<DocumentBindingLocation>,
    ) -> Result<(), String> {
        match value {
            serde_json::Value::Object(map) if map.contains_key("$document") => {
                if map
                    .get("reviewed_by")
                    .is_some_and(|value| !value.is_string())
                {
                    return Err("reviewed_by must be a literal gate step ID".into());
                }
                if authority {
                    return Err("$document is forbidden in authority, executable identity, source assets, prompt_file and workspace fields".into());
                }
                let binding: DocumentBinding = serde_json::from_value(value.clone())
                    .map_err(|e| format!("invalid $document binding: {e}"))?;
                crate::validate_identifier(crate::IdentifierKind::StepId, &binding.document)
                    .map_err(|e| e.to_string())?;
                if let Some(gate) = &binding.reviewed_by {
                    if binding.access != DocumentAccess::Read {
                        return Err("reviewed_by is permitted only on read bindings".into());
                    }
                    crate::validate_identifier(crate::IdentifierKind::StepId, gate)
                        .map_err(|e| e.to_string())?;
                }
                found.push(DocumentBindingLocation {
                    path: path.into(),
                    binding,
                });
            }
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    walk(
                        value,
                        &format!("{path}/{}", crate::binding_pointer_segment(key)),
                        authority || document_authority_field(key),
                        found,
                    )?;
                }
            }
            serde_json::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    walk(value, &format!("{path}/{index}"), authority, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(value, "", false, &mut found)?;
    let mut accesses = std::collections::BTreeSet::new();
    for location in &found {
        if location.binding.access != DocumentAccess::Read
            && !accesses.insert(&location.binding.document)
        {
            return Err(format!(
                "conflicting $document accesses for '{}'",
                location.binding.document
            ));
        }
    }
    Ok(found)
}

/// Returns checked compiled locations, rejecting metadata drift and non-process use.
pub fn step_document_bindings(
    step: &crate::RecipeStep,
) -> Result<Vec<DocumentBindingLocation>, String> {
    let values: BTreeMap<_, _> = step
        .with
        .iter()
        .filter(|(key, _)| !key.starts_with("__transist_"))
        .collect();
    let found =
        collect_document_bindings(&serde_json::to_value(values).map_err(|e| e.to_string())?)?;
    if !found.is_empty()
        && !matches!(
            step.execution,
            crate::StepExecutionDescriptor::Invoke {
                adapter: crate::AdapterKind::Process,
                ..
            }
        )
    {
        return Err("$document bindings are supported only for process invoke actions; HTTP/MCP are unsupported".into());
    }
    if let Some(metadata) = step.with.get(DOCUMENT_BINDINGS_KEY) {
        let compiled: Vec<DocumentBindingLocation> =
            serde_json::from_value(serde_json::to_value(metadata).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if compiled != found {
            return Err("compiled $document bindings differ from action data".into());
        }
    }
    Ok(found)
}

/// Storage kind of a managed document.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// One exact byte sequence.
    File,
    /// A bounded manifest of regular files and directories.
    Tree,
}

/// Root-lexical document declaration; it grants no filesystem authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentDeclaration {
    /// Immutable storage shape.
    pub kind: DocumentKind,
    /// Positive aggregate content-byte limit.
    pub max_bytes: u64,
    /// Positive file count limit, required only for trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    /// Optional literal UTF-8 initial file bytes; never a source path/template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
}

impl DocumentDeclaration {
    /// Validates the declaration's name, positive bounds, and literal initial bytes.
    pub fn validate(&self, name: &str) -> Result<(), String> {
        crate::validate_identifier(crate::IdentifierKind::StepId, name)
            .map_err(|error| error.to_string())?;
        if self.max_bytes == 0 {
            return Err("document max_bytes must be positive".into());
        }
        match self.kind {
            DocumentKind::File if self.max_files.is_some() => {
                return Err("file documents cannot declare max_files".into());
            }
            DocumentKind::Tree if self.max_files.unwrap_or(0) == 0 || self.initial.is_some() => {
                return Err("trees require positive max_files and cannot declare initial".into());
            }
            _ => {}
        }
        if let Some(initial) = &self.initial {
            if initial.len() as u64 > self.max_bytes {
                return Err("initial document exceeds max_bytes".into());
            }
        }
        Ok(())
    }
}

/// An immutable content-addressed byte sequence within one root run tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentBlob {
    /// Lowercase SHA-256 of the exact bytes.
    pub sha256: String,
    /// Exact byte count.
    pub size_bytes: u64,
}

/// Canonically sorted tree members; paths are relative, UTF-8 normal components.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct DocumentTreeManifest {
    /// Regular file members, including empty files.
    pub files: BTreeMap<String, DocumentBlob>,
    /// Sorted directory members, including empty directories.
    pub directories: Vec<String>,
}

/// One immutable revision, published only by run birth or a successful execution record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevision {
    /// Format identifier, independent of release versions.
    pub schema: String,
    /// SHA-256 of this record with an empty revision_id.
    pub revision_id: String,
    /// Root run tree owning the document namespace and blobs.
    pub root_run_id: String,
    /// Root-lexical document name.
    pub document: String,
    /// Producing root or contained run.
    pub run_id: String,
    /// Producing step, or the reserved initialization identity.
    pub step_id: String,
    /// Lifetime execution ordinal; zero is reserved for root birth initialization.
    pub execution: u32,
    /// Root-tree publication order; zero is reserved for initialization.
    pub commit_sequence: u64,
    /// Declared storage kind.
    pub kind: DocumentKind,
    /// File bytes or canonical tree-manifest bytes.
    pub blob: DocumentBlob,
    /// Aggregate business content bytes (excludes tree manifest encoding).
    pub size_bytes: u64,
    /// Regular file count (one for a file).
    pub file_count: u32,
}

impl DocumentRevision {
    /// Current revision format identifier.
    pub const SCHEMA: &str = "transist.document_revision.v1";
    /// Reserved producing step for literal root-run initial revisions.
    pub const INITIAL_STEP: &str = "__transist_document_initialization";
}

/// Immutable review content, committed in the gate's first blocked execution record.
/// Waiting re-entry retains this request, independent of newer document heads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateReviewRequest {
    /// Format identifier.
    pub schema: String,
    /// Run owning the recipe-local gate (not necessarily the document root).
    pub run_id: String,
    /// Authored gate identity.
    pub gate_id: String,
    /// Recipe-local gate step identity.
    pub step_id: String,
    /// Lifetime ordinal of the initial waiting execution.
    pub execution: u32,
    /// Exact committed revisions, sorted by document name. Mandatory, never inferred on decision.
    pub review_documents: Vec<DocumentRevision>,
}

/// Parses literal manual-gate review names; rejects duplicates, unknown names and other controls.
pub fn gate_review_documents(step: &crate::RecipeStep) -> Result<Vec<String>, String> {
    let Some(value) = step.with.get("review_documents") else {
        return Ok(Vec::new());
    };
    if !matches!(&step.execution, crate::StepExecutionDescriptor::Control { primitive } if primitive == "control.gate.manual")
    {
        return Err("review_documents is reserved for manual gates".into());
    }
    let names: Vec<String> =
        serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?)
            .map_err(|e| format!("review_documents must be an array of literal names: {e}"))?;
    let mut unique = std::collections::BTreeSet::new();
    for name in &names {
        crate::validate_identifier(crate::IdentifierKind::StepId, name)
            .map_err(|e| e.to_string())?;
        if !unique.insert(name) {
            return Err("duplicate review_documents name".into());
        }
    }
    Ok(unique.into_iter().cloned().collect())
}

/// Checks recipe-local review references and root-lexical document declarations before birth.
pub fn validate_document_reviews(recipe: &crate::Recipe) -> Result<(), String> {
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|s| &s.step))
    {
        for name in gate_review_documents(step)? {
            if !recipe.documents.contains_key(&name) {
                return Err(format!("undeclared review document '{name}'"));
            }
        }
        for location in step_document_bindings(step)? {
            if let Some(gate_id) = &location.binding.reviewed_by {
                let gate = recipe
                    .steps
                    .iter()
                    .find(|s| &s.id == gate_id)
                    .ok_or_else(|| {
                        format!("unknown recipe-local reviewed_by gate step '{gate_id}'")
                    })?;
                if !gate_review_documents(gate)?.contains(&location.binding.document) {
                    return Err(format!(
                        "reviewed_by gate '{gate_id}' does not review '{}'",
                        location.binding.document
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Atomic root-run initialization record, committed with the run-birth rename.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentInitialization {
    /// Format identifier.
    pub schema: String,
    /// Owning root run id.
    pub root_run_id: String,
    /// SHA-256 of the canonical root declarations, pinning kinds, bounds and initial bytes.
    pub declarations_sha256: String,
    /// Initial revisions, sorted by document name.
    pub document_revisions: Vec<DocumentRevision>,
}

impl DocumentInitialization {
    /// Current initialization format identifier.
    pub const SCHEMA: &str = "transist.document_initialization.v1";
}
