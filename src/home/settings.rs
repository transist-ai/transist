// SPDX-License-Identifier: MPL-2.0
use super::resolution::absolute_from;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use transist_storage_local::write_string_atomic;

const SETTINGS_SCHEMA_V1: &str = "transist.user-settings.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UserSettings {
    schema: String,
    #[serde(default)]
    home: UserHomeSettings,
    #[serde(default, skip_serializing_if = "UserIdentitySettings::is_empty")]
    identity: UserIdentitySettings,
    #[serde(default, skip_serializing_if = "UserToolSettings::is_empty")]
    tools: UserToolSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UserHomeSettings {
    #[serde(default)]
    global_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UserIdentitySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
}

impl UserIdentitySettings {
    fn is_empty(&self) -> bool {
        self.actor.is_none()
    }
}

/// Operator preferences for invoking external file viewers/editors from the
/// TUI. Both fields are arbitrary command strings (e.g. `"less -R"`,
/// `"bat --paging=always"`, `"nvim"`, `"code --wait"`). When unset, the TUI
/// falls back to the legacy `$PAGER` env var (for the pager) or refuses to
/// open an editor (no `$EDITOR` fallback by design).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UserToolSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pager: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inspection_cache_runs: Option<usize>,
}

impl UserToolSettings {
    fn is_empty(&self) -> bool {
        self.pager.is_none() && self.editor.is_none() && self.inspection_cache_runs.is_none()
    }
}

/// Default number of runs tracked by the inspection cache when no user value
/// is configured.
pub const DEFAULT_INSPECTION_CACHE_RUNS: usize = 10;

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA_V1.to_string(),
            home: UserHomeSettings::default(),
            identity: UserIdentitySettings::default(),
            tools: UserToolSettings::default(),
        }
    }
}

/// Returns the path to the user-scoped Transist settings file.
pub fn user_settings_path() -> Result<PathBuf> {
    let base = platform_config_dir()?;
    Ok(base.join("transist").join("settings.yaml"))
}

/// Loads the persisted user-set Transist home path from the user settings file, if present.
///
/// Reads the on-disk key `home.global_root` for back-compatibility with existing settings files.
pub fn load_user_home_path() -> Result<Option<PathBuf>> {
    let path = user_settings_path()?;
    let settings = read_settings(&path)?.unwrap_or_default();
    validate_global_root(settings.home.global_root)
}

/// Saves the user-set Transist home path in the user settings file and initializes the target.
///
/// The target directory is initialized via [`crate::init_home_dir`] as an idempotent step, so
/// `transist home set ~/foo` immediately makes `~/foo` usable without a separate init call.
/// Returns the absolute path that was saved.
pub fn save_user_home_path(cwd: &Path, raw_path: &Path) -> Result<PathBuf> {
    let home_path = record_user_home_path(cwd, raw_path)?;
    crate::init::init_home_dir(&home_path)?;
    Ok(home_path)
}

/// Records `raw_path` as the persisted user-set Transist home without initializing it.
///
/// Callers that need to initialize the target with a specific runtime mode (rather than the
/// default governed mode applied by [`save_user_home_path`]) should call this first and then
/// initialize the directory themselves (e.g. via `init_home_dir_with_mode`).
/// Returns the absolute path that was recorded.
pub fn record_user_home_path(cwd: &Path, raw_path: &Path) -> Result<PathBuf> {
    let path = user_settings_path()?;
    let home_path = absolute_from(cwd, raw_path);
    let mut settings = read_settings(&path)?.unwrap_or_default();
    settings.home.global_root = Some(home_path.clone());
    write_settings(&path, &settings)?;
    Ok(home_path)
}

/// Removes the persisted user-set home configuration, if it exists.
pub fn clear_user_home_path() -> Result<()> {
    let path = user_settings_path()?;
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(());
    };
    settings.home.global_root = None;
    write_or_remove_settings(&path, &settings)
}

/// Loads the operator's preferred manual-gate actor identity, if configured.
pub fn load_user_gate_actor() -> Result<Option<String>> {
    let path = user_settings_path()?;
    Ok(read_settings(&path)?
        .and_then(|s| s.identity.actor)
        .filter(|s| !s.trim().is_empty()))
}

/// Persists the operator's preferred manual-gate actor identity. Trims whitespace; an
/// empty input is treated as "unset" and removes the field.
pub fn save_user_gate_actor(actor: &str) -> Result<()> {
    let path = user_settings_path()?;
    let trimmed = actor.trim();
    let mut settings = read_settings(&path)?.unwrap_or_default();
    settings.identity.actor = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    write_or_remove_settings(&path, &settings)
}

/// Removes the persisted preferred manual-gate actor identity, if any.
pub fn clear_user_gate_actor() -> Result<()> {
    let path = user_settings_path()?;
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(());
    };
    settings.identity.actor = None;
    write_or_remove_settings(&path, &settings)
}

// Back-compat aliases used by the old public API surface; callers should migrate to the new names.
#[doc(hidden)]
pub fn load_user_global_home_root() -> Result<Option<PathBuf>> {
    load_user_home_path()
}

#[doc(hidden)]
pub fn save_user_global_home_root(cwd: &Path, raw_path: &Path) -> Result<PathBuf> {
    save_user_home_path(cwd, raw_path)
}

#[doc(hidden)]
pub fn clear_user_global_home_root() -> Result<()> {
    clear_user_home_path()
}

/// Loads the operator's preferred pager command, if configured.
pub fn load_user_preferred_pager() -> Result<Option<String>> {
    let path = user_settings_path()?;
    Ok(read_settings(&path)?
        .and_then(|s| s.tools.pager)
        .filter(|s| !s.trim().is_empty()))
}

/// Persists the operator's preferred pager command. Trims whitespace; an
/// empty input is treated as "unset" and removes the field.
pub fn save_user_preferred_pager(command: &str) -> Result<()> {
    let path = user_settings_path()?;
    let trimmed = command.trim();
    let mut settings = read_settings(&path)?.unwrap_or_default();
    settings.tools.pager = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    write_or_remove_settings(&path, &settings)
}

/// Removes the persisted preferred pager command, if any.
pub fn clear_user_preferred_pager() -> Result<()> {
    let path = user_settings_path()?;
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(());
    };
    settings.tools.pager = None;
    write_or_remove_settings(&path, &settings)
}

/// Loads the operator's preferred editor command, if configured.
pub fn load_user_preferred_editor() -> Result<Option<String>> {
    let path = user_settings_path()?;
    Ok(read_settings(&path)?
        .and_then(|s| s.tools.editor)
        .filter(|s| !s.trim().is_empty()))
}

/// Persists the operator's preferred editor command. Trims whitespace; an
/// empty input is treated as "unset" and removes the field.
pub fn save_user_preferred_editor(command: &str) -> Result<()> {
    let path = user_settings_path()?;
    let trimmed = command.trim();
    let mut settings = read_settings(&path)?.unwrap_or_default();
    settings.tools.editor = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    write_or_remove_settings(&path, &settings)
}

/// Removes the persisted preferred editor command, if any.
pub fn clear_user_preferred_editor() -> Result<()> {
    let path = user_settings_path()?;
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(());
    };
    settings.tools.editor = None;
    write_or_remove_settings(&path, &settings)
}

/// Loads the persisted run-capacity limit for the inspection cache, if set.
pub fn load_user_inspection_cache_runs() -> Result<Option<usize>> {
    let path = user_settings_path()?;
    Ok(read_settings(&path)?.and_then(|s| s.tools.inspection_cache_runs))
}

/// Persists the run-capacity limit for the inspection cache.
pub fn save_user_inspection_cache_runs(value: usize) -> Result<()> {
    let path = user_settings_path()?;
    let mut settings = read_settings(&path)?.unwrap_or_default();
    settings.tools.inspection_cache_runs = Some(value);
    write_or_remove_settings(&path, &settings)
}

/// Removes the persisted inspection-cache run-capacity limit, if any.
pub fn clear_user_inspection_cache_runs() -> Result<()> {
    let path = user_settings_path()?;
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(());
    };
    settings.tools.inspection_cache_runs = None;
    write_or_remove_settings(&path, &settings)
}

fn read_settings(path: &Path) -> Result<Option<UserSettings>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading settings file: {}", path.display()))?;
    let settings: UserSettings = yaml_serde::from_str(&raw)
        .with_context(|| format!("invalid settings yaml: {}", path.display()))?;
    if settings.schema != SETTINGS_SCHEMA_V1 {
        bail!(
            "unsupported user settings schema '{}'; expected '{}'",
            settings.schema,
            SETTINGS_SCHEMA_V1
        );
    }
    Ok(Some(settings))
}

fn write_settings(path: &Path, settings: &UserSettings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating settings directory: {}", parent.display()))?;
    }
    let yaml = yaml_serde::to_string(settings).context("failed serializing settings yaml")?;
    write_string_atomic(path, &yaml)
        .with_context(|| format!("failed writing settings file: {}", path.display()))?;
    Ok(())
}

/// Persist `settings` if it carries any non-default field; otherwise remove
/// the file. This keeps the on-disk surface small: a fully-cleared settings
/// object becomes "no file" rather than an empty stub.
fn write_or_remove_settings(path: &Path, settings: &UserSettings) -> Result<()> {
    if settings.home.global_root.is_none()
        && settings.identity.is_empty()
        && settings.tools.is_empty()
    {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("failed removing settings file: {}", path.display()))?;
        }
        return Ok(());
    }
    write_settings(path, settings)
}

fn validate_global_root(root: Option<PathBuf>) -> Result<Option<PathBuf>> {
    match root {
        Some(root) if !root.is_absolute() => {
            bail!("user-set home must be absolute: {}", root.display())
        }
        other => Ok(other),
    }
}

fn platform_config_dir() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("APPDATA is not set"));
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        return Ok(home.join("Library").join("Application Support"));
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    Ok(home.join(".config"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn settings_path(dir: &TempDir) -> PathBuf {
        dir.path().join("settings.yaml")
    }

    #[test]
    fn round_trip_pager_setting() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.tools.pager = Some("less -R".to_string());
        write_settings(&path, &s).unwrap();
        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.tools.pager.as_deref(), Some("less -R"));
        assert!(loaded.tools.editor.is_none());
        assert!(loaded.home.global_root.is_none());
        assert!(loaded.identity.is_empty());
    }

    #[test]
    fn round_trip_gate_actor_setting() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.identity.actor = Some("reviewer@example.com".to_string());
        write_settings(&path, &s).unwrap();

        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(
            loaded.identity.actor.as_deref(),
            Some("reviewer@example.com")
        );
        assert!(loaded.tools.is_empty());
        assert!(loaded.home.global_root.is_none());
    }

    #[test]
    fn settings_serialization_matches_golden_bytes() {
        let mut settings = UserSettings::default();
        settings.identity.actor = Some("reviewer@example.com".to_string());
        let actual = yaml_serde::to_string(&settings).expect("serialize settings");
        let expected = fs::read("tests/fixtures/yaml_compatibility/settings.yaml")
            .expect("read settings golden");
        assert_eq!(actual.as_bytes(), expected.as_slice());
    }

    #[test]
    fn write_or_remove_drops_file_when_empty() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.tools.pager = Some("less".to_string());
        write_settings(&path, &s).unwrap();
        assert!(path.exists());

        s.tools.pager = None;
        write_or_remove_settings(&path, &s).unwrap();
        assert!(!path.exists(), "fully-cleared settings should remove file");
    }

    #[test]
    fn pager_change_preserves_global_root() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.home.global_root = Some(PathBuf::from("/abs/home"));
        s.tools.editor = Some("nvim".to_string());
        write_settings(&path, &s).unwrap();

        // Simulate "save_user_preferred_pager" without touching disk path
        // resolution by going through the same read/mutate/write helpers.
        let mut loaded = read_settings(&path).unwrap().unwrap();
        loaded.tools.pager = Some("less -R".to_string());
        write_or_remove_settings(&path, &loaded).unwrap();

        let after = read_settings(&path).unwrap().unwrap();
        assert_eq!(after.home.global_root, Some(PathBuf::from("/abs/home")));
        assert_eq!(after.tools.editor.as_deref(), Some("nvim"));
        assert_eq!(after.tools.pager.as_deref(), Some("less -R"));
    }

    #[test]
    fn legacy_yaml_without_tools_block_loads() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "schema: transist.user-settings.v1\nhome:\n  global_root: /abs/home\n",
        )
        .unwrap();
        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.home.global_root, Some(PathBuf::from("/abs/home")));
        assert!(loaded.tools.is_empty());
        assert!(loaded.identity.is_empty());
    }

    #[test]
    fn legacy_yaml_without_identity_block_loads() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "schema: transist.user-settings.v1\ntools:\n  pager: less -R\n",
        )
        .unwrap();

        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.tools.pager.as_deref(), Some("less -R"));
        assert!(loaded.identity.is_empty());
    }

    #[test]
    fn round_trip_inspection_cache_runs_setting() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.tools.inspection_cache_runs = Some(25);
        write_settings(&path, &s).unwrap();

        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.tools.inspection_cache_runs, Some(25));
        assert!(loaded.tools.pager.is_none());
        assert!(loaded.tools.editor.is_none());
    }

    #[test]
    fn legacy_yaml_without_inspection_cache_runs_loads() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "schema: transist.user-settings.v1\ntools:\n  pager: less -R\n",
        )
        .unwrap();

        let loaded = read_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.tools.pager.as_deref(), Some("less -R"));
        assert!(loaded.tools.inspection_cache_runs.is_none());
    }

    #[test]
    fn write_or_remove_drops_file_when_only_actor_is_cleared() {
        let dir = TempDir::new().unwrap();
        let path = settings_path(&dir);
        let mut s = UserSettings::default();
        s.identity.actor = Some("reviewer@example.com".to_string());
        write_settings(&path, &s).unwrap();
        assert!(path.exists());

        s.identity.actor = None;
        write_or_remove_settings(&path, &s).unwrap();
        assert!(!path.exists(), "fully-cleared settings should remove file");
    }

    #[test]
    fn validate_global_root_error_message_uses_user_set_home() {
        let result = validate_global_root(Some(PathBuf::from("relative/path")));
        let err = result.unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("user-set home must be absolute:"),
            "expected error to mention 'user-set home must be absolute', got: {msg}"
        );
    }
}
