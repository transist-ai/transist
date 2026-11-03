// SPDX-License-Identifier: MPL-2.0
//! Home-directory resolution helpers.
//!
//! Transist can operate either out of a repository-local `.transist` directory or a
//! user-set home. This module centralizes the precedence rules and the
//! small persisted settings file that records the user's global preference.

mod resolution;
mod settings;

#[cfg(test)]
pub(crate) use resolution::resolve_home_with_overrides;
pub use resolution::{
    HomeResolutionError, HomeSource, ResolvedHome, per_user_default_home, repo_local_home_dir,
    resolve_home, resolve_settings_payload,
};
pub use settings::{
    DEFAULT_INSPECTION_CACHE_RUNS, clear_user_gate_actor, clear_user_global_home_root,
    clear_user_home_path, clear_user_inspection_cache_runs, clear_user_preferred_editor,
    clear_user_preferred_pager, load_user_gate_actor, load_user_global_home_root,
    load_user_home_path, load_user_inspection_cache_runs, load_user_preferred_editor,
    load_user_preferred_pager, record_user_home_path, save_user_gate_actor,
    save_user_global_home_root, save_user_home_path, save_user_inspection_cache_runs,
    save_user_preferred_editor, save_user_preferred_pager, user_settings_path,
};

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolve_home_prefers_explicit_then_env_then_config_then_default_then_error() {
        let cwd = Path::new("/tmp/work");

        // --home wins over everything
        let resolved = resolve_home_with_overrides(
            cwd,
            Some(Path::new("cli-home")),
            Some(Path::new("/env-home")),
            Some(Path::new("/config-home")),
            Some(Path::new("/default-home")),
        )
        .unwrap();
        assert_eq!(resolved.source, HomeSource::CliOverride);
        assert_eq!(resolved.effective_home, cwd.join("cli-home"));

        // TRANSIST_HOME wins over persisted and the per-user default
        let resolved = resolve_home_with_overrides(
            cwd,
            None,
            Some(Path::new("/env-home")),
            Some(Path::new("/config-home")),
            Some(Path::new("/default-home")),
        )
        .unwrap();
        assert_eq!(resolved.source, HomeSource::EnvOverride);
        assert_eq!(resolved.effective_home, Path::new("/env-home"));

        // persisted user home wins over the per-user default when no env
        let resolved = resolve_home_with_overrides(
            cwd,
            None,
            None,
            Some(Path::new("/config-home")),
            Some(Path::new("/default-home")),
        )
        .unwrap();
        assert_eq!(resolved.source, HomeSource::UserConfig);
        assert_eq!(resolved.effective_home, Path::new("/config-home"));

        // per-user default used when nothing else is set
        let resolved =
            resolve_home_with_overrides(cwd, None, None, None, Some(Path::new("/default-home")))
                .unwrap();
        assert_eq!(resolved.source, HomeSource::PerUserDefault);
        assert_eq!(resolved.effective_home, Path::new("/default-home"));

        // error when nothing is set and no per-user default can be computed
        let err = resolve_home_with_overrides(cwd, None, None, None, None).unwrap_err();
        assert!(
            matches!(err, HomeResolutionError::NotSetAndNoLocalDir { .. }),
            "expected NotSetAndNoLocalDir, got: {:?}",
            err
        );
    }

    #[test]
    fn cwd_transist_dir_is_no_longer_discovered_implicitly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path();
        std::fs::create_dir_all(cwd.join(".transist")).expect("create .transist");

        // A `.transist` directory in cwd must no longer be discovered implicitly;
        // the per-user default is used instead.
        let resolved =
            resolve_home_with_overrides(cwd, None, None, None, Some(Path::new("/default-home")))
                .unwrap();
        assert_eq!(resolved.source, HomeSource::PerUserDefault);
        assert_eq!(resolved.effective_home, Path::new("/default-home"));
    }

    #[test]
    fn cli_override_wins_over_everything() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path();

        let resolved = resolve_home_with_overrides(
            cwd,
            Some(Path::new("/explicit")),
            None,
            None,
            Some(Path::new("/default-home")),
        )
        .unwrap();
        assert_eq!(resolved.source, HomeSource::CliOverride);
        assert_eq!(resolved.effective_home, Path::new("/explicit"));
    }

    #[test]
    fn env_override_wins_over_user_config_and_default() {
        let cwd = Path::new("/tmp/work");
        let resolved = resolve_home_with_overrides(
            cwd,
            None,
            Some(Path::new("/env-home")),
            Some(Path::new("/config-home")),
            Some(Path::new("/default-home")),
        )
        .unwrap();
        assert_eq!(resolved.source, HomeSource::EnvOverride);
        assert_eq!(resolved.effective_home, Path::new("/env-home"));
        assert_eq!(
            resolved.persisted_user_home,
            Some(Path::new("/config-home").to_path_buf())
        );
    }

    #[test]
    fn empty_env_home_is_treated_as_absent() {
        let cwd = Path::new("/tmp/work");
        let resolved = resolve_home_with_overrides(
            cwd,
            None,
            Some(Path::new("")),
            Some(Path::new("/config-home")),
            Some(Path::new("/default-home")),
        )
        .unwrap();
        // empty env value must not win; falls through to user config
        assert_eq!(resolved.source, HomeSource::UserConfig);
    }

    #[test]
    fn no_home_error_message_contains_cwd() {
        let cwd = Path::new("/my/special/dir");
        let err = resolve_home_with_overrides(cwd, None, None, None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("/my/special/dir"),
            "error should mention cwd: {msg}"
        );
        assert!(
            msg.contains("transist home init"),
            "error should suggest home init: {msg}"
        );
    }

    #[test]
    fn per_user_default_source_serializes_as_per_user_default() {
        let resolved = ResolvedHome {
            effective_home: std::path::PathBuf::from("/home/user/.local/state/transist"),
            source: HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let json = serde_json::to_value(&resolved).expect("serialize");
        assert_eq!(json["source"], "per_user_default");
    }

    #[test]
    fn resolved_home_json_keeps_stable_field_names() {
        let resolved = ResolvedHome {
            effective_home: Path::new("/home/transist").to_path_buf(),
            source: HomeSource::UserConfig,
            persisted_user_home: Some(Path::new("/global/root").to_path_buf()),
        };
        let json = serde_json::to_value(&resolved).expect("serialize");
        let obj = json.as_object().expect("json object");
        assert!(
            obj.contains_key("effective_home"),
            "JSON must have 'effective_home'"
        );
        assert!(obj.contains_key("source"), "JSON must have 'source'");
        assert!(
            obj.contains_key("persisted_user_home"),
            "JSON must have 'persisted_user_home'"
        );
        assert_eq!(obj["effective_home"], "/home/transist");
        assert_eq!(obj["source"], "user_config");
        assert_eq!(obj["persisted_user_home"], "/global/root");

        let resolved_none = ResolvedHome {
            effective_home: Path::new("/home/transist").to_path_buf(),
            source: HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let json_none = serde_json::to_value(&resolved_none).expect("serialize");
        assert!(
            !json_none
                .as_object()
                .unwrap()
                .contains_key("persisted_user_home"),
            "persisted_user_home should be omitted when None"
        );
    }

    #[test]
    fn per_user_default_home_resolution() {
        // Serializes env mutation across both cases to avoid cross-test races on
        // process-global environment variables.
        let _guard = env_lock().lock().expect("env lock");
        let prior_xdg = std::env::var_os("XDG_STATE_HOME");
        let prior_home = std::env::var_os("HOME");

        // SAFETY: `env_lock` is held, so no other thread races on the
        // process-global environment; the test restores prior values below.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", "/xdg-state");
            std::env::remove_var("HOME");
        }
        let with_xdg = resolution::per_user_default_home();

        // SAFETY: `env_lock` is held; this is the second mutation in the
        // same guarded test section, restored before the guard drops.
        unsafe {
            std::env::remove_var("XDG_STATE_HOME");
            std::env::set_var("HOME", "/home/user");
        }
        let without_xdg = resolution::per_user_default_home();

        // SAFETY: `env_lock` is held; the prior captured values are restored
        // exactly so the process environment returns to its original state.
        unsafe {
            match prior_xdg {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            match prior_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }

        assert_eq!(with_xdg, Some(PathBuf::from("/xdg-state/transist")));
        assert_eq!(
            without_xdg,
            Some(PathBuf::from("/home/user/.local/state/transist"))
        );
    }
}
