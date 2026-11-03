// SPDX-License-Identifier: MPL-2.0
#[cfg(not(feature = "dev-version-bypass"))]
use crate::flow::is_reserved_test_workflow_version;
use crate::flow::{Flow, is_test_workflow};
use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use transist_protocol::Recipe;

static RECIPE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Loads and validates a flow from disk.
pub fn load_flow(path: &Path) -> Result<Flow> {
    let flow = load_flow_unvalidated(path)?;
    crate::validate::validate_flow(&flow)?;
    Ok(flow)
}

/// Loads a flow from disk without running semantic validation.
pub fn load_flow_unvalidated(path: &Path) -> Result<Flow> {
    load_flow_document(path, true)
}

/// Loads a child flow or package-local module without semantic validation.
pub(crate) fn load_child_flow_unvalidated(path: &Path) -> Result<Flow> {
    load_flow_document(path, false)
}

fn load_flow_document(path: &Path, root: bool) -> Result<Flow> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed resolving flow file: {}", path.display()))?;
    let raw = std::fs::read_to_string(&canonical)
        .with_context(|| format!("failed reading flow file: {}", canonical.display()))?;
    let mut flow = if root {
        crate::modules::parse_root_flow(&raw)
    } else {
        crate::modules::parse_child_flow(&raw)
    }
    .with_context(|| format!("invalid flow yaml: {}", canonical.display()))?;
    flow.source_path = Some(canonical);
    Ok(flow)
}

/// Writes a compiled recipe into the repository-local Transist home.
pub fn write_recipe(repo_root: &Path, recipe: &Recipe) -> Result<PathBuf> {
    write_recipe_in_home(&repo_root.join(".transist"), recipe)
}

/// Writes a compiled recipe into an explicit Transist home.
///
/// The registry path `registry/recipes/<workflow-id>-<version>.yaml` is
/// content-immutable for non-test workflows: a missing target is written,
/// byte-identical content is a no-op, and different content under the same
/// workflow id+version is rejected so that a workflow version always resolves
/// to the same bytes. The reserved test workflow version `v0` is the one
/// supported exception and may overwrite the existing registry recipe.
pub fn write_recipe_in_home(home_dir: &Path, recipe: &Recipe) -> Result<PathBuf> {
    #[cfg(not(feature = "dev-version-bypass"))]
    if is_reserved_test_workflow_version(&recipe.workflow.version) {
        bail!(
            "workflow '{}' uses reserved test version '{}'; rebuild with dev-version-bypass to use development-only test workflows",
            recipe.workflow.id,
            recipe.workflow.version
        );
    }

    let recipe_path = home_dir.join("registry/recipes").join(format!(
        "{}-{}.yaml",
        recipe.workflow.id, recipe.workflow.version
    ));
    let parent = recipe_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("recipe path has no parent"))?;
    create_dir_all_durable(parent)
        .with_context(|| format!("failed creating recipe dir: {}", parent.display()))?;
    let yaml = yaml_serde::to_string(recipe).context("failed serializing recipe yaml")?;
    match fs::read_to_string(&recipe_path) {
        Ok(existing) => {
            if existing == yaml {
                return Ok(recipe_path);
            }
            if is_test_workflow(&recipe.workflow.version) {
                tracing::warn!(
                    workflow_id = %recipe.workflow.id,
                    version = %recipe.workflow.version,
                    path = %recipe_path.display(),
                    "overwriting compiled recipe for test workflow"
                );
            } else {
                bail!(
                    "workflow '{}' version '{}' is already compiled with different content; \
                     bump the workflow version to publish the change",
                    recipe.workflow.id,
                    recipe.workflow.version
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed reading recipe file: {}", recipe_path.display()));
        }
    }
    if is_test_workflow(&recipe.workflow.version) {
        atomic_replace(&recipe_path, yaml.as_bytes())?;
    } else if !atomic_publish_new(&recipe_path, yaml.as_bytes())? {
        let existing = fs::read_to_string(&recipe_path).with_context(|| {
            format!(
                "failed reading concurrently published recipe: {}",
                recipe_path.display()
            )
        })?;
        if existing != yaml {
            bail!(
                "workflow '{}' version '{}' is already compiled with different content; \
                 bump the workflow version to publish the change",
                recipe.workflow.id,
                recipe.workflow.version
            );
        }
        sync_directory(parent)?;
    }
    Ok(recipe_path)
}

fn atomic_publish_new(path: &Path, payload: &[u8]) -> Result<bool> {
    let parent = path.parent().expect("recipe path has a parent");
    let (temp_path, mut temp_file) = create_recipe_temp(path)?;
    let result = (|| -> Result<bool> {
        temp_file
            .write_all(payload)
            .with_context(|| format!("failed writing recipe temp: {}", temp_path.display()))?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed syncing recipe temp: {}", temp_path.display()))?;
        match fs::hard_link(&temp_path, path) {
            Ok(()) => {
                sync_directory(parent)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("failed publishing recipe: {}", path.display()))
            }
        }
    })();
    let cleanup = fs::remove_file(&temp_path)
        .with_context(|| format!("failed removing recipe temp: {}", temp_path.display()))
        .and_then(|()| sync_directory(parent));
    match result {
        Ok(published) => {
            cleanup?;
            Ok(published)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

fn atomic_replace(path: &Path, payload: &[u8]) -> Result<()> {
    let parent = path.parent().expect("recipe path has a parent");
    let (temp_path, mut temp_file) = create_recipe_temp(path)?;
    let result = (|| -> Result<()> {
        temp_file
            .write_all(payload)
            .with_context(|| format!("failed writing recipe temp: {}", temp_path.display()))?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed syncing recipe temp: {}", temp_path.display()))?;
        fs::rename(&temp_path, path)
            .with_context(|| format!("failed replacing recipe: {}", path.display()))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

fn create_recipe_temp(path: &Path) -> Result<(PathBuf, fs::File)> {
    let parent = path.parent().expect("recipe path has a parent");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recipe");
    loop {
        let counter = RECIPE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), counter));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed creating recipe temp: {}", temp_path.display())
                });
            }
        }
    }
}

fn create_dir_all_durable(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|parent| *parent != path)
        .ok_or_else(|| anyhow::anyhow!("recipe directory '{}' has no parent", path.display()))?;
    create_dir_all_durable(parent)?;
    match fs::create_dir(path) {
        Ok(()) => {
            sync_directory(path)?;
            sync_directory(parent)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed creating recipe directory: {}", path.display())),
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)
        .with_context(|| format!("failed opening recipe directory: {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed syncing recipe directory: {}", path.display()))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn resolve_relative_path(base_flow: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        base_flow
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;
    use transist_protocol::{PrerequisiteSet, Recipe, WorkflowRef};

    fn recipe(id: &str, version: &str, source_dir: PathBuf) -> Recipe {
        Recipe {
            schema: crate::RECIPE_SCHEMA_V1.to_string(),
            workflow: WorkflowRef {
                id: id.to_string(),
                version: version.to_string(),
            },
            source_dir: Some(source_dir.display().to_string()),
            workspace: None,
            repositories: Vec::new(),
            prerequisites: PrerequisiteSet::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: Default::default(),
            steps: Vec::new(),
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    #[test]
    fn writes_recipe_when_target_missing() {
        let temp = tempdir().expect("tempdir");
        let recipe = recipe("wf", "v1", temp.path().join("wf-v1.yaml"));
        let path = write_recipe_in_home(temp.path(), &recipe).expect("write");
        assert!(path.ends_with("registry/recipes/wf-v1.yaml"));
        assert!(path.exists());
    }

    #[test]
    fn identical_content_is_noop() {
        let temp = tempdir().expect("tempdir");
        let recipe = recipe("wf", "v1", temp.path().join("wf-v1.yaml"));
        let path = write_recipe_in_home(temp.path(), &recipe).expect("first write");
        let written = std::fs::read_to_string(&path).expect("read");
        write_recipe_in_home(temp.path(), &recipe).expect("second write");
        assert_eq!(std::fs::read_to_string(&path).expect("reread"), written);
    }

    #[test]
    fn different_content_under_same_version_is_rejected() {
        let temp = tempdir().expect("tempdir");
        let first = recipe("wf", "v1", temp.path().join("wf-v1.yaml"));
        write_recipe_in_home(temp.path(), &first).expect("first write");
        let second = recipe("wf", "v1", temp.path().join("changed-v1.yaml"));
        let err = write_recipe_in_home(temp.path(), &second).unwrap_err();
        assert!(
            err.to_string()
                .contains("already compiled with different content"),
            "expected immutability error, got: {err}"
        );
    }

    #[test]
    fn concurrent_publication_never_clobbers_immutable_content() {
        const WRITERS: usize = 12;
        let temp = tempdir().expect("tempdir");
        let barrier = Arc::new(Barrier::new(WRITERS));
        let threads = (0..WRITERS)
            .map(|index| {
                let home = temp.path().to_path_buf();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let recipe = recipe("wf", "v1", PathBuf::from(format!("source-{index}")));
                    barrier.wait();
                    (recipe.clone(), write_recipe_in_home(&home, &recipe))
                })
            })
            .collect::<Vec<_>>();
        let outcomes = threads
            .into_iter()
            .map(|thread| thread.join().expect("writer thread"))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );

        let winner = outcomes
            .iter()
            .find_map(|(recipe, result)| result.is_ok().then_some(recipe))
            .expect("winner");
        let path = temp.path().join("registry/recipes/wf-v1.yaml");
        assert_eq!(
            fs::read_to_string(path).expect("published recipe"),
            yaml_serde::to_string(winner).expect("serialize winner")
        );
    }

    #[test]
    fn changed_version_writes_new_target() {
        let temp = tempdir().expect("tempdir");
        let v1 = recipe("wf", "v1", temp.path().join("wf-v1.yaml"));
        write_recipe_in_home(temp.path(), &v1).expect("v1 write");
        let v2 = recipe("wf", "v2", temp.path().join("wf-v2.yaml"));
        let path = write_recipe_in_home(temp.path(), &v2).expect("v2 write");
        assert!(path.ends_with("registry/recipes/wf-v2.yaml"));
        assert!(path.exists());
    }

    #[cfg(feature = "dev-version-bypass")]
    #[test]
    fn test_workflow_version_overwrites_existing_recipe() {
        let temp = tempdir().expect("tempdir");
        let first = recipe(
            "wf",
            crate::TEST_WORKFLOW_VERSION,
            temp.path().join("wf-v0.yaml"),
        );
        let path = write_recipe_in_home(temp.path(), &first).expect("first write");
        let second = recipe(
            "wf",
            crate::TEST_WORKFLOW_VERSION,
            temp.path().join("changed-v0.yaml"),
        );
        write_recipe_in_home(temp.path(), &second).expect("overwrite");
        let written = std::fs::read_to_string(&path).expect("read overwritten recipe");
        let expected = yaml_serde::to_string(&second).expect("serialize expected recipe");
        assert_eq!(written, expected);
    }

    #[cfg(not(feature = "dev-version-bypass"))]
    #[test]
    fn reserved_test_workflow_version_is_rejected_in_release_builds() {
        let temp = tempdir().expect("tempdir");
        let recipe = recipe(
            "wf",
            crate::TEST_WORKFLOW_VERSION,
            temp.path().join("wf-v0.yaml"),
        );
        let err = write_recipe_in_home(temp.path(), &recipe).unwrap_err();
        assert!(
            err.to_string().contains("reserved test version"),
            "expected reserved-version error, got: {err}"
        );
    }
}
