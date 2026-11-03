// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use transist_protocol::Recipe;

/// One compiled workflow recipe discovered in a local Transist home.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledWorkflow {
    /// Stable workflow id without version decoration.
    pub id: String,
    /// Stable workflow version string.
    pub version: String,
    /// Absolute path to the compiled recipe file on disk.
    pub path: PathBuf,
}

/// Lists compiled workflow recipes persisted under an explicit Transist home.
pub fn list_recipes_in_home(home_dir: &Path) -> Result<Vec<InstalledWorkflow>> {
    let root = home_dir.join("registry").join("recipes");
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut recipes = Vec::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("failed reading recipe registry: {}", root.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed reading recipe registry entry: {}", root.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed reading compiled recipe: {}", path.display()))?;
        let recipe: Recipe = yaml_serde::from_str(&raw)
            .with_context(|| format!("invalid compiled recipe yaml: {}", path.display()))?;
        recipes.push(InstalledWorkflow {
            id: recipe.workflow.id,
            version: recipe.workflow.version,
            path,
        });
    }

    recipes.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.version.cmp(&right.version))
    });
    Ok(recipes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use transist_protocol::{PrerequisiteSet, Recipe, WorkflowRef};

    #[test]
    fn list_recipes_in_home_returns_empty_when_registry_missing() {
        let temp = tempdir().expect("tempdir");
        let listed = list_recipes_in_home(temp.path()).expect("list recipes");
        assert!(listed.is_empty());
    }

    #[test]
    fn list_recipes_in_home_returns_single_recipe() {
        let temp = tempdir().expect("tempdir");
        write_recipe(
            temp.path(),
            recipe("single_workflow", "v1", temp.path().join("single-v1.yaml")),
        );

        let listed = list_recipes_in_home(temp.path()).expect("list recipes");
        assert_eq!(
            listed,
            vec![InstalledWorkflow {
                id: "single_workflow".to_string(),
                version: "v1".to_string(),
                path: temp
                    .path()
                    .join("registry")
                    .join("recipes")
                    .join("single_workflow-v1.yaml"),
            }]
        );
    }

    #[test]
    fn list_recipes_in_home_sorts_by_workflow_identity() {
        let temp = tempdir().expect("tempdir");
        write_recipe(
            temp.path(),
            recipe("zebra_workflow", "v2", temp.path().join("zebra-v2.yaml")),
        );
        write_recipe(
            temp.path(),
            recipe("alpha_workflow", "v9", temp.path().join("alpha-v9.yaml")),
        );
        write_recipe(
            temp.path(),
            recipe("alpha_workflow", "v1", temp.path().join("alpha-v1.yaml")),
        );

        let listed = list_recipes_in_home(temp.path()).expect("list recipes");
        assert_eq!(
            listed,
            vec![
                InstalledWorkflow {
                    id: "alpha_workflow".to_string(),
                    version: "v1".to_string(),
                    path: temp
                        .path()
                        .join("registry")
                        .join("recipes")
                        .join("alpha_workflow-v1.yaml"),
                },
                InstalledWorkflow {
                    id: "alpha_workflow".to_string(),
                    version: "v9".to_string(),
                    path: temp
                        .path()
                        .join("registry")
                        .join("recipes")
                        .join("alpha_workflow-v9.yaml"),
                },
                InstalledWorkflow {
                    id: "zebra_workflow".to_string(),
                    version: "v2".to_string(),
                    path: temp
                        .path()
                        .join("registry")
                        .join("recipes")
                        .join("zebra_workflow-v2.yaml"),
                },
            ]
        );
    }

    fn write_recipe(home_dir: &Path, recipe: Recipe) {
        crate::write_recipe_in_home(home_dir, &recipe).expect("write recipe");
    }

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
}
