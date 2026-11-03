// SPDX-License-Identifier: MPL-2.0
//! Private document copies and logical-path normalization. No durable path crosses the port.

use super::*;
use transist_core::ports::DocumentStorePort;
use transist_protocol::{DocumentAccess, DocumentBinding, DocumentKind, DocumentTreeManifest};

/// A logical binding paired only with this invocation's host/sandbox copy paths.
pub(super) struct StagedDocument {
    pub binding: DocumentBinding,
    pub host_path: PathBuf,
    visible_path: String,
}

impl InvocationStaging {
    /// Copies committed content and replaces reserved objects with private paths.
    pub(super) fn stage_documents(
        &self,
        request: &mut ExecutionRequest,
        store: &dyn DocumentStorePort,
    ) -> Result<()> {
        let bindings = transist_protocol::collect_document_bindings(&request.input)
            .map_err(anyhow::Error::msg)?;
        if bindings.is_empty() {
            return Ok(());
        }
        if request.adapter_kind != transist_protocol::AdapterKind::Process {
            bail!("$document bindings are unsupported on HTTP/MCP actions");
        }
        if !self.documents.borrow().is_empty() {
            bail!("documents already staged");
        }
        let heads = store.document_heads(&request.run_id)?;
        let root = self.runtime_temp_root.path().join("documents");
        let read_root = root.join("read");
        let write_root = root.join("write");
        fs::create_dir_all(&read_root)?;
        fs::create_dir_all(&write_root)?;
        set_mode(&root, 0o700)?;
        set_mode(&read_root, 0o700)?;
        set_mode(&write_root, 0o700)?;
        let mut staged = Vec::new();
        for (index, location) in bindings.into_iter().enumerate() {
            let binding = location.binding;
            let parent = if binding.access == DocumentAccess::Read {
                &read_root
            } else {
                &write_root
            };
            // Names are runtime-chosen, independent of untrusted document identities.
            let target = parent.join(format!("binding-{index:020}"));
            if binding.access != DocumentAccess::Write {
                let revision = heads.get(&binding.document).ok_or_else(|| {
                    anyhow::anyhow!("$document '{}' is unpublished", binding.document)
                })?;
                let bytes = store.read_document_blob(&request.run_id, revision)?;
                match revision.kind {
                    DocumentKind::File => {
                        fs::write(&target, bytes)?;
                        set_mode(
                            &target,
                            if binding.access == DocumentAccess::Read {
                                0o400
                            } else {
                                0o600
                            },
                        )?;
                    }
                    DocumentKind::Tree => {
                        let manifest: DocumentTreeManifest = serde_json::from_slice(&bytes)?;
                        fs::create_dir(&target)?;
                        for directory in &manifest.directories {
                            fs::create_dir_all(target.join(directory))?;
                        }
                        for member in manifest.files.keys() {
                            let member_target = target.join(member);
                            if let Some(parent) = member_target.parent() {
                                fs::create_dir_all(parent)?;
                            }
                            fs::write(
                                &member_target,
                                store.read_document_member(&request.run_id, revision, member)?,
                            )?;
                            set_mode(
                                &member_target,
                                if binding.access == DocumentAccess::Read {
                                    0o400
                                } else {
                                    0o600
                                },
                            )?;
                        }
                    }
                }
            }
            let relative = target.strip_prefix(self.runtime_temp_root.path())?;
            #[cfg(target_os = "linux")]
            let visible_root = Path::new("/tmp").join(
                self.runtime_temp_root
                    .path()
                    .file_name()
                    .context("temporary root name missing")?,
            );
            #[cfg(not(target_os = "linux"))]
            let visible_root = self.runtime_temp_root.path().to_path_buf();
            let visible_path = visible_root.join(relative).display().to_string();
            *request
                .input
                .pointer_mut(&location.path)
                .context("document binding location disappeared")? =
                serde_json::Value::String(visible_path.clone());
            staged.push(StagedDocument {
                binding,
                host_path: target,
                visible_path,
            });
        }
        *self.documents.borrow_mut() = staged;
        Ok(())
    }

    /// Rewrites complete private prefixes in text without replacing a home/shared parent.
    pub(super) fn normalize_document_text(&self, text: &str) -> String {
        if self.documents.borrow().is_empty() {
            return text.to_string();
        }
        let mut text = text.to_string();
        for document in self.documents.borrow().iter() {
            let logical = format!("$document:{}", document.binding.document);
            for path in [
                &document.visible_path,
                &document.host_path.display().to_string(),
            ] {
                text = text.replace(path, &logical);
            }
        }
        // Replace the full private prefix only, never a shared parent/home prefix.
        text = text.replace(
            &self.runtime_temp_root.path().display().to_string(),
            "[invocation-temp]",
        );
        #[cfg(target_os = "linux")]
        {
            text = text.replace(
                &Path::new("/tmp")
                    .join(self.runtime_temp_root.path().file_name().unwrap())
                    .display()
                    .to_string(),
                "[invocation-temp]",
            );
        }
        text.replace(
            &self._root.path().display().to_string(),
            "[invocation-staging]",
        )
    }

    /// Restores logical objects for exact output paths and labels for embedded text.
    pub(super) fn normalize_document_value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(raw) => {
                for document in self.documents.borrow().iter() {
                    if raw == &document.visible_path
                        || raw == &document.host_path.display().to_string()
                    {
                        *value = serde_json::to_value(&document.binding)
                            .expect("document binding serialization");
                        return;
                    }
                }
                *raw = self.normalize_document_text(raw);
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    self.normalize_document_value(value);
                }
            }
            serde_json::Value::Object(values) => {
                let old = std::mem::take(values);
                for (key, mut value) in old {
                    self.normalize_document_value(&mut value);
                    let key = self.normalize_document_text(&key);
                    let mut unique = key.clone();
                    let mut index = 1;
                    while values.contains_key(&unique) {
                        unique = format!("{key}#{index}");
                        index += 1;
                    }
                    values.insert(unique, value);
                }
            }
            _ => {}
        }
    }

    /// Normalizes non-output metadata without changing typed string field shapes.
    pub(super) fn normalize_text_value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(raw) => *raw = self.normalize_document_text(raw),
            serde_json::Value::Array(values) => {
                for value in values {
                    self.normalize_text_value(value);
                }
            }
            serde_json::Value::Object(values) => {
                let old = std::mem::take(values);
                for (key, mut value) in old {
                    self.normalize_text_value(&mut value);
                    let key = self.normalize_document_text(&key);
                    let mut unique = key.clone();
                    let mut index = 1;
                    while values.contains_key(&unique) {
                        unique = format!("{key}#{index}");
                        index += 1;
                    }
                    values.insert(unique, value);
                }
            }
            _ => {}
        }
    }
}
