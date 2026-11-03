// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect, Size};
use std::ops::Range;
use std::path::Path;

use crate::InspectionSyncSummary;

use super::dashboard::{
    DashboardActionId, SetupView, UiStatusMessage, build_dashboard_setup_lines,
    load_inspection_sync_summary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigSection {
    Details,
    Actions,
}

#[derive(Debug, Clone)]
pub(super) struct ConfigActionItem {
    pub(super) action: DashboardActionId,
    pub(super) label: String,
    /// Optional section heading rendered above the first item of each group.
    /// Does not consume a navigation index.
    pub(super) section_heading: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ConfigScreenState {
    pub(super) header_lines: Vec<String>,
    pub(super) details_lines: Vec<String>,
    pub(super) details_error_line_range: Option<Range<usize>>,
    pub(super) action_items: Vec<ConfigActionItem>,
    pub(super) selected_action: usize,
    pub(super) focused_section: ConfigSection,
    pub(super) details_scroll: usize,
}

pub(super) fn restore_config_view_state(
    previous: &ConfigScreenState,
    next: &mut ConfigScreenState,
) {
    next.focused_section = previous.focused_section;
    next.details_scroll = previous.details_scroll;
    next.selected_action = previous
        .selected_action
        .min(next.action_items.len().saturating_sub(1));
}

pub(super) fn config_move_focus(focus: ConfigSection, forward: bool) -> ConfigSection {
    match (focus, forward) {
        (ConfigSection::Details, true) => ConfigSection::Actions,
        (ConfigSection::Actions, true) => ConfigSection::Details,
        (ConfigSection::Details, false) => ConfigSection::Actions,
        (ConfigSection::Actions, false) => ConfigSection::Details,
    }
}

pub(super) fn config_screen_metrics(size: Size) -> (usize, usize) {
    let area = Rect::from((Position::ORIGIN, size));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(layout[1]);
    (
        body[0].width.saturating_sub(2) as usize,
        body[0].height.saturating_sub(2) as usize,
    )
}

pub(super) fn build_configuration_screen_state(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    status_message: Option<&UiStatusMessage>,
) -> Result<ConfigScreenState> {
    let resolved = transist::resolve_settings_payload(repo_root, cwd, explicit_home)?;
    let context = transist::TransistContext::with_home(repo_root, resolved.effective_home.clone());
    let initialized = transist_home_is_initialized(context.home_dir());
    let settings_path = transist::user_settings_path()?;
    let inspection_available = crate::inspection_sync_available();
    let inspection_summary = if inspection_available {
        load_inspection_sync_summary(context.repo_root(), context.home_dir(), &resolved)
    } else {
        InspectionSyncSummary::unavailable()
    };
    let details = build_dashboard_setup_lines(
        &resolved,
        &settings_path,
        initialized,
        SetupView::Configuration,
        &inspection_summary,
        inspection_available,
        status_message,
    );
    Ok(ConfigScreenState {
        header_lines: vec![
            format!("Home: {}", resolved.effective_home.display()),
            format!("Source: {}", home_source_label(resolved.source)),
            format!("Initialized: {}", if initialized { "yes" } else { "no" }),
        ],
        details_lines: details.lines,
        details_error_line_range: details.error_line_range,
        action_items: config_action_items(),
        selected_action: 0,
        focused_section: ConfigSection::Actions,
        details_scroll: 0,
    })
}

fn config_action_items() -> Vec<ConfigActionItem> {
    vec![
        ConfigActionItem {
            action: DashboardActionId::SetPreferredPager,
            label: "Set/Unset preferred pager".to_string(),
            section_heading: Some("Preferences".to_string()),
        },
        ConfigActionItem {
            action: DashboardActionId::SetGateActor,
            label: "Set/Unset gate actor".to_string(),
            section_heading: None,
        },
        ConfigActionItem {
            action: DashboardActionId::SetPreferredEditor,
            label: "Set/Unset preferred editor".to_string(),
            section_heading: None,
        },
        ConfigActionItem {
            action: DashboardActionId::SetInspectionCacheRuns,
            label: "Set inspection cache size (runs)".to_string(),
            section_heading: None,
        },
    ]
}

pub(super) fn transist_home_is_initialized(home_dir: &Path) -> bool {
    // Older homes may legitimately be missing lazily-created directories like
    // `plugins/` or `registry/recipes/`. Treat the home as initialized when
    // the core config files and top-level runtime directories are present.
    home_dir.join("config.yaml").is_file()
        && home_dir.join("toolchains.yaml").is_file()
        && home_dir.join("registry").is_dir()
        && home_dir.join("runs").is_dir()
}

pub(super) fn home_source_label(source: transist::HomeSource) -> &'static str {
    match source {
        transist::HomeSource::CliOverride => "--home flag",
        transist::HomeSource::PerUserDefault => "per-user default",
        transist::HomeSource::EnvOverride => "TRANSIST_HOME",
        transist::HomeSource::UserConfig => "user setting",
    }
}

#[cfg(test)]
mod tests {
    use super::{config_action_items, home_source_label};

    #[test]
    fn config_actions_exclude_home_and_trust_mutations() {
        let items = config_action_items();
        assert!(
            items
                .iter()
                .any(|item| item.label.contains("preferred pager"))
        );
        assert!(items.iter().any(|item| item.label.contains("gate actor")));
        assert!(
            items
                .iter()
                .all(|item| !item.label.contains("home") && !item.label.contains("trust root"))
        );
    }

    #[test]
    fn config_actions_omit_inspection_when_sync_is_unavailable() {
        let items = config_action_items();
        let headings: Vec<&str> = items
            .iter()
            .filter_map(|item| item.section_heading.as_deref())
            .collect();
        assert_eq!(headings, vec!["Preferences"]);
    }

    #[test]
    fn config_actions_are_exactly_the_four_user_preferences() {
        let items = config_action_items();
        assert_eq!(items.len(), 4);
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Set/Unset preferred pager",
                "Set/Unset gate actor",
                "Set/Unset preferred editor",
                "Set inspection cache size (runs)",
            ]
        );
    }

    #[test]
    fn home_source_label_covers_all_variants() {
        assert_eq!(
            home_source_label(transist::HomeSource::CliOverride),
            "--home flag"
        );
        assert_eq!(
            home_source_label(transist::HomeSource::PerUserDefault),
            "per-user default"
        );
        assert_eq!(
            home_source_label(transist::HomeSource::EnvOverride),
            "TRANSIST_HOME"
        );
        assert_eq!(
            home_source_label(transist::HomeSource::UserConfig),
            "user setting"
        );
    }
}
