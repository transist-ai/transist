// SPDX-License-Identifier: MPL-2.0
//! Projects contained-run summaries into the inspection UI's execution outline.

use std::collections::{HashMap, HashSet};

use super::{LinkRef, helpers::url_encode};
use crate::domain::api_models::ContainedRunSummary;

pub(super) struct ChildOutline {
    pub(super) items: Vec<OutlineItem>,
    pub(super) unplaced: Vec<ChildRunRow>,
}

// Emit matching open/close items so the template can render a nested list
// without recursive template calls.
pub(super) enum OutlineItem {
    Run(ChildRunRow, usize),
    CloseBranch,
}

impl OutlineItem {
    pub(super) fn row(&self) -> Option<&ChildRunRow> {
        match self {
            Self::Run(row, _) => Some(row),
            Self::CloseBranch => None,
        }
    }

    pub(super) fn child_count(&self) -> usize {
        match self {
            Self::Run(_, count) => *count,
            Self::CloseBranch => 0,
        }
    }
}

pub(super) struct ChildRunRow {
    pub(super) run_id: String,
    pub(super) run_link: LinkRef,
    pub(super) status: String,
    pub(super) current_step: Option<LinkRef>,
    pub(super) artifact_count: usize,
    pub(super) evidence_count: usize,
}

impl ChildOutline {
    /// Builds the subtree of `run_id`, using recorded parent IDs and keeping
    /// the inspection order among siblings. Disconnected runs remain linkable.
    pub(super) fn new(run_id: &str, contained_runs: &[ContainedRunSummary]) -> Self {
        enum Pending<'a> {
            Run(&'a ContainedRunSummary),
            CloseBranch,
        }

        let mut by_parent = HashMap::<_, Vec<_>>::new();
        let mut remaining = HashSet::new();
        for run in contained_runs {
            if run.run_id == run_id || !remaining.insert(run.run_id.as_str()) {
                continue;
            }
            if let Some(parent) = run.parent_run_id.as_deref() {
                by_parent.entry(parent).or_default().push(run);
            }
        }

        let mut pending: Vec<_> = by_parent
            .remove(run_id)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(Pending::Run)
            .collect();
        let mut items = Vec::new();
        while let Some(item) = pending.pop() {
            match item {
                Pending::Run(run) => {
                    remaining.remove(run.run_id.as_str());
                    let children = by_parent.remove(run.run_id.as_str()).unwrap_or_default();
                    items.push(OutlineItem::Run(ChildRunRow::new(run), children.len()));
                    if !children.is_empty() {
                        pending.push(Pending::CloseBranch);
                        pending.extend(children.into_iter().rev().map(Pending::Run));
                    }
                }
                Pending::CloseBranch => items.push(OutlineItem::CloseBranch),
            }
        }

        let unplaced = contained_runs
            .iter()
            .filter(|run| remaining.remove(run.run_id.as_str()))
            .map(ChildRunRow::new)
            .collect();
        Self { items, unplaced }
    }
}

impl ChildRunRow {
    fn new(run: &ContainedRunSummary) -> Self {
        let run_url = format!("/inspect/runs/{}", url_encode(&run.run_id));
        Self {
            run_id: run.run_id.clone(),
            current_step: run.current_step_id.as_ref().map(|step| LinkRef {
                label: step.clone(),
                href: format!("{run_url}/steps/{}", url_encode(step)),
            }),
            run_link: LinkRef {
                label: format!("{}@{}", run.workflow_id, run.workflow_version),
                href: run_url,
            },
            status: run.status.clone(),
            artifact_count: run.artifact_count,
            evidence_count: run.evidence_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;

    #[derive(Template)]
    #[template(path = "child_outline.html")]
    struct OutlineTemplate<'a> {
        outline: &'a ChildOutline,
    }

    fn run(run_id: &str, parent: &str, workflow: &str) -> ContainedRunSummary {
        ContainedRunSummary {
            run_id: run_id.into(),
            workflow_id: workflow.into(),
            workflow_version: "0.1.0".into(),
            status: "running".into(),
            created_at: String::new(),
            updated_at: String::new(),
            current_step_id: Some("draft".into()),
            artifact_count: 0,
            evidence_count: 0,
            parent_run_id: Some(parent.into()),
        }
    }

    #[test]
    fn renders_recorded_parentage_in_collapsible_branches() {
        let outline = ChildOutline::new(
            "root",
            &[
                run("unrelated-looking-id", "parent", "review"),
                run("parent", "root", "plan"),
                run("sibling", "root", "build"),
            ],
        );
        let html = OutlineTemplate { outline: &outline }.render().unwrap();
        let parent = html.find("/inspect/runs/parent\">plan@0.1.0").unwrap();
        let open = html.find("<details>").unwrap();
        let child = html
            .find("/inspect/runs/unrelated-looking-id\">review@0.1.0")
            .unwrap();
        let close = html.find("</details>").unwrap();
        let sibling = html.find("/inspect/runs/sibling\">build@0.1.0").unwrap();
        assert!(parent < open && open < child && child < close && close < sibling);
        assert!(html.contains("/inspect/runs/parent/steps/draft"));
        assert_eq!(html.matches("<details>").count(), 1);
    }

    #[test]
    fn renders_unplaced_runs_separately_and_preserves_their_links() {
        let outline = ChildOutline::new(
            "root",
            &[
                run("orphan", "missing", "review"),
                run("cycle-a", "cycle-b", "loop"),
                run("cycle-b", "cycle-a", "loop"),
            ],
        );
        let html = OutlineTemplate { outline: &outline }.render().unwrap();
        assert!(html.contains("Runs without a complete parent chain"));
        for id in ["orphan", "cycle-a", "cycle-b"] {
            let link = format!("href=\"/inspect/runs/{id}\"");
            assert_eq!(html.matches(link.as_str()).count(), 1);
        }
        assert!(!html.contains("<details>"));
    }
}
