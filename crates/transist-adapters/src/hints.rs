// SPDX-License-Identifier: MPL-2.0
//! Artifact and evidence hint canonicalization and persistence.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use transist_protocol::{ExecutionRequest, FileRef};

/// Maximum number of hints accepted per family (artifacts or evidence).
const MAX_HINTS_PER_FAMILY: usize = 32;

/// Maximum bytes for a single hint's inline contents.
const MAX_HINT_BYTES: u64 = 256 * 1024;

/// Maximum total bytes across all hints in one family.
const MAX_FAMILY_TOTAL_BYTES: u64 = 1024 * 1024;

pub(crate) fn canonicalize_hints(
    request: &ExecutionRequest,
    artifact_hints: &[serde_json::Value],
    evidence_hints: &[serde_json::Value],
    secret_values: &BTreeMap<String, String>,
) -> Result<(Vec<FileRef>, Vec<FileRef>)> {
    Ok((
        write_hint_family(&request.paths.artifacts_dir, artifact_hints, secret_values)?,
        write_hint_family(&request.paths.evidence_dir, evidence_hints, secret_values)?,
    ))
}

pub(crate) fn write_hint_family(
    dir: &str,
    hints: &[serde_json::Value],
    secret_values: &BTreeMap<String, String>,
) -> Result<Vec<FileRef>> {
    if hints.len() > MAX_HINTS_PER_FAMILY {
        anyhow::bail!(
            "artifact/evidence hint count {} exceeds maximum {}",
            hints.len(),
            MAX_HINTS_PER_FAMILY
        );
    }
    fs::create_dir_all(dir).with_context(|| format!("failed creating '{}'", dir))?;
    let mut refs = Vec::new();
    let mut total_bytes: u64 = 0;
    for (index, hint) in hints.iter().enumerate() {
        let file_name = hint
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("hint.json");
        let file_name_path = Path::new(file_name);
        if file_name_path.is_absolute()
            || file_name_path.components().count() != 1
            || !file_name_path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("artifact/evidence hint name must be a single relative file name");
        }
        let path = Path::new(dir).join(format!("{index:03}_{file_name}"));
        let bytes = if let Some(contents) = hint.get("contents").and_then(serde_json::Value::as_str)
        {
            let redacted = crate::redact_text(contents, secret_values);
            let len = redacted.len() as u64;
            if len > MAX_HINT_BYTES {
                anyhow::bail!(
                    "artifact/evidence hint '{}' exceeds maximum {} bytes (got {})",
                    file_name,
                    MAX_HINT_BYTES,
                    len
                );
            }
            fs::write(&path, redacted.as_bytes())
                .with_context(|| format!("failed writing '{}'", path.display()))?;
            len
        } else {
            let redacted = crate::redact_json(hint.clone(), secret_values);
            let body = serde_json::to_vec_pretty(&redacted)?;
            let len = body.len() as u64;
            if len > MAX_HINT_BYTES {
                anyhow::bail!(
                    "artifact/evidence hint '{}' exceeds maximum {} bytes (got {})",
                    file_name,
                    MAX_HINT_BYTES,
                    len
                );
            }
            fs::write(&path, &body)
                .with_context(|| format!("failed writing '{}'", path.display()))?;
            len
        };
        total_bytes = total_bytes.saturating_add(bytes);
        if total_bytes > MAX_FAMILY_TOTAL_BYTES {
            anyhow::bail!(
                "artifact/evidence hint family total bytes {} exceeds maximum {}",
                total_bytes,
                MAX_FAMILY_TOTAL_BYTES
            );
        }
        refs.push(FileRef {
            path: path.display().to_string(),
            sha256: String::new(),
            bytes,
        });
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_too_many_hints() {
        let dir = std::env::temp_dir().join(format!("transist-hint-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hints: Vec<serde_json::Value> = (0..(MAX_HINTS_PER_FAMILY + 1))
            .map(|i| json!({"name": format!("f{i}.txt"), "contents": "x"}))
            .collect();
        let result = write_hint_family(dir.to_str().unwrap(), &hints, &BTreeMap::new());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_oversize_hint() {
        let dir =
            std::env::temp_dir().join(format!("transist-hint-test-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let big_contents = "x".repeat((MAX_HINT_BYTES + 1) as usize);
        let hints = vec![json!({"name": "big.txt", "contents": big_contents})];
        let result = write_hint_family(dir.to_str().unwrap(), &hints, &BTreeMap::new());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_normal_hints() {
        let dir =
            std::env::temp_dir().join(format!("transist-hint-test-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hints = vec![
            json!({"name": "a.txt", "contents": "hello"}),
            json!({"name": "b.txt", "contents": "world"}),
        ];
        let refs = write_hint_family(dir.to_str().unwrap(), &hints, &BTreeMap::new())
            .expect("write hints");
        assert_eq!(refs.len(), 2);
        assert!(refs[0].path.ends_with("000_a.txt"));
        assert!(refs[1].path.ends_with("001_b.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
