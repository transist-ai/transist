// SPDX-License-Identifier: MPL-2.0
use crate::WorkflowDefinitionSnapshot;
use crate::flow::{Flow, FlowStep};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use transist_protocol::{
    RecipeSkillBundle, RecipeSkillDocument, RecipeSkillRef, RecipeSkillRoot, RecipeSkillsBinding,
};

pub(crate) fn compile_skills_binding(
    home_dir: &Path,
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<Option<RecipeSkillsBinding>> {
    if step.skills.is_none() {
        return Ok(None);
    }

    let binding = crate::validate::resolve_skills_action(home_dir, step, step_idx)?;
    let bundle = build_skill_bundle(flow, step, step_idx, snapshot)?;
    let rendered = render_skill_bundle(&bundle);
    Ok(Some(RecipeSkillsBinding {
        bundle_field: binding.bundle_field,
        rendered_field: binding.rendered_field,
        bundle,
        rendered,
    }))
}

pub(crate) fn build_skill_bundle(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<RecipeSkillBundle> {
    let skills = step
        .skills
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("internal error: missing skills"))?;
    let source_path = flow.source_path.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "flow.steps[{step_idx}] '{}' requires a source-backed flow to resolve skills",
            step.id
        )
    })?;

    if let Some(snapshot) = snapshot {
        return build_snapshot_skill_bundle(snapshot, source_path, skills, step_idx);
    }

    let mut authored_refs = Vec::new();
    let mut roots = Vec::new();
    let mut seen_skill_files = BTreeSet::new();
    for (skill_idx, raw_ref) in skills.iter().enumerate() {
        let resolved = crate::io::resolve_relative_path(source_path, raw_ref.trim());
        let metadata = std::fs::metadata(&resolved).with_context(|| {
            format!(
                "flow.steps[{step_idx}].skills[{skill_idx}] '{}' does not exist",
                raw_ref
            )
        })?;
        if metadata.is_file() {
            if resolved.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                bail!(
                    "flow.steps[{step_idx}].skills[{skill_idx}] '{}' must reference SKILL.md when using a file path",
                    raw_ref
                );
            }
            let skill_file = std::fs::canonicalize(&resolved).with_context(|| {
                format!(
                    "failed resolving flow.steps[{step_idx}].skills[{skill_idx}] '{}'",
                    raw_ref
                )
            })?;
            authored_refs.push(RecipeSkillRef {
                raw: raw_ref.clone(),
                resolved_path: skill_file.display().to_string(),
                resolution_kind: "skill_file".to_string(),
            });
            if seen_skill_files.insert(skill_file.clone()) {
                roots.push(build_skill_root(&skill_file)?);
            }
            continue;
        }
        if !metadata.is_dir() {
            bail!(
                "flow.steps[{step_idx}].skills[{skill_idx}] '{}' must resolve to a file or directory",
                raw_ref
            );
        }

        let resolved_dir = std::fs::canonicalize(&resolved).with_context(|| {
            format!(
                "failed resolving flow.steps[{step_idx}].skills[{skill_idx}] '{}'",
                raw_ref
            )
        })?;
        let direct_skill_file = resolved_dir.join("SKILL.md");
        if direct_skill_file.exists() {
            let skill_file = std::fs::canonicalize(&direct_skill_file).with_context(|| {
                format!(
                    "failed resolving flow.steps[{step_idx}].skills[{skill_idx}] '{}'",
                    raw_ref
                )
            })?;
            authored_refs.push(RecipeSkillRef {
                raw: raw_ref.clone(),
                resolved_path: resolved_dir.display().to_string(),
                resolution_kind: "skill_root".to_string(),
            });
            if seen_skill_files.insert(skill_file.clone()) {
                roots.push(build_skill_root(&skill_file)?);
            }
            continue;
        }

        let discovered = discover_skill_files(&resolved_dir)?;
        if discovered.is_empty() {
            bail!(
                "flow.steps[{step_idx}].skills[{skill_idx}] '{}' resolves to no skills",
                raw_ref
            );
        }
        authored_refs.push(RecipeSkillRef {
            raw: raw_ref.clone(),
            resolved_path: resolved_dir.display().to_string(),
            resolution_kind: "collection".to_string(),
        });
        for skill_file in discovered {
            if seen_skill_files.insert(skill_file.clone()) {
                roots.push(build_skill_root(&skill_file)?);
            }
        }
    }

    Ok(RecipeSkillBundle {
        version: "v1".to_string(),
        authored_refs,
        roots,
    })
}

fn build_snapshot_skill_bundle(
    snapshot: &WorkflowDefinitionSnapshot,
    source_path: &Path,
    skills: &[String],
    step_idx: usize,
) -> Result<RecipeSkillBundle> {
    let mut authored_refs = Vec::new();
    let mut roots = Vec::new();
    let mut seen_skill_files = BTreeSet::new();
    for (skill_idx, raw_ref) in skills.iter().enumerate() {
        let relative = snapshot
            .resolve_from_source(source_path, raw_ref.trim())
            .with_context(|| {
                format!("invalid flow.steps[{step_idx}].skills[{skill_idx}] '{raw_ref}'")
            })?;
        let relative_path = Path::new(&relative);
        let (resolution_kind, resolved_relative, skill_files) = if relative_path
            .file_name()
            .and_then(|name| name.to_str())
            == Some("SKILL.md")
        {
            snapshot.asset(&relative)?;
            ("skill_file", relative.clone(), vec![relative.clone()])
        } else {
            let direct = format!("{relative}/SKILL.md");
            if snapshot.asset(&direct).is_ok() {
                ("skill_root", relative.clone(), vec![direct])
            } else {
                let prefix = format!("{relative}/");
                let skill_files = snapshot
                    .assets
                    .iter()
                    .filter(|asset| {
                        asset.relative_path.starts_with(&prefix)
                            && Path::new(&asset.relative_path)
                                .file_name()
                                .and_then(|name| name.to_str())
                                == Some("SKILL.md")
                    })
                    .map(|asset| asset.relative_path.clone())
                    .collect::<Vec<_>>();
                if skill_files.is_empty() {
                    bail!(
                        "flow.steps[{step_idx}].skills[{skill_idx}] '{}' resolves to no skills in workflow snapshot",
                        raw_ref
                    );
                }
                ("collection", relative.clone(), skill_files)
            }
        };
        authored_refs.push(RecipeSkillRef {
            raw: raw_ref.clone(),
            resolved_path: snapshot
                .workflow_root
                .join(&resolved_relative)
                .display()
                .to_string(),
            resolution_kind: resolution_kind.to_string(),
        });
        for skill_file in skill_files {
            if seen_skill_files.insert(skill_file.clone()) {
                roots.push(build_snapshot_skill_root(snapshot, &skill_file)?);
            }
        }
    }
    Ok(RecipeSkillBundle {
        version: "v1".to_string(),
        authored_refs,
        roots,
    })
}

fn build_snapshot_skill_root(
    snapshot: &WorkflowDefinitionSnapshot,
    skill_file: &str,
) -> Result<RecipeSkillRoot> {
    let skill_root = Path::new(skill_file)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("skill file '{skill_file}' has no parent"))?;
    let mut documents = Vec::new();
    let mut visited = BTreeSet::new();
    collect_snapshot_skill_document(
        snapshot,
        skill_file,
        skill_root,
        &mut visited,
        &mut documents,
    )?;
    let absolute_root = snapshot.workflow_root.join(skill_root);
    Ok(RecipeSkillRoot {
        skill_name: extract_skill_name(&documents, &absolute_root),
        skill_root: absolute_root.display().to_string(),
        skill_file: snapshot
            .workflow_root
            .join(skill_file)
            .display()
            .to_string(),
        documents,
    })
}

fn collect_snapshot_skill_document(
    snapshot: &WorkflowDefinitionSnapshot,
    relative_path: &str,
    skill_root: &Path,
    visited: &mut BTreeSet<String>,
    documents: &mut Vec<RecipeSkillDocument>,
) -> Result<()> {
    if !visited.insert(relative_path.to_string()) {
        return Ok(());
    }
    let asset = snapshot.asset(relative_path)?;
    let content = std::str::from_utf8(&asset.bytes)
        .with_context(|| format!("skill document '{relative_path}' is not UTF-8"))?
        .to_string();
    let relative = Path::new(relative_path);
    documents.push(RecipeSkillDocument {
        logical_path: relative
            .strip_prefix(skill_root)
            .expect("captured skill document stays in root")
            .to_string_lossy()
            .replace('\\', "/"),
        resolved_path: snapshot.workflow_root.join(relative).display().to_string(),
        content: content.clone(),
    });
    for link in extract_markdown_links(&content) {
        let link_path = Path::new(&link);
        let linked = relative
            .parent()
            .expect("skill document has parent")
            .join(link_path);
        if !linked.starts_with(skill_root) {
            bail!("skill markdown reference escapes its skill root");
        }
        collect_snapshot_skill_document(
            snapshot,
            &linked.to_string_lossy().replace('\\', "/"),
            skill_root,
            visited,
            documents,
        )?;
    }
    Ok(())
}

fn discover_skill_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed reading skills directory '{}'", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed reading skills directory '{}'", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed reading metadata for '{}'", path.display()))?;
        if metadata.is_dir() {
            let nested = path.join("SKILL.md");
            if nested.exists() {
                found.push(std::fs::canonicalize(&nested).with_context(|| {
                    format!("failed resolving discovered skill '{}'", nested.display())
                })?);
            }
            found.extend(discover_skill_files(&path)?);
        }
    }
    Ok(found)
}

fn build_skill_root(skill_file: &Path) -> Result<RecipeSkillRoot> {
    let skill_file = std::fs::canonicalize(skill_file)
        .with_context(|| format!("failed resolving skill file '{}'", skill_file.display()))?;
    let skill_root = skill_file
        .parent()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "skill file '{}' has no parent directory",
                skill_file.display()
            )
        })?
        .to_path_buf();
    let mut documents = Vec::new();
    let mut visited = BTreeSet::new();
    collect_skill_document(&skill_file, &skill_root, &mut visited, &mut documents)?;
    let skill_name = extract_skill_name(&documents, &skill_root);
    Ok(RecipeSkillRoot {
        skill_name,
        skill_root: skill_root.display().to_string(),
        skill_file: skill_file.display().to_string(),
        documents,
    })
}

fn collect_skill_document(
    path: &Path,
    skill_root: &Path,
    visited: &mut BTreeSet<PathBuf>,
    documents: &mut Vec<RecipeSkillDocument>,
) -> Result<()> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed resolving skill document '{}'", path.display()))?;
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }

    let content = std::fs::read_to_string(&canonical)
        .with_context(|| format!("failed reading skill document '{}'", canonical.display()))?;
    documents.push(RecipeSkillDocument {
        logical_path: display_skill_path(&canonical, skill_root),
        resolved_path: canonical.display().to_string(),
        content: content.clone(),
    });

    for link in extract_markdown_links(&content) {
        let target = resolve_markdown_link(&canonical, &link);
        let metadata = std::fs::metadata(&target).with_context(|| {
            format!(
                "skill document '{}' references missing markdown file '{}'",
                canonical.display(),
                target.display()
            )
        })?;
        if !metadata.is_file() {
            bail!(
                "skill document '{}' references non-file markdown path '{}'",
                canonical.display(),
                target.display()
            );
        }
        collect_skill_document(&target, skill_root, visited, documents)?;
    }
    Ok(())
}

fn extract_skill_name(documents: &[RecipeSkillDocument], skill_root: &Path) -> String {
    if let Some(root_doc) = documents.iter().find(|doc| doc.logical_path == "SKILL.md")
        && let Some(name) = parse_skill_name(&root_doc.content)
    {
        return name;
    }
    skill_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill")
        .to_string()
}

fn parse_skill_name(content: &str) -> Option<String> {
    let mut in_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            let parsed = value.trim();
            if !parsed.is_empty() {
                return Some(parsed.to_string());
            }
        }
    }
    None
}

fn display_skill_path(path: &Path, skill_root: &Path) -> String {
    match path.strip_prefix(skill_root) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => path.display().to_string(),
    }
}

pub(crate) fn extract_markdown_links(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let raw = after[..end].trim();
        if let Some(link) = normalize_markdown_link(raw) {
            links.push(link);
        }
        rest = &after[end + 1..];
    }
    links
}

fn normalize_markdown_link(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.starts_with('#')
        || raw.starts_with("http://")
        || raw.starts_with("https://")
    {
        return None;
    }
    let target = raw.split('#').next().unwrap_or_default().trim();
    if !target.ends_with(".md") {
        return None;
    }
    Some(target.to_string())
}

fn resolve_markdown_link(current_doc: &Path, link: &str) -> PathBuf {
    let link_path = PathBuf::from(link);
    if link_path.is_absolute() {
        link_path
    } else {
        current_doc
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link_path)
    }
}

fn render_skill_bundle(bundle: &RecipeSkillBundle) -> String {
    let mut rendered = String::from("# Workflow Skills\n");
    for root in &bundle.roots {
        rendered.push_str("\n## ");
        rendered.push_str(&root.skill_name);
        rendered.push('\n');
        for document in &root.documents {
            rendered.push_str("\n### ");
            rendered.push_str(&document.logical_path);
            rendered.push_str("\n\n");
            rendered.push_str(&document.content);
            if !document.content.ends_with('\n') {
                rendered.push('\n');
            }
        }
    }
    rendered
}
