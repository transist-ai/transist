// SPDX-License-Identifier: MPL-2.0
//! Revision-scoped inspection: logical metadata and exact bytes, never durable home paths.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use transist_core::ports::DocumentStorePort;
use transist_protocol::{DocumentKind, DocumentRevision, DocumentTreeManifest, GateReviewRequest};
use transist_storage_local::{LocalRunStore, LocalStorageConfig};

/// Opaque links to one immutable revision within a selected run tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionDocumentLinks {
    /// Revision metadata, including tree member links.
    pub metadata: String,
    /// Exact file bytes, or the canonical manifest for a tree.
    pub content: String,
}

/// A tree member is selected by an opaque ID, never by a supplied filesystem path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionDocumentMember {
    /// Opaque, revision-scoped member identity.
    pub member_id: String,
    /// Validated relative manifest label, not a host path.
    pub relative_path: String,
    /// Exact byte count.
    pub size_bytes: u64,
    /// Exact content digest.
    pub sha256: String,
    /// Exact content download link.
    pub content: String,
}

/// Exact revision metadata used by gate review and document content inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionDocument {
    /// Runtime-validated immutable revision record.
    pub revision: DocumentRevision,
    /// Run-scoped opaque retrieval links.
    pub links: InspectionDocumentLinks,
    /// Tree member metadata (populated by revision detail, empty in compact gate summaries).
    pub members: Vec<InspectionDocumentMember>,
}

impl InspectionDocument {
    /// Builds host-independent links for a known revision and selected inspection run.
    pub fn for_revision(run_id: &str, revision: DocumentRevision) -> Self {
        let metadata = format!("/api/v1/runs/{run_id}/documents/{}", revision.revision_id);
        Self {
            revision,
            links: InspectionDocumentLinks {
                content: format!("{metadata}/content"),
                metadata,
            },
            members: Vec::new(),
        }
    }
}

impl InspectionGate {
    /// Returns compact immutable review metadata from the inspector-validated committed
    /// request, never current heads or authored guidance. The builder reserves this key.
    pub fn review_documents(&self) -> Result<Vec<InspectionDocument>> {
        let Some(value) = self.request_summary.get("review_request") else {
            return Ok(Vec::new());
        };
        let request: GateReviewRequest = serde_json::from_value(serde_json::to_value(value)?)?;
        Ok(request
            .review_documents
            .into_iter()
            .map(|r| InspectionDocument::for_revision(&request.run_id, r))
            .collect())
    }
}

fn member_id(revision: &str, path: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{revision}\0{path}").as_bytes())
    )
}

pub(crate) fn get_document(home: &Path, run: &str, id: &str) -> Result<Option<InspectionDocument>> {
    let store = LocalRunStore::new(LocalStorageConfig::new(home));
    let Some(revision) = store.document_revision(run, id)? else {
        return Ok(None);
    };
    let mut document = InspectionDocument::for_revision(run, revision);
    if document.revision.kind == DocumentKind::Tree {
        let manifest: DocumentTreeManifest =
            serde_json::from_slice(&store.read_document_blob(run, &document.revision)?)?;
        document.members = manifest
            .files
            .into_iter()
            .map(|(relative_path, blob)| {
                let member_id = member_id(id, &relative_path);
                InspectionDocumentMember {
                    content: format!("{}?member_id={member_id}", document.links.content),
                    member_id,
                    relative_path,
                    size_bytes: blob.size_bytes,
                    sha256: blob.sha256,
                }
            })
            .collect();
    }
    Ok(Some(document))
}

pub(crate) fn read_document(
    home: &Path,
    run: &str,
    id: &str,
    member: Option<&str>,
) -> Result<Option<InspectionBlobContent>> {
    let Some(document) = get_document(home, run, id)? else {
        return Ok(None);
    };
    let store = LocalRunStore::new(LocalStorageConfig::new(home));
    let (name, bytes) = if let Some(id) = member {
        let Some(member) = document.members.iter().find(|m| m.member_id == id) else {
            return Ok(None);
        };
        (
            member.relative_path.clone(),
            store.read_document_member(run, &document.revision, &member.relative_path)?,
        )
    } else {
        (
            document.revision.document.clone(),
            store.read_document_blob(run, &document.revision)?,
        )
    };
    Ok(Some(InspectionBlobContent {
        relative_path: name.clone(),
        name,
        bytes,
    }))
}
