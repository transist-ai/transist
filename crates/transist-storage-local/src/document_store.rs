// SPDX-License-Identifier: MPL-2.0
//! Execution-record-backed document publication, shared by a root run tree.

use crate::atomic_io::{
    AnchoredDir, sync_directory, write_new_file_if_identical, write_replace_file,
};
#[cfg(unix)]
use crate::atomic_io::{CaptureFingerprint, capture_fingerprint, read_capture_file};
use crate::fault_injection::*;
use crate::{LocalRunStore, run_paths};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use transist_core::Redactor;
use transist_core::StepExecutionRecord;
use transist_core::ports::{DocumentOutput, DocumentStorePort};
use transist_protocol::{
    DocumentBlob, DocumentDeclaration, DocumentInitialization, DocumentKind, DocumentRevision,
    DocumentTreeManifest,
};

const INITIAL: &str = "documents/initial.json";
const HEADS: &str = "documents/heads.json";
const BLOBS: &str = "documents/blobs";
const CANDIDATES: &str = "documents/candidates";
const MAX_DEPTH: usize = 64;

#[cfg(unix)]
struct TreeWitness {
    relative: PathBuf,
    file: std::fs::File,
    fingerprint: CaptureFingerprint,
    content: Option<DocumentBlob>,
    entries: Option<Vec<std::ffi::OsString>>,
}

#[cfg(unix)]
struct TreeCapture {
    manifest: DocumentTreeManifest,
    total: u64,
    witnesses: Vec<TreeWitness>,
}

#[cfg(unix)]
impl TreeCapture {
    fn revalidate(&self, anchor: &AnchoredDir, entry_limit: usize) -> Result<()> {
        // Every witness spans its read/enumeration through this final fence.
        // Their intervals all contain end-of-capture, so accepted members describe
        // one coherent tree, rather than independent per-file snapshots.
        for witness in &self.witnesses {
            let mut current = anchor.open_relative(&witness.relative)?;
            if capture_fingerprint(&witness.file)? != witness.fingerprint
                || capture_fingerprint(&current)? != witness.fingerprint
            {
                bail!("tree member identity/version changed across capture");
            }
            if let Some(expected) = &witness.content {
                // Timestamp witnesses can share a tick on Linux filesystems.
                // Re-read through a fresh anchored descriptor and compare exact
                // content too, so same-size in-place changes cannot form a mosaic.
                let (bytes, fingerprint) = read_capture_file(&mut current, expected.size_bytes)?;
                if fingerprint != witness.fingerprint || blob(&bytes) != *expected {
                    bail!("tree member content changed across capture");
                }
            }
            if let Some(entries) = &witness.entries {
                let directory = AnchoredDir::from_file(&witness.file)?;
                if directory.list_bounded(Path::new(""), entry_limit)? != *entries
                    || capture_fingerprint(&witness.file)? != witness.fingerprint
                {
                    bail!("tree directory inventory changed across capture");
                }
            }
        }
        Ok(())
    }
}

struct Namespace {
    root_id: String,
    root: PathBuf,
    declarations: BTreeMap<String, DocumentDeclaration>,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn blob(bytes: &[u8]) -> DocumentBlob {
    DocumentBlob {
        sha256: digest(bytes),
        size_bytes: bytes.len() as u64,
    }
}

fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.split('/').count() > MAX_DEPTH
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        bail!("unsafe document member path");
    }
    Ok(())
}

fn validate_blob_id(blob: &DocumentBlob) -> Result<()> {
    if blob.sha256.len() != 64
        || !blob
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid document blob digest");
    }
    Ok(())
}

fn revision_id(revision: &DocumentRevision) -> Result<String> {
    let mut unsigned = revision.clone();
    unsigned.revision_id.clear();
    Ok(digest(&serde_json::to_vec(&unsigned)?))
}

fn declarations(yaml: &str) -> Result<BTreeMap<String, DocumentDeclaration>> {
    let value: yaml_serde::Value = yaml_serde::from_str(yaml)?;
    let declarations: BTreeMap<String, DocumentDeclaration> = match value.get("documents") {
        Some(value) => yaml_serde::from_value(value.clone())?,
        None => BTreeMap::new(),
    };
    for (name, declaration) in &declarations {
        DocumentDeclaration::validate(declaration, name).map_err(anyhow::Error::msg)?;
    }
    Ok(declarations)
}

fn read_optional(anchor: &AnchoredDir, relative: &Path) -> Result<Option<Vec<u8>>> {
    // Runtime-owned immutable publications can retain a temp hard-link alias
    // after process death. These are anchored reads with record/content checks,
    // not untrusted output captures. Never remove an alias during a read.
    match anchor.read_regular_bounded(relative, u64::MAX, false) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl LocalRunStore {
    fn document_namespace(&self, run_id: &str) -> Result<Namespace> {
        let locator = run_paths::load_run_locator(&self.config.root, run_id)?;
        // Physical containment and persisted parent lineage must agree. Replay ancestry
        // is never a storage root, and a child cannot redirect reads into another tree.
        let mut current = run_id.to_string();
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current.clone()) || visited.len() > 64 {
                bail!("invalid document root lineage");
            }
            let current_locator = run_paths::load_run_locator(&self.config.root, &current)?;
            if current_locator.root_run_id != locator.root_run_id {
                bail!("document locator root mismatch");
            }
            let path = run_paths::resolve_run_root(&self.config.root, &current)?;
            let state: transist_core::PersistedRunRecord = serde_json::from_slice(
                &AnchoredDir::open(&path)?.read_regular(Path::new("state.json"))?,
            )?;
            if state.run_id != current {
                bail!("document lineage run identity mismatch");
            }
            if state
                .lineage
                .as_ref()
                .and_then(|l| l.parent_run_id.as_ref())
                != current_locator.parent_run_id.as_ref()
            {
                bail!("document lineage parent differs from physical containment");
            }
            if let Some(lineage) = &state.lineage {
                if lineage.root_run_id != locator.root_run_id {
                    bail!("document lineage root mismatch");
                }
                if let Some(parent) = &lineage.parent_run_id {
                    if current == locator.root_run_id || lineage.parent_step_id.is_none() {
                        bail!("invalid document parent lineage");
                    }
                    current = parent.clone();
                    continue;
                }
            }
            if current != locator.root_run_id {
                bail!("document lineage does not reach its physical root");
            }
            break;
        }
        let root = run_paths::resolve_run_root(&self.config.root, &locator.root_run_id)?;
        let anchor = AnchoredDir::open(&root)?;
        let yaml = String::from_utf8(anchor.read_regular(Path::new("recipe.yaml"))?)?;
        Ok(Namespace {
            root_id: locator.root_run_id,
            root,
            declarations: declarations(&yaml)?,
        })
    }

    pub(crate) fn validate_document_execution(&self, record: &StepExecutionRecord) -> Result<()> {
        if record.document_revisions.is_empty() {
            if record.status == "success" {
                let locator = run_paths::load_run_locator(&self.config.root, &record.run_id)?;
                let root = run_paths::resolve_run_root(&self.config.root, &locator.root_run_id)?;
                let candidate = candidate_path(&record.run_id, &record.step_id, record.execution)?;
                if let Some(bytes) = read_optional(&AnchoredDir::open(&root)?, &candidate)? {
                    let revisions: Vec<DocumentRevision> = serde_json::from_slice(&bytes)?;
                    if !revisions.is_empty() {
                        bail!("successful completion must publish the entire candidate batch");
                    }
                }
            }
            return Ok(());
        }
        let namespace = self.document_namespace(&record.run_id)?;
        validate_producer(self, &record.run_id, &record.step_id, record.execution)?;
        validate_execution(&namespace, record)?;
        let candidates =
            self.prepared_document_revisions(&record.run_id, &record.step_id, record.execution)?;
        if candidates != record.document_revisions {
            bail!("execution documents differ from the immutable candidate batch");
        }
        let (_, last_sequence) = committed_heads(&namespace)?;
        if record.document_revisions[0].commit_sequence
            != last_sequence
                .checked_add(1)
                .context("document sequence exhausted")?
        {
            bail!("stale document candidate publication sequence");
        }
        // An earlier no-clobber publication can have returned an ambiguous link
        // error. Resync validated candidate/blob entries before the commit record
        // is allowed to become durable, never just the subsequent heads cache.
        sync_directory(&namespace.root.join(BLOBS))?;
        let candidate = namespace.root.join(candidate_path(
            &record.run_id,
            &record.step_id,
            record.execution,
        )?);
        sync_directory(candidate.parent().context("candidate has no parent")?)?;
        Ok(())
    }

    pub(crate) fn validate_loaded_document_execution(
        &self,
        record: &StepExecutionRecord,
    ) -> Result<()> {
        if !record.document_revisions.is_empty() {
            validate_execution(&self.document_namespace(&record.run_id)?, record)?;
        } else if record.status == "success" {
            let locator = run_paths::load_run_locator(&self.config.root, &record.run_id)?;
            let root = run_paths::resolve_run_root(&self.config.root, &locator.root_run_id)?;
            if let Some(bytes) = read_optional(
                &AnchoredDir::open(&root)?,
                &candidate_path(&record.run_id, &record.step_id, record.execution)?,
            )? {
                let candidates: Vec<DocumentRevision> = serde_json::from_slice(&bytes)?;
                if !candidates.is_empty() {
                    bail!("committed success dropped document candidates");
                }
            }
        }
        Ok(())
    }
}

fn candidate_path(run_id: &str, step_id: &str, execution: u32) -> Result<PathBuf> {
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::RunRef, run_id)?;
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, step_id)?;
    if execution == 0 || step_id == DocumentRevision::INITIAL_STEP {
        bail!("document output requires a positive real execution ordinal");
    }
    Ok(Path::new(CANDIDATES)
        .join(run_id)
        .join(step_id)
        .join(format!("execution_{execution:03}.json")))
}

fn validate_producer(
    store: &LocalRunStore,
    run_id: &str,
    step_id: &str,
    execution: u32,
) -> Result<()> {
    let root = run_paths::resolve_run_root(&store.config.root, run_id)?;
    let state: transist_core::PersistedRunRecord =
        serde_json::from_slice(&AnchoredDir::open(&root)?.read_regular(Path::new("state.json"))?)?;
    if state.run_id != run_id {
        bail!("document producer run identity mismatch");
    }
    let main = state.steps.get(step_id);
    let cleanup = state
        .cleanup
        .as_ref()
        .and_then(|cleanup| cleanup.steps.get(step_id));
    if main.is_some() && cleanup.is_some() {
        bail!("ambiguous main/cleanup document producer");
    }
    let step = main
        .or(cleanup)
        .context("document producer step is absent")?;
    if step.executions != execution
        || !matches!(
            step.status,
            transist_core::KernelStepStatus::Running | transist_core::KernelStepStatus::Dispatching
        )
    {
        bail!("document producer must match the active lifetime execution ordinal");
    }
    Ok(())
}

fn persist_blobs(root: &Path, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (sha256, bytes) in blobs {
        if digest(bytes) != *sha256 {
            bail!("document blob digest differs from bytes");
        }
        write_new_file_if_identical(&root.join(BLOBS).join(sha256), bytes)?;
        check_fault(FAULT_DOCUMENT_AFTER_BLOB)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn make_revision(
    namespace: &Namespace,
    name: &str,
    run_id: &str,
    step_id: &str,
    execution: u32,
    sequence: u64,
    content: DocumentBlob,
    size: u64,
    files: u32,
) -> Result<DocumentRevision> {
    let mut revision = DocumentRevision {
        schema: DocumentRevision::SCHEMA.into(),
        revision_id: String::new(),
        root_run_id: namespace.root_id.clone(),
        document: name.into(),
        run_id: run_id.into(),
        step_id: step_id.into(),
        execution,
        commit_sequence: sequence,
        kind: namespace.declarations[name].kind,
        blob: content,
        size_bytes: size,
        file_count: files,
    };
    revision.revision_id = revision_id(&revision)?;
    Ok(revision)
}

fn validate_revision(namespace: &Namespace, revision: &DocumentRevision) -> Result<Vec<u8>> {
    let declaration = namespace
        .declarations
        .get(&revision.document)
        .context("revision names an undeclared document")?;
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::RunRef,
        &revision.run_id,
    )?;
    if revision.execution != 0 {
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::StepId,
            &revision.step_id,
        )?;
    }
    if revision.schema != DocumentRevision::SCHEMA
        || revision.root_run_id != namespace.root_id
        || revision.kind != declaration.kind
        || revision.revision_id != revision_id(revision)?
        || revision.size_bytes > declaration.max_bytes
    {
        bail!("invalid immutable document revision");
    }
    let initial = revision.execution == 0;
    if initial != (revision.step_id == DocumentRevision::INITIAL_STEP)
        || initial != (revision.commit_sequence == 0)
        || (initial && (revision.run_id != namespace.root_id || declaration.initial.is_none()))
    {
        bail!("invalid document producing execution identity");
    }
    let anchor = AnchoredDir::open(&namespace.root)?;
    let manifest_limit = u64::from(declaration.max_files.unwrap_or(1))
        .saturating_mul(50000)
        .saturating_add(128);
    if (revision.kind == DocumentKind::File && revision.blob.size_bytes > declaration.max_bytes)
        || (revision.kind == DocumentKind::Tree && revision.blob.size_bytes > manifest_limit)
    {
        bail!("document blob exceeds declared storage bounds");
    }
    let bytes = read_blob(&anchor, &revision.blob)?;
    match revision.kind {
        DocumentKind::File => {
            if revision.size_bytes != revision.blob.size_bytes
                || revision.file_count != 1
                || (initial
                    && declaration.initial.as_deref().map(str::as_bytes) != Some(bytes.as_slice()))
            {
                bail!("file revision size or initial bytes differ");
            }
        }
        DocumentKind::Tree => {
            let manifest: DocumentTreeManifest = serde_json::from_slice(&bytes)?;
            if serde_json::to_vec(&manifest)? != bytes {
                bail!("tree manifest is not canonically encoded");
            }
            validate_manifest(declaration, &manifest)?;
            let size = manifest.files.values().try_fold(0u64, |total, member| {
                total
                    .checked_add(member.size_bytes)
                    .context("document byte count overflow")
            })?;
            if size != revision.size_bytes || manifest.files.len() as u32 != revision.file_count {
                bail!("tree revision counts differ from its manifest");
            }
            for member in manifest.files.values() {
                read_blob(&anchor, member)?;
            }
        }
    }
    Ok(bytes)
}

fn read_blob(anchor: &AnchoredDir, blob: &DocumentBlob) -> Result<Vec<u8>> {
    validate_blob_id(blob)?;
    let bytes = anchor.read_regular_bounded(
        &Path::new(BLOBS).join(&blob.sha256),
        blob.size_bytes,
        false,
    )?;
    if bytes.len() as u64 != blob.size_bytes || digest(&bytes) != blob.sha256 {
        bail!("document blob is missing or corrupt");
    }
    Ok(bytes)
}

fn validate_manifest(
    declaration: &DocumentDeclaration,
    manifest: &DocumentTreeManifest,
) -> Result<()> {
    let limit = declaration.max_files.context("tree missing max_files")? as usize;
    if manifest.files.len() > limit || manifest.directories.len() > limit {
        bail!("tree exceeds file/directory count bounds");
    }
    let directories = manifest
        .directories
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if directories.len() != manifest.directories.len()
        || !manifest
            .directories
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    {
        bail!("tree directories must be sorted and unique");
    }
    let mut total = 0u64;
    for (name, member) in &manifest.files {
        safe_path(name)?;
        validate_blob_id(member)?;
        total = total
            .checked_add(member.size_bytes)
            .context("tree byte count overflow")?;
        if directories.contains(name.as_str()) {
            bail!("tree file/directory collision");
        }
    }
    for name in manifest.files.keys().chain(manifest.directories.iter()) {
        safe_path(name)?;
        let mut parent = Path::new(name).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            if !directories.contains(path.to_str().context("non-UTF8 tree path")?) {
                bail!("tree manifest missing parent directory");
            }
            parent = path.parent();
        }
    }
    if total > declaration.max_bytes {
        bail!("tree exceeds max_bytes");
    }
    Ok(())
}

fn validate_execution(namespace: &Namespace, record: &StepExecutionRecord) -> Result<()> {
    if record.status == "success" {
        let anchor = AnchoredDir::open(&namespace.root)?;
        let candidate = read_optional(
            &anchor,
            &candidate_path(&record.run_id, &record.step_id, record.execution)?,
        )?;
        let candidates: Vec<DocumentRevision> = candidate
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?
            .unwrap_or_default();
        if candidates != record.document_revisions {
            bail!("committed document array differs from its immutable candidate batch");
        }
    }
    if record.document_revisions.is_empty() {
        return Ok(());
    }
    let result = record
        .result
        .as_ref()
        .context("document publication requires a successful result")?;
    if record.status != "success"
        || result.status != "success"
        || result.run_id != record.run_id
        || result.step_id != record.step_id
        || result.execution != record.execution
        || record.execution == 0
    {
        bail!("failed/canceled/mismatched execution cannot publish documents");
    }
    let sequence = record.document_revisions[0].commit_sequence;
    let mut names = BTreeSet::new();
    for revision in &record.document_revisions {
        if revision.run_id != record.run_id
            || revision.step_id != record.step_id
            || revision.execution != record.execution
            || revision.commit_sequence != sequence
            || !names.insert(&revision.document)
        {
            bail!("document batch has conflicting producing identities");
        }
        validate_revision(namespace, revision)?;
    }
    Ok(())
}

#[cfg(unix)]
fn list_optional(anchor: &AnchoredDir, relative: &Path) -> Result<Vec<std::ffi::OsString>> {
    match anchor.list_bounded(relative, usize::MAX) {
        Ok(entries) => Ok(entries),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

fn committed_heads(namespace: &Namespace) -> Result<(BTreeMap<String, DocumentRevision>, u64)> {
    committed_heads_with_sync(namespace, false)
}

fn committed_heads_with_sync(
    namespace: &Namespace,
    sync_records: bool,
) -> Result<(BTreeMap<String, DocumentRevision>, u64)> {
    let anchor = AnchoredDir::open(&namespace.root)?;
    let initial_bytes = read_optional(&anchor, Path::new(INITIAL))?;
    if namespace.declarations.is_empty() {
        if initial_bytes.is_some() {
            bail!("document declarations were removed from their immutable namespace");
        }
        return Ok((BTreeMap::new(), 0));
    }
    let mut heads = BTreeMap::new();
    if !namespace.declarations.is_empty() {
        let initial: DocumentInitialization = serde_json::from_slice(
            &initial_bytes.context("document initialization record is missing")?,
        )?;
        if initial.schema != DocumentInitialization::SCHEMA
            || initial.root_run_id != namespace.root_id
            || initial.declarations_sha256 != digest(&serde_json::to_vec(&namespace.declarations)?)
        {
            bail!("invalid document initialization record");
        }
        for revision in initial.document_revisions {
            validate_revision(namespace, &revision)?;
            if revision.execution != 0
                || heads.insert(revision.document.clone(), revision).is_some()
            {
                bail!("invalid initial revision identity");
            }
        }
        if namespace
            .declarations
            .iter()
            .filter(|(_, declaration)| declaration.initial.is_some())
            .count()
            != heads.len()
        {
            bail!("initial document set differs from recipe");
        }
    } else if initial_bytes.is_some() {
        bail!("document initialization without declarations");
    }
    let mut batches = BTreeMap::new();
    collect_committed(
        namespace,
        &anchor,
        Path::new(""),
        &mut batches,
        sync_records,
    )?;
    let mut last = 0u64;
    for (sequence, revisions) in batches {
        if sequence != last.checked_add(1).context("document sequence overflow")? {
            bail!("document commit sequence has a gap");
        }
        last = sequence;
        for revision in revisions {
            heads.insert(revision.document.clone(), revision);
        }
    }
    Ok((heads, last))
}

#[cfg(unix)]
fn collect_committed(
    namespace: &Namespace,
    anchor: &AnchoredDir,
    run_relative: &Path,
    batches: &mut BTreeMap<u64, Vec<DocumentRevision>>,
    sync_records: bool,
) -> Result<()> {
    let state: transist_core::PersistedRunRecord =
        serde_json::from_slice(&anchor.read_regular(&run_relative.join("state.json"))?)?;
    let expected = if run_relative.as_os_str().is_empty() {
        namespace.root_id.as_str()
    } else {
        run_relative
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("run_"))
            .context("invalid contained run identity")?
    };
    if state.run_id != expected
        || state
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.root_run_id != namespace.root_id)
    {
        bail!("document run-tree producing identity mismatch");
    }
    let executions = run_relative.join("step_executions");
    let mut consumed = BTreeMap::<String, BTreeSet<u32>>::new();
    for step in list_optional(anchor, &executions)? {
        let step_id = step.to_str().context("non-UTF8 execution directory")?;
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, step_id)?;
        let step_path = executions.join(&step);
        let mut document_completion = false;
        for file in list_optional(anchor, &step_path)? {
            let name = file.to_str().context("non-UTF8 execution filename")?;
            // Atomic publication temps are not completion records.
            if name.starts_with('.') {
                continue;
            }
            let ordinal = name
                .strip_prefix("execution_")
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|name| name.parse::<u32>().ok())
                .context("invalid execution filename")?;
            let record: StepExecutionRecord = serde_json::from_slice(
                &anchor.read_regular_bounded(&step_path.join(&file), u64::MAX, false)?,
            )?;
            if ordinal == 0
                || record.run_id != state.run_id
                || record.step_id != step_id
                || record.execution != ordinal
            {
                bail!("execution record identity mismatch during document recovery");
            }
            if !consumed.entry(step_id.into()).or_default().insert(ordinal) {
                bail!("duplicate execution ordinal during document recovery");
            }
            validate_execution(namespace, &record)?;
            if let Some(first) = record.document_revisions.first() {
                document_completion = true;
                if batches
                    .insert(first.commit_sequence, record.document_revisions)
                    .is_some()
                {
                    bail!("duplicate root-tree document commit sequence");
                }
            }
        }
        if sync_records && document_completion {
            // Completion may have linked before a failed containing-directory
            // sync. Recovery must make that entry durable before advancing any
            // heads/state projection from it.
            sync_directory(&namespace.root.join(&step_path))?;
        }
    }
    validate_consumed_history(&state, &consumed)?;
    let children = run_relative.join("contained_runs");
    for child in list_optional(anchor, &children)? {
        if !child.to_str().is_some_and(|name| name.starts_with("run_")) {
            bail!("invalid contained run directory");
        }
        collect_committed(
            namespace,
            anchor,
            &children.join(child),
            batches,
            sync_records,
        )?;
    }
    Ok(())
}

fn validate_consumed_history(
    state: &transist_core::PersistedRunRecord,
    found: &BTreeMap<String, BTreeSet<u32>>,
) -> Result<()> {
    let mut projections = state.steps.iter().collect::<BTreeMap<_, _>>();
    if let Some(cleanup) = &state.cleanup {
        for (id, step) in &cleanup.steps {
            if projections.insert(id, step).is_some() {
                bail!("ambiguous main/cleanup execution history");
            }
        }
    }
    for id in found.keys() {
        if !projections.contains_key(id) {
            bail!("execution history has no producing step projection");
        }
    }
    for (id, step) in projections {
        let empty = BTreeSet::new();
        let ordinals = found.get(id).unwrap_or(&empty);
        // Only the current transient attempt may lack a record. A reset/failed
        // activation retains its consumed history even when last_result is absent.
        let result_claims_latest = step
            .last_result
            .as_ref()
            .is_some_and(|result| result.execution == step.executions);
        let inflight = matches!(
            step.status,
            transist_core::KernelStepStatus::Dispatching | transist_core::KernelStepStatus::Running
        ) && !result_claims_latest;
        let required = if step.executions > 0 && inflight && !ordinals.contains(&step.executions) {
            step.executions - 1
        } else {
            step.executions
        };
        if ordinals.len() as u64 != u64::from(required)
            || ordinals
                .iter()
                .copied()
                .enumerate()
                .any(|(index, ordinal)| u64::from(ordinal) != index as u64 + 1)
        {
            bail!(
                "missing consumed execution history for run '{}' step '{}'",
                state.run_id,
                id
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn collect_committed(
    _namespace: &Namespace,
    _anchor: &AnchoredDir,
    _run_relative: &Path,
    _batches: &mut BTreeMap<u64, Vec<DocumentRevision>>,
    _sync_records: bool,
) -> Result<()> {
    bail!("document storage requires descriptor-anchored filesystem support")
}

impl DocumentStorePort for LocalRunStore {
    fn prepare_document_revisions(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
        outputs: &[DocumentOutput],
        redactor: &Redactor,
    ) -> Result<Vec<DocumentRevision>> {
        let candidate = candidate_path(run_id, step_id, execution)?;
        let namespace = self.document_namespace(run_id)?;
        validate_producer(self, run_id, step_id, execution)?;
        if self
            .step_execution_path(run_id, step_id, execution)?
            .symlink_metadata()
            .is_ok()
        {
            bail!("cannot capture documents for a committed execution");
        }
        if outputs.is_empty() {
            return Ok(Vec::new());
        }
        let (_, last) = committed_heads(&namespace)?;
        let sequence = last.checked_add(1).context("document sequence exhausted")?;
        let mut names = BTreeSet::new();
        let mut blobs = BTreeMap::new();
        let mut revisions = Vec::new();
        for output in outputs {
            let declaration = namespace
                .declarations
                .get(&output.document)
                .context("output names an undeclared document")?;
            if !names.insert(&output.document) || redactor.contains_secret(&output.document) {
                bail!("duplicate or secret-bearing document name");
            }
            let (bytes, size, count) =
                capture_output(declaration, &output.path, redactor, &mut blobs)?;
            let content = blob(&bytes);
            blobs.insert(content.sha256.clone(), bytes);
            revisions.push(make_revision(
                &namespace,
                &output.document,
                run_id,
                step_id,
                execution,
                sequence,
                content,
                size,
                count,
            )?);
        }
        revisions.sort_by(|left, right| left.document.cmp(&right.document));
        if redactor.contains_secret_bytes(&serde_json::to_vec(&revisions)?) {
            bail!("secret-bearing document revision metadata rejected");
        }
        // Scan the exact durable payloads too: canonical manifests contain member
        // digests/metadata not present in source bytes or the outer revision array.
        if blobs
            .values()
            .any(|bytes| redactor.contains_secret_bytes(bytes))
        {
            bail!("secret-bearing document blob or canonical manifest rejected");
        }
        check_fault(FAULT_DOCUMENT_BEFORE_BLOBS)?;
        persist_blobs(&namespace.root, &blobs)?;
        check_fault(FAULT_DOCUMENT_AFTER_BLOBS)?;
        write_new_file_if_identical(
            &namespace.root.join(candidate),
            &serde_json::to_vec(&revisions)?,
        )?;
        Ok(revisions)
    }

    fn prepared_document_revisions(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Vec<DocumentRevision>> {
        let candidate = candidate_path(run_id, step_id, execution)?;
        let namespace = self.document_namespace(run_id)?;
        let anchor = AnchoredDir::open(&namespace.root)?;
        let Some(bytes) = read_optional(&anchor, &candidate)? else {
            return Ok(Vec::new());
        };
        let revisions: Vec<DocumentRevision> = serde_json::from_slice(&bytes)?;
        let mut names = BTreeSet::new();
        for revision in &revisions {
            if revision.run_id != run_id
                || revision.step_id != step_id
                || revision.execution != execution
                || !names.insert(&revision.document)
            {
                bail!("document candidate identity mismatch");
            }
            validate_revision(&namespace, revision)?;
        }
        Ok(revisions)
    }

    fn document_heads(&self, run_id: &str) -> Result<BTreeMap<String, DocumentRevision>> {
        Ok(committed_heads(&self.document_namespace(run_id)?)?.0)
    }

    fn document_revision(
        &self,
        run_id: &str,
        revision_id: &str,
    ) -> Result<Option<DocumentRevision>> {
        validate_blob_id(&DocumentBlob {
            sha256: revision_id.into(),
            size_bytes: 0,
        })?;
        let namespace = self.document_namespace(run_id)?;
        committed_heads(&namespace)?;
        if namespace.declarations.is_empty() {
            // Empty roots intentionally have no initialization record. Namespace
            // resolution above must still validate every physical parent/lineage.
            return Ok(None);
        }
        let anchor = AnchoredDir::open(&namespace.root)?;
        let initial: DocumentInitialization =
            serde_json::from_slice(&anchor.read_regular(Path::new(INITIAL))?)?;
        let mut batches = BTreeMap::new();
        collect_committed(&namespace, &anchor, Path::new(""), &mut batches, false)?;
        let revision = initial
            .document_revisions
            .into_iter()
            .chain(batches.into_values().flatten())
            .find(|r| r.revision_id == revision_id);
        if let Some(revision) = &revision {
            validate_revision(&namespace, revision)?;
        }
        Ok(revision)
    }

    fn read_document_blob(&self, run_id: &str, revision: &DocumentRevision) -> Result<Vec<u8>> {
        let namespace = self.document_namespace(run_id)?;
        committed_heads(&namespace)?;
        // Only committed revision identities are readable, including older history.
        let anchor = AnchoredDir::open(&namespace.root)?;
        let mut batches = BTreeMap::new();
        collect_committed(&namespace, &anchor, Path::new(""), &mut batches, false)?;
        let committed = if revision.execution == 0 {
            let initial: DocumentInitialization =
                serde_json::from_slice(&anchor.read_regular(Path::new(INITIAL))?)?;
            initial.document_revisions.contains(revision)
        } else {
            batches.values().any(|batch| batch.contains(revision))
        };
        if !committed {
            bail!("document revision is unpublished or belongs to another run");
        }
        validate_revision(&namespace, revision)
    }

    fn reconcile_document_heads(&self, run_id: &str) -> Result<()> {
        let namespace = self.document_namespace(run_id)?;
        let (heads, _) = committed_heads_with_sync(&namespace, true)?;
        if namespace.declarations.is_empty() {
            return Ok(());
        }
        check_fault(FAULT_DOCUMENT_BEFORE_HEADS)?;
        write_replace_file(&namespace.root.join(HEADS), &serde_json::to_vec(&heads)?)?;
        check_fault(FAULT_DOCUMENT_AFTER_HEADS)?;
        Ok(())
    }

    fn read_document_member(
        &self,
        run_id: &str,
        revision: &DocumentRevision,
        member: &str,
    ) -> Result<Vec<u8>> {
        safe_path(member)?;
        if revision.kind != DocumentKind::Tree {
            bail!("file revision has no tree members");
        }
        let manifest: DocumentTreeManifest =
            serde_json::from_slice(&self.read_document_blob(run_id, revision)?)?;
        let blob = manifest
            .files
            .get(member)
            .context("tree member is absent")?;
        let namespace = self.document_namespace(run_id)?;
        read_blob(&AnchoredDir::open(&namespace.root)?, blob)
    }
}

#[cfg(unix)]
fn capture_output(
    declaration: &DocumentDeclaration,
    path: &Path,
    redactor: &Redactor,
    blobs: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, u64, u32)> {
    match declaration.kind {
        DocumentKind::File => {
            let anchor = AnchoredDir::open(path.parent().context("document file has no parent")?)?;
            let bytes = anchor.read_regular_bounded(
                Path::new(path.file_name().context("document file has no name")?),
                declaration.max_bytes,
                true,
            )?;
            if redactor.contains_secret_bytes(&bytes) {
                bail!("secret-bearing document bytes rejected");
            }
            let size = bytes.len() as u64;
            Ok((bytes, size, 1))
        }
        DocumentKind::Tree => {
            let anchor = AnchoredDir::open(path)?;
            let mut capture = TreeCapture {
                manifest: DocumentTreeManifest::default(),
                total: 0,
                witnesses: Vec::new(),
            };
            capture_tree(
                &anchor,
                Path::new(""),
                declaration,
                redactor,
                &mut capture,
                blobs,
            )?;
            let root_now = AnchoredDir::open(path)?.open_relative(Path::new(""))?;
            if capture_fingerprint(&root_now)? != capture.witnesses[0].fingerprint {
                bail!("tree capture root was replaced or changed");
            }
            capture.revalidate(
                &anchor,
                (declaration.max_files.unwrap_or(0) as usize).saturating_mul(2),
            )?;
            let mut manifest = capture.manifest;
            manifest.directories.sort();
            validate_manifest(declaration, &manifest)?;
            let count = manifest.files.len() as u32;
            Ok((serde_json::to_vec(&manifest)?, capture.total, count))
        }
    }
}

#[cfg(unix)]
fn capture_tree(
    anchor: &AnchoredDir,
    relative: &Path,
    declaration: &DocumentDeclaration,
    redactor: &Redactor,
    capture: &mut TreeCapture,
    blobs: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let limit = declaration.max_files.context("tree missing max_files")? as usize;
    let directory = anchor.open_relative(relative)?;
    let fingerprint = capture_fingerprint(&directory)?;
    let entries =
        AnchoredDir::from_file(&directory)?.list_bounded(Path::new(""), limit.saturating_mul(2))?;
    capture.witnesses.push(TreeWitness {
        relative: relative.into(),
        file: directory,
        fingerprint,
        content: None,
        entries: Some(entries.clone()),
    });
    run_tree_capture_hook(relative, true);
    for entry in entries {
        let path = relative.join(entry);
        let name = path
            .to_str()
            .context("tree member must be UTF-8")?
            .to_string();
        safe_path(&name)?;
        if redactor.contains_secret(&name) {
            bail!("secret-bearing tree path rejected");
        }
        let mut file = anchor.open_relative(&path)?;
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            if capture.manifest.directories.len() >= limit {
                bail!("tree exceeds directory count bound");
            }
            capture.manifest.directories.push(name);
            capture_tree(anchor, &path, declaration, redactor, capture, blobs)?;
        } else if metadata.is_file() {
            if capture.manifest.files.len() >= limit {
                bail!("tree exceeds max_files");
            }
            let (bytes, fingerprint) =
                read_capture_file(&mut file, declaration.max_bytes - capture.total)?;
            if redactor.contains_secret_bytes(&bytes) {
                bail!("secret-bearing document bytes rejected");
            }
            capture.total = capture
                .total
                .checked_add(bytes.len() as u64)
                .context("tree byte count overflow")?;
            let member = blob(&bytes);
            blobs.insert(member.sha256.clone(), bytes);
            capture.manifest.files.insert(name, member.clone());
            capture.witnesses.push(TreeWitness {
                relative: path.clone(),
                file,
                fingerprint,
                content: Some(member),
                entries: None,
            });
            run_tree_capture_hook(&path, false);
        } else {
            bail!("tree contains a special file");
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn capture_output(
    _declaration: &DocumentDeclaration,
    _path: &Path,
    _redactor: &Redactor,
    _blobs: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, u64, u32)> {
    bail!("document storage requires descriptor-anchored filesystem support")
}

pub(crate) fn initialize_documents(
    root: &Path,
    root_id: &str,
    yaml: &str,
    child: bool,
) -> Result<()> {
    let declarations = declarations(yaml)?;
    if declarations.is_empty() || child {
        return Ok(());
    }
    let namespace = Namespace {
        root_id: root_id.into(),
        root: root.to_path_buf(),
        declarations,
    };
    let mut revisions = Vec::new();
    let mut blobs = BTreeMap::new();
    for (name, declaration) in &namespace.declarations {
        if let Some(initial) = &declaration.initial {
            let content = blob(initial.as_bytes());
            blobs.insert(content.sha256.clone(), initial.as_bytes().to_vec());
            revisions.push(make_revision(
                &namespace,
                name,
                root_id,
                DocumentRevision::INITIAL_STEP,
                0,
                0,
                content,
                initial.len() as u64,
                1,
            )?);
        }
    }
    persist_blobs(root, &blobs)?;
    let initial = DocumentInitialization {
        schema: DocumentInitialization::SCHEMA.into(),
        root_run_id: root_id.into(),
        declarations_sha256: digest(&serde_json::to_vec(&namespace.declarations)?),
        document_revisions: revisions,
    };
    write_new_file_if_identical(&root.join(INITIAL), &serde_json::to_vec(&initial)?)?;
    Ok(())
}

pub(crate) fn validate_initial_documents(
    root: &Path,
    root_id: &str,
    yaml: &str,
    child: bool,
) -> Result<()> {
    let declarations = declarations(yaml)?;
    if child {
        return Ok(());
    }
    let namespace = Namespace {
        root_id: root_id.into(),
        root: root.to_path_buf(),
        declarations,
    };
    committed_heads(&namespace)?;
    Ok(())
}

pub(crate) fn validate_recipe_documents_unchanged(old: &str, new: &str) -> Result<()> {
    if declarations(old)? != declarations(new)? {
        bail!("document declarations are immutable at root birth; select a fresh run namespace");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use transist_core::ports::{RunBirthArtifacts, RunStorePort};

    fn capture_fixture() -> (tempfile::TempDir, LocalRunStore, DocumentOutput) {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(crate::LocalStorageConfig::new(tmp.path()));
        let mut record = transist_core::PersistedRunRecord::from_snapshot(
            transist_core::KernelRunSnapshot::new("run", "demo", "0.1.0", &["produce".into()]),
            None,
            None,
            BTreeMap::new(),
            0,
            1,
            1,
        );
        store.create_run_staged(&RunBirthArtifacts {
            record: &record, recipe_yaml: "documents:\n  seed: {kind: file, max_bytes: 32, initial: seed}\n  tree: {kind: tree, max_bytes: 32, max_files: 8}\n",
            run_inputs_json: b"{}", provenance_json: b"{}", workflow_asset_manifest_json: None, workflow_asset_blobs: &[],
        }).unwrap();
        store.publish_run_locator(tmp.path(), &record).unwrap();
        let step = record.steps.get_mut("produce").unwrap();
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 1;
        store.update_run_state("run", &record).unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let output = DocumentOutput {
            document: "tree".into(),
            path: tree,
        };
        (tmp, store, output)
    }

    #[test]
    fn review4_full_tree_capture_rejects_coordinated_file_inventory_and_link_mutations() {
        for case in ["mosaic", "added", "replacement", "symlink"] {
            let (tmp, store, output) = capture_fixture();
            let tree = output.path.clone();
            std::fs::write(tree.join("a"), b"a-old").unwrap();
            if case == "mosaic" {
                std::fs::write(tree.join("b"), b"b-old").unwrap();
            }
            let barrier = Arc::new(Barrier::new(2));
            let hook_barrier = barrier.clone();
            let mut triggered = false;
            let _hook = after_tree_capture_hook(move |path, directory| {
                if !triggered
                    && ((case == "added" && directory && path.as_os_str().is_empty())
                        || (case != "added" && !directory && path == Path::new("a")))
                {
                    triggered = true;
                    hook_barrier.wait();
                    hook_barrier.wait();
                }
            });
            let outside = tmp.path().join("outside");
            std::fs::write(&outside, b"outside").unwrap();
            let writer = std::thread::spawn(move || {
                barrier.wait();
                match case {
                    "mosaic" => {
                        // a-old/b-new never coexist: advance a before b.
                        std::fs::write(tree.join("a"), b"a-new").unwrap();
                        std::fs::write(tree.join("b"), b"b-new").unwrap();
                    }
                    "added" => std::fs::write(tree.join("b"), b"new").unwrap(),
                    "replacement" => {
                        std::fs::write(tree.join("new"), b"a-old").unwrap();
                        std::fs::rename(tree.join("new"), tree.join("a")).unwrap();
                    }
                    _ => {
                        std::fs::remove_file(tree.join("a")).unwrap();
                        std::os::unix::fs::symlink(outside, tree.join("a")).unwrap();
                    }
                }
                barrier.wait();
            });
            let captured = store.prepare_document_revisions(
                "run",
                "produce",
                1,
                &[output],
                &Redactor::default(),
            );
            writer.join().unwrap();
            assert!(
                captured.is_err(),
                "{case} must reject the entire tree capture"
            );
            assert_eq!(store.document_heads("run").unwrap().len(), 1);
            assert!(
                !tmp.path()
                    .join("runs/run_run/documents/candidates")
                    .exists()
            );
            assert_eq!(
                std::fs::read_dir(tmp.path().join("runs/run_run/documents/blobs"))
                    .unwrap()
                    .count(),
                1
            );
        }
    }

    #[test]
    fn anchored_enumeration_has_independent_offsets_and_bounded_reads() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file"), b"exact").unwrap();
        let anchor = AnchoredDir::open(tmp.path()).unwrap();
        let first = anchor.list_bounded(Path::new(""), 1).unwrap();
        assert_eq!(anchor.list_bounded(Path::new(""), 1).unwrap(), first);
        assert!(anchor.list_bounded(Path::new(""), 0).is_err());
        assert_eq!(
            anchor
                .read_regular_bounded(Path::new("file"), 5, true)
                .unwrap(),
            b"exact"
        );
        assert!(
            anchor
                .read_regular_bounded(Path::new("file"), 4, true)
                .is_err()
        );
        assert!(
            anchor
                .read_regular_bounded(Path::new("../file"), 5, true)
                .is_err()
        );
    }
}
