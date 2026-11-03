// SPDX-License-Identifier: MPL-2.0
use crate::manifest::PluginPackage;
use crate::paths::manifest_path;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use transist_trust_digest::digest_file_entries;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginFileEntry {
    pub(crate) relative_path: String,
    pub(crate) source_path: PathBuf,
}

/// Computes a deterministic SHA-256 digest for the declared plugin payload.
///
/// The payload is `plugin.yaml` plus the exact files selected by
/// `package.files`, using sorted relative POSIX paths, permission mode bits,
/// and raw file bytes.
pub fn plugin_dir_sha256(plugin_dir: &Path) -> Result<String> {
    crate::record_validation_work(crate::ValidationWork::PackageHash);
    let entries = collect_plugin_files(plugin_dir)?;
    let mut files = Vec::with_capacity(entries.len());
    for entry in &entries {
        let metadata = fs::symlink_metadata(&entry.source_path).with_context(|| {
            format!(
                "failed reading metadata for: {}",
                entry.source_path.display()
            )
        })?;
        let bytes = fs::read(&entry.source_path).with_context(|| {
            format!(
                "failed reading plugin file: {}",
                entry.source_path.display()
            )
        })?;
        files.push((
            entry.relative_path.clone(),
            file_mode_bits(&metadata),
            bytes,
        ));
    }
    Ok(digest_file_entries(&files))
}

/// Hashes a parsed manifest's exact selected payload using the already-read manifest bytes.
pub(crate) fn parsed_package_sha256(
    plugin_dir: &Path,
    manifest: &crate::PluginManifest,
    raw: Vec<u8>,
) -> Result<String> {
    crate::record_validation_work(crate::ValidationWork::PackageHash);
    let mut selected = crate::validate::validate_package_files(plugin_dir, &manifest.package)?;
    selected.insert(crate::manifest::PLUGIN_MANIFEST_FILE_NAME.to_string());
    let mut files = Vec::new();
    for relative in selected {
        let path = plugin_dir.join(&relative);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("plugin payload is not a regular file: {}", path.display());
        }
        let bytes = if relative == crate::manifest::PLUGIN_MANIFEST_FILE_NAME {
            raw.clone()
        } else {
            fs::read(path)?
        };
        files.push((relative, file_mode_bits(&metadata), bytes));
    }
    Ok(digest_file_entries(&files))
}

pub(crate) fn collect_plugin_files(plugin_dir: &Path) -> Result<Vec<PluginFileEntry>> {
    let manifest_file = manifest_path(plugin_dir);
    let raw = fs::read_to_string(&manifest_file).with_context(|| {
        format!(
            "failed reading plugin manifest: {}",
            manifest_file.display()
        )
    })?;
    let manifest: PackageManifest = yaml_serde::from_str(&raw)
        .with_context(|| format!("invalid plugin manifest: {}", manifest_file.display()))?;
    let package_files = crate::validate::validate_package_files(plugin_dir, &manifest.package)?;
    let mut entries = vec![PluginFileEntry {
        relative_path: crate::manifest::PLUGIN_MANIFEST_FILE_NAME.to_string(),
        source_path: manifest_file,
    }];
    entries.extend(package_files.into_iter().map(|relative_path| {
        let source_path = plugin_dir.join(&relative_path);
        PluginFileEntry {
            relative_path,
            source_path,
        }
    }));
    for entry in &entries {
        let metadata = fs::symlink_metadata(&entry.source_path).with_context(|| {
            format!(
                "failed reading metadata for plugin payload file: {}",
                entry.source_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "plugin package contains unsupported symlink: {}",
                entry.source_path.display()
            );
        }
        if !metadata.is_file() {
            anyhow::bail!(
                "plugin package contains unsupported special file: {}",
                entry.source_path.display()
            );
        }
    }
    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(entries)
}

#[derive(Debug, Deserialize)]
struct PackageManifest {
    package: PluginPackage,
}

#[cfg(unix)]
fn file_mode_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn file_mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::Path;

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod");
    }

    #[cfg(not(unix))]
    fn set_executable(_path: &Path) {}

    fn write_plugin(dir: &Path) {
        fs::create_dir_all(dir.join("schemas")).expect("schemas");
        fs::write(dir.join("schemas/input.json"), "{}").expect("input");
        fs::write(dir.join("plugin.sh"), "#!/bin/sh\nexit 0\n").expect("script");
        set_executable(&dir.join("plugin.sh"));
        fs::write(
            dir.join("plugin.yaml"),
            "schema: transist.plugin.v1\nid: shell\nversion: v1\nadapter: process\npackage:\n  files: [plugin.sh, schemas/input.json]\n",
        )
        .expect("manifest");
    }

    #[test]
    fn digest_is_stable_for_identical_content() {
        let a = tempfile::tempdir().expect("tempdir a");
        let b = tempfile::tempdir().expect("tempdir b");
        write_plugin(a.path());
        write_plugin(b.path());
        assert_eq!(
            plugin_dir_sha256(a.path()).expect("digest a"),
            plugin_dir_sha256(b.path()).expect("digest b")
        );
    }

    #[test]
    fn digest_changes_when_content_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plugin(dir.path());
        let first = plugin_dir_sha256(dir.path()).expect("first digest");
        fs::write(dir.path().join("plugin.sh"), "#!/bin/sh\necho changed\n").expect("rewrite");
        set_executable(&dir.path().join("plugin.sh"));
        let second = plugin_dir_sha256(dir.path()).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn runtime_temp_declaration_changes_package_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plugin(dir.path());
        let first = plugin_dir_sha256(dir.path()).expect("first digest");
        let manifest = fs::read_to_string(dir.path().join("plugin.yaml")).expect("manifest");
        fs::write(
            dir.path().join("plugin.yaml"),
            manifest.replace(
                "package:\n",
                "entrypoint:\n  type: process\n  command: plugin.sh\n  env:\n    XDG_CACHE_HOME: runtime://temp/cache\npackage:\n",
            ),
        )
        .expect("manifest with runtime temp declaration");

        let second = plugin_dir_sha256(dir.path()).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn unselected_files_do_not_affect_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plugin(dir.path());
        let first = plugin_dir_sha256(dir.path()).expect("first digest");
        fs::write(dir.path().join(".transist.lock"), "probe").expect("lock");
        for relative in [
            "README.md",
            "CHANGELOG.md",
            "__pycache__/x.pyc",
            "generated.pyc",
            "generated.pyo",
        ] {
            let path = dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("unselected parent");
            }
            fs::write(path, "unselected").expect("unselected file");
        }
        let second = plugin_dir_sha256(dir.path()).expect("second digest");
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn digest_rejects_selected_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plugin(dir.path());
        symlink(dir.path().join("plugin.sh"), dir.path().join("linked.sh")).expect("symlink");
        let manifest = fs::read_to_string(dir.path().join("plugin.yaml")).expect("manifest");
        fs::write(
            dir.path().join("plugin.yaml"),
            manifest.replace("schemas/input.json]", "schemas/input.json, linked.sh]"),
        )
        .expect("manifest");

        let error = plugin_dir_sha256(dir.path()).expect_err("symlink should fail");
        assert!(format!("{error:#}").contains("symlink"));
    }

    #[test]
    fn digest_changes_when_selected_mode_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plugin(dir.path());
        let first = plugin_dir_sha256(dir.path()).expect("first digest");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join("plugin.sh"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("mode");
        }
        let second = plugin_dir_sha256(dir.path()).expect("second digest");
        assert_ne!(first, second);
    }
}
