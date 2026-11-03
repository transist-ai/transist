// SPDX-License-Identifier: MPL-2.0
//! Run-level asset lock construction and verification.
//!
//! At run creation the kernel materializes a [`AssetLock`] per pinned plugin
//! and resolved prompt file so re-entry paths can verify byte-level
//! immutability without re-parsing the recipe snapshot.

use crate::types::{AssetLock, AssetLockKind, PersistedRunRecord};
use anyhow::Result;
use std::path::Path;
use transist_protocol::{Recipe, RecipePromptSource, RecipeStep, StepExecutionDescriptor};

/// Builds the run-level asset lock list for a recipe at run creation time.
///
/// Plugin locks are taken from every invoke step's `plugin_version` and
/// `plugin_sha256`. Prompt-file locks resolve the authored `prompt_file`
/// path against the recipe source directory and the provided workflow
/// inputs, hashing the resolved file's bytes. Inline prompts and embedded
/// skills are already part of the recipe snapshot and need no separate lock.
///
/// Locks are scoped to the **current** recipe only: nested subflow steps
/// (`StepExecutionDescriptor::Subflow`) are skipped. Each subflow child run
/// builds its own asset locks from the child recipe at its own run-creation
/// time, so flattening child locks into the parent would produce duplicate
/// matches (same `ref_id` + `step.id`) at dispatch time and incorrectly
/// reject the run.
pub fn build_asset_locks(
    recipe: &Recipe,
    workflow_inputs: &std::collections::BTreeMap<String, transist_protocol::KernelValue>,
    workflow_vars: &std::collections::BTreeMap<String, transist_protocol::KernelValue>,
    prompt_files: &dyn crate::ports::PromptFilePort,
) -> Result<Vec<AssetLock>> {
    let source_dir = recipe.source_dir.as_deref().map(std::path::PathBuf::from);
    let mut locks: Vec<AssetLock> = Vec::new();
    let mut error: Option<anyhow::Error> = None;

    // Collect the direct (non-recursive) invoke steps of this recipe: the
    // main `steps` list plus the `cleanup` steps. Subflow descriptors are
    // intentionally excluded; their child runs own their own locks.
    let mut direct_steps: Vec<(&RecipeStep, bool)> =
        Vec::with_capacity(recipe.steps.len() + recipe.cleanup.len());
    for step in &recipe.steps {
        if matches!(step.execution, StepExecutionDescriptor::Subflow { .. }) {
            continue;
        }
        direct_steps.push((step, false));
    }
    for cleanup in &recipe.cleanup {
        if matches!(
            cleanup.step.execution,
            StepExecutionDescriptor::Subflow { .. }
        ) {
            continue;
        }
        direct_steps.push((&cleanup.step, true));
    }

    for (step, _is_cleanup) in direct_steps {
        if error.is_some() {
            break;
        }
        if let StepExecutionDescriptor::Invoke {
            plugin,
            plugin_version: Some(version),
            plugin_sha256: Some(sha),
            ..
        } = &step.execution
        {
            locks.push(AssetLock {
                step_id: step.id.clone(),
                kind: AssetLockKind::Plugin,
                ref_id: plugin.clone(),
                version: Some(version.clone()),
                sha256: sha.clone(),
            });
        }
        if let Some(binding) = &step.prompt_binding {
            if let RecipePromptSource::File { path } = &binding.source {
                let resolved = match crate::dispatch::resolve_string_input(
                    path,
                    workflow_inputs,
                    workflow_vars,
                    "prompt_file",
                ) {
                    Ok(resolved) => resolved,
                    Err(err) => {
                        error = Some(err);
                        break;
                    }
                };
                let prompt = match prompt_files.resolve_prompt_file(
                    &resolved,
                    source_dir.as_deref().map(Path::new),
                    &step.id,
                ) {
                    Ok(prompt) => prompt,
                    Err(err) => {
                        error = Some(err);
                        break;
                    }
                };
                locks.push(AssetLock {
                    step_id: step.id.clone(),
                    kind: AssetLockKind::PromptFile,
                    ref_id: path.clone(),
                    version: None,
                    sha256: prompt.sha256,
                });
            }
        }
    }

    if let Some(error) = error {
        return Err(error);
    }

    Ok(locks)
}

#[derive(Debug, Clone)]
pub(crate) struct AssetLockCheckOutcome {
    pub(crate) lock: AssetLock,
    pub(crate) error: Option<String>,
}

pub(crate) fn collect_asset_lock_checks(
    record: &PersistedRunRecord,
    recipe: &Recipe,
    home_dir: &Path,
    plugin_verifier: &dyn crate::ports::PluginVerificationPort,
    prompt_files: &dyn crate::ports::PromptFilePort,
) -> Result<Vec<AssetLockCheckOutcome>> {
    let mut outcomes = Vec::with_capacity(record.asset_locks.len());
    let is_test_workflow = crate::kernel::test_mode::is_test_workflow(&recipe.workflow.version);
    for lock in &record.asset_locks {
        let error = match lock.kind {
            AssetLockKind::Plugin if is_test_workflow => None,
            AssetLockKind::Plugin => verify_plugin_lock(lock, home_dir, plugin_verifier)
                .err()
                .map(|error| error.to_string()),
            AssetLockKind::PromptFile if record.uses_workflow_asset_snapshot() => None,
            AssetLockKind::PromptFile => resolve_locked_prompt_file_path(
                lock,
                recipe,
                &record.workflow_inputs,
                &record.workflow_vars,
                prompt_files,
            )
            .and_then(|prompt| verify_prompt_file_lock(lock, &prompt))
            .err()
            .map(|error| error.to_string()),
        };
        outcomes.push(AssetLockCheckOutcome {
            lock: lock.clone(),
            error,
        });
    }
    Ok(outcomes)
}

/// Verifies the persisted asset locks against the current installed plugin
/// and prompt-file bytes. Returns the first mismatch as an error.
pub fn verify_asset_locks(
    record: &PersistedRunRecord,
    recipe: &Recipe,
    home_dir: &Path,
    plugin_verifier: &dyn crate::ports::PluginVerificationPort,
    prompt_files: &dyn crate::ports::PromptFilePort,
) -> Result<()> {
    for outcome in
        collect_asset_lock_checks(record, recipe, home_dir, plugin_verifier, prompt_files)?
    {
        if let Some(error) = outcome.error {
            anyhow::bail!(error);
        }
    }
    Ok(())
}

fn verify_plugin_lock(
    lock: &AssetLock,
    home_dir: &Path,
    plugin_verifier: &dyn crate::ports::PluginVerificationPort,
) -> Result<()> {
    if lock.version.is_none() {
        anyhow::bail!(
            "plugin lock for step '{}' ('{}') has no version",
            lock.step_id,
            lock.ref_id
        );
    }
    plugin_verifier.verify_plugin_digest(home_dir, lock)
}

fn resolve_locked_prompt_file_path(
    lock: &AssetLock,
    recipe: &Recipe,
    workflow_inputs: &std::collections::BTreeMap<String, transist_protocol::KernelValue>,
    workflow_vars: &std::collections::BTreeMap<String, transist_protocol::KernelValue>,
    prompt_files: &dyn crate::ports::PromptFilePort,
) -> Result<crate::ports::PromptFileReference> {
    let source_dir = recipe.source_dir.as_deref().map(Path::new);
    let resolved = crate::dispatch::resolve_string_input(
        &lock.ref_id,
        workflow_inputs,
        workflow_vars,
        "prompt_file",
    )?;
    prompt_files.resolve_prompt_file(&resolved, source_dir, &lock.step_id)
}

fn verify_prompt_file_lock(
    lock: &AssetLock,
    prompt: &crate::ports::PromptFileReference,
) -> Result<()> {
    if prompt.sha256 != lock.sha256 {
        anyhow::bail!(
            "prompt_file '{}' locked by step '{}' has changed; \
             expected digest '{}' but found '{}'",
            prompt.path.display(),
            lock.step_id,
            lock.sha256,
            prompt.sha256
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AssetLockKind;
    use std::collections::BTreeMap;
    use tempfile::tempdir;
    use transist_protocol::{
        AdapterKind, CleanupTrigger, Recipe, RecipeCleanupStep, RecipePromptBinding,
        RecipePromptSource, RecipeStep, StepExecutionDescriptor, WorkflowRef,
    };

    struct AllowPluginVerification;

    struct TestPromptFiles;

    impl crate::ports::PromptFilePort for TestPromptFiles {
        fn resolve_prompt_file(
            &self,
            raw_path: &str,
            recipe_source_dir: Option<&Path>,
            _step_id: &str,
        ) -> Result<crate::ports::PromptFileReference> {
            let path = Path::new(raw_path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                recipe_source_dir
                    .ok_or_else(|| anyhow::anyhow!("missing prompt source directory"))?
                    .join(path)
            };
            Ok(crate::ports::PromptFileReference {
                sha256: crate::kernel::sha256_hex(&std::fs::read(&resolved)?),
                path: resolved,
                content: String::new(),
            })
        }
    }

    impl crate::ports::PluginVerificationPort for AllowPluginVerification {
        fn verify_plugin_digest(&self, _home_dir: &Path, _lock: &AssetLock) -> Result<()> {
            Ok(())
        }
    }

    fn invoke_step(
        id: &str,
        plugin: &str,
        version: Option<String>,
        sha: Option<String>,
    ) -> RecipeStep {
        RecipeStep {
            id: id.to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some(plugin.to_string()),
                plugin: plugin.to_string(),
                action: "run".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: version,
                plugin_sha256: sha,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    fn recipe(steps: Vec<RecipeStep>, source_dir: Option<String>) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "test_wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: Default::default(),
            steps,
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    fn cleanup_step(step: RecipeStep) -> RecipeCleanupStep {
        RecipeCleanupStep {
            when: vec![CleanupTrigger::Failed],
            step,
        }
    }

    #[test]
    fn build_asset_locks_captures_pinned_plugins() {
        let step = invoke_step(
            "s1",
            "shell",
            Some("1.0.0".to_string()),
            Some("abc123".to_string()),
        );
        let recipe = recipe(vec![step], None);
        let inputs = BTreeMap::new();
        let vars = BTreeMap::new();
        let locks =
            build_asset_locks(&recipe, &inputs, &vars, &TestPromptFiles).expect("build locks");
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].kind, AssetLockKind::Plugin);
        assert_eq!(locks[0].ref_id, "shell");
        assert_eq!(locks[0].version.as_deref(), Some("1.0.0"));
        assert_eq!(locks[0].sha256, "abc123");
    }

    #[test]
    fn build_asset_locks_captures_prompt_files() {
        let tmp = tempdir().expect("tempdir");
        let prompt_path = tmp.path().join("prompt.txt");
        std::fs::write(&prompt_path, "hello prompt").expect("write prompt");
        let mut step = invoke_step("s1", "shell", None, None);
        step.prompt_binding = Some(RecipePromptBinding {
            inline_field: "prompt".to_string(),
            file_field: Some("prompt_file".to_string()),
            source: RecipePromptSource::File {
                path: prompt_path.display().to_string(),
            },
        });
        let recipe = recipe(vec![step], None);
        let inputs = BTreeMap::new();
        let vars = BTreeMap::new();
        let locks =
            build_asset_locks(&recipe, &inputs, &vars, &TestPromptFiles).expect("build locks");
        let prompt_lock = locks
            .iter()
            .find(|l| l.kind == AssetLockKind::PromptFile)
            .expect("prompt file lock");
        assert_eq!(prompt_lock.ref_id, prompt_path.display().to_string());
        assert!(!prompt_lock.sha256.is_empty());
    }

    #[test]
    fn build_asset_locks_captures_cleanup_prompt_file() {
        let tmp = tempdir().expect("tempdir");
        let prompt_path = tmp.path().join("cleanup-prompt.txt");
        std::fs::write(&prompt_path, "cleanup prompt").expect("write prompt");
        let mut cleanup = invoke_step("cleanup_step", "shell", None, None);
        cleanup.prompt_binding = Some(RecipePromptBinding {
            inline_field: "prompt".to_string(),
            file_field: Some("prompt_file".to_string()),
            source: RecipePromptSource::File {
                path: prompt_path.display().to_string(),
            },
        });
        let mut recipe = recipe(Vec::new(), None);
        recipe.cleanup = vec![cleanup_step(cleanup)];

        let locks = build_asset_locks(
            &recipe,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &TestPromptFiles,
        )
        .expect("build locks");
        let prompt_lock = locks
            .iter()
            .find(|lock| lock.kind == AssetLockKind::PromptFile)
            .expect("cleanup prompt lock");
        assert_eq!(prompt_lock.step_id, "cleanup_step");
        assert_eq!(prompt_lock.ref_id, prompt_path.display().to_string());
    }

    #[test]
    fn build_asset_locks_skips_subflow_invoke_steps() {
        let child_step = invoke_step(
            "child_step",
            "shell",
            Some("1.0.0".to_string()),
            Some("child-sha".to_string()),
        );
        let child_recipe = recipe(vec![child_step], None);
        let parent_subflow = RecipeStep {
            id: "subflow_step".to_string(),
            execution: StepExecutionDescriptor::Subflow {
                recipe: Box::new(child_recipe),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let recipe = recipe(vec![parent_subflow], None);

        let locks = build_asset_locks(
            &recipe,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &TestPromptFiles,
        )
        .expect("build locks");
        assert!(
            locks.is_empty(),
            "subflow child steps must not contribute locks to the parent recipe; got {locks:?}"
        );
    }

    /// Regression for Finding 3: a parent recipe and a nested subflow that
    /// share the same step id and the same `prompt_file` path must not
    /// produce duplicate prompt-file locks in the parent run. The parent
    /// run's locks are scoped to its own direct steps; the child run owns
    /// its own locks. Without scoping, `prompt_file_lock_for_dispatch`
    /// would see two matches and reject the run as a duplicate.
    #[test]
    fn build_asset_locks_does_not_duplicate_subflow_prompt_locks() {
        let tmp = tempdir().expect("tempdir");
        let prompt_path = tmp.path().join("shared-prompt.txt");
        std::fs::write(&prompt_path, "shared prompt body").expect("write prompt");

        let prompt_binding = RecipePromptBinding {
            inline_field: "prompt".to_string(),
            file_field: Some("prompt_file".to_string()),
            source: RecipePromptSource::File {
                path: prompt_path.display().to_string(),
            },
        };

        // Child invoke step reuses the SAME step id and SAME prompt_file path
        // as the parent step below.
        let mut child_step = invoke_step("shared_step", "shell", None, None);
        child_step.prompt_binding = Some(prompt_binding.clone());
        let child_recipe = recipe(vec![child_step], None);

        let parent_subflow = RecipeStep {
            id: "subflow_step".to_string(),
            execution: StepExecutionDescriptor::Subflow {
                recipe: Box::new(child_recipe),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };

        let mut parent_step = invoke_step("shared_step", "shell", None, None);
        parent_step.prompt_binding = Some(prompt_binding);

        let recipe = recipe(vec![parent_subflow, parent_step], None);

        let locks = build_asset_locks(
            &recipe,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &TestPromptFiles,
        )
        .expect("build locks");

        // Exactly one prompt-file lock, attributable solely to the parent's
        // direct `shared_step`. The subflow child's lock must not appear.
        let prompt_locks: Vec<_> = locks
            .iter()
            .filter(|lock| lock.kind == AssetLockKind::PromptFile)
            .collect();
        assert_eq!(
            prompt_locks.len(),
            1,
            "expected exactly one prompt-file lock for the parent step, got {prompt_locks:?}"
        );
        assert_eq!(prompt_locks[0].step_id, "shared_step");
        assert_eq!(prompt_locks[0].ref_id, prompt_path.display().to_string());
    }

    #[test]
    fn build_asset_locks_skips_unpinned_plugins() {
        let step = invoke_step("s1", "shell", None, None);
        let recipe = recipe(vec![step], None);
        let inputs = BTreeMap::new();
        let vars = BTreeMap::new();
        let locks =
            build_asset_locks(&recipe, &inputs, &vars, &TestPromptFiles).expect("build locks");
        assert!(locks.is_empty(), "no locks expected without plugin_sha256");
    }

    #[test]
    fn verify_asset_locks_fails_on_missing_prompt_file() {
        let lock = AssetLock {
            step_id: "s1".to_string(),
            kind: AssetLockKind::PromptFile,
            ref_id: "missing-prompt.txt".to_string(),
            version: None,
            sha256: "deadbeef".to_string(),
        };
        let tmp = tempdir().expect("tempdir");
        let record = PersistedRunRecord {
            schema: crate::infra::records::schema_versions::RUN_STATE_RECORD_SCHEMA_V1.to_string(),
            run_id: "run_1".to_string(),
            workflow_id: "wf".to_string(),
            workflow_version: "v1".to_string(),
            status: crate::types::KernelRunStatus::Created,
            steps: BTreeMap::new(),
            executions: BTreeMap::new(),
            recipe_snapshot: None,
            lineage: None,
            resume_count: 0,
            workflow_inputs: BTreeMap::new(),
            workspace: None,
            workflow_vars: BTreeMap::new(),
            recovery_state: None,
            cleanup: None,
            preflight: None,
            trusted_execution_mode: false,
            trust_decision: None,
            local_only_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            asset_locks: vec![lock],
            asset_lock_version: 1,
            workflow_asset_snapshot: false,
            test_mode: false,
            capability_authority_version: 0,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        let recipe = recipe(Vec::new(), Some(tmp.path().display().to_string()));
        let err = verify_asset_locks(
            &record,
            &recipe,
            tmp.path(),
            &AllowPluginVerification,
            &TestPromptFiles,
        )
        .unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
