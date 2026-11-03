// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;
use transist_core::RuntimeOperatingMode;

/// Initializes a repository with the minimal Transist directory structure and default config files.
pub fn init_repo(repo_root: &Path) -> Result<()> {
    init_home_dir(&crate::repo_local_home_dir(repo_root))
}

fn default_home_config() -> &'static str {
    transist_storage_local::DEFAULT_HOME_CONFIG_YAML
}

/// Ensures a Transist home directory contains the expected runtime directories and defaults.
pub fn init_home_dir(home_dir: &Path) -> Result<()> {
    init_home_dir_with_mode(home_dir, RuntimeOperatingMode::Governed)
}

/// Ensures a Transist home exists with an explicitly selected operating mode.
pub fn init_home_dir_with_mode(home_dir: &Path, mode: RuntimeOperatingMode) -> Result<()> {
    let root = home_dir.to_path_buf();
    let mut dirs = vec![
        root.join("registry/recipes"),
        root.join("runs"),
        root.join("plugins"),
    ];
    if matches!(mode, RuntimeOperatingMode::Governed) {
        dirs.push(transist_storage_local::trust_store_dir(&root));
        dirs.push(root.join("policy/workflows"));
        dirs.push(root.join("policy/workflow-authorizations"));
    }

    for dir in dirs {
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed creating directory: {}", dir.display()))?;
    }

    write_if_missing(
        &root.join("config.yaml"),
        default_home_config_for_mode(mode).as_bytes(),
    )?;
    write_if_missing(
        &root.join("toolchains.yaml"),
        transist_storage_local::DEFAULT_REPO_CONFIG_YAML.as_bytes(),
    )?;
    let _ = transist_storage_local::Installation::load_or_create(home_dir)?;
    Ok(())
}

fn default_home_config_for_mode(mode: RuntimeOperatingMode) -> String {
    if matches!(mode, RuntimeOperatingMode::Governed) {
        return default_home_config().to_string();
    }

    format!(
        "version: v1\nruntime:\n  mode: {}\npolicy:\n  profile: permissive\n  remote_secret_delivery:\n    process: false\n    http: false\n    mcp: false\n",
        mode.as_str()
    )
}

fn write_if_missing(path: &Path, bytes: &[u8]) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(bytes)
            .with_context(|| format!("failed writing file: {}", path.display())),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed creating file: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_repo_creates_runtime_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_repo(tmp.path()).expect("init");
        assert!(crate::repo_local_home_dir(tmp.path()).join("runs").exists());
        assert!(
            crate::repo_local_home_dir(tmp.path())
                .join("registry/recipes")
                .exists()
        );
        assert!(
            !crate::repo_local_home_dir(tmp.path())
                .join("registry/flows")
                .exists()
        );
    }

    #[test]
    fn write_if_missing_is_atomic_under_concurrent_init() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();

        let threads: Vec<_> = (0..16)
            .map(|_| {
                let home = home.clone();
                std::thread::spawn(move || init_home_dir(&home).expect("init_home_dir"))
            })
            .collect();
        for t in threads {
            t.join().expect("thread join");
        }

        let config = std::fs::read(home.join("config.yaml")).expect("config");
        assert_eq!(config.as_slice(), default_home_config().as_bytes(),);
        let toolchains = std::fs::read(home.join("toolchains.yaml")).expect("toolchains");
        assert_eq!(
            toolchains.as_slice(),
            transist_storage_local::DEFAULT_REPO_CONFIG_YAML.as_bytes(),
        );
    }

    #[test]
    fn write_if_missing_preserves_existing_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("preexisting.yaml");
        std::fs::write(&path, b"user-edited\n").expect("seed");
        write_if_missing(&path, b"default\n").expect("no-op write");
        let after = std::fs::read(&path).expect("read");
        assert_eq!(after.as_slice(), b"user-edited\n");
    }

    #[test]
    fn mode_aware_init_writes_explicit_non_governed_postures() {
        for mode in [
            RuntimeOperatingMode::Standalone,
            RuntimeOperatingMode::Development,
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            init_home_dir_with_mode(tmp.path(), mode).expect("init");
            let config = fs::read_to_string(tmp.path().join("config.yaml")).expect("config");
            assert!(config.contains(&format!("mode: {}", mode.as_str())));
            assert!(config.contains("profile: permissive"));
            assert!(config.contains("process: false"));
        }
    }

    #[test]
    fn governed_init_creates_trust_store_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");

        init_home_dir_with_mode(tmp.path(), RuntimeOperatingMode::Governed).expect("init");

        assert!(transist_storage_local::trust_store_dir(tmp.path()).is_dir());
        assert!(tmp.path().join("policy/workflows").is_dir());
        assert!(tmp.path().join("policy/workflow-authorizations").is_dir());
        assert!(
            fs::read_to_string(tmp.path().join("config.yaml"))
                .expect("config")
                .contains("sources: [workflows, workflow-authorizations]")
        );
    }

    #[test]
    fn mode_aware_init_preserves_existing_configuration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("config.yaml"),
            "version: v1\ncustom: true\n",
        )
        .expect("seed config");
        init_home_dir_with_mode(tmp.path(), RuntimeOperatingMode::Governed).expect("init");
        assert_eq!(
            fs::read_to_string(tmp.path().join("config.yaml")).expect("config"),
            "version: v1\ncustom: true\n"
        );
    }
}
