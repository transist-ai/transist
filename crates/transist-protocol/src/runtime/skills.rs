// SPDX-License-Identifier: MPL-2.0
//! Skill-related recipe types compiled from workflow `skills` directives.

use serde::{Deserialize, Serialize};

/// Rendered skills bundle and field mapping for one compiled step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeSkillsBinding {
    /// Input field that receives the structured bundle.
    pub bundle_field: String,
    /// Input field that receives the rendered skill text.
    pub rendered_field: String,
    /// Structured bundle describing the resolved skills.
    pub bundle: RecipeSkillBundle,
    /// Concatenated rendered skill content.
    pub rendered: String,
}

/// Structured description of the resolved skills for a compiled step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeSkillBundle {
    /// Bundle format version.
    pub version: String,
    /// Authored skill references before filesystem expansion.
    #[serde(default)]
    pub authored_refs: Vec<RecipeSkillRef>,
    /// Resolved roots and documents included in the bundle.
    #[serde(default)]
    pub roots: Vec<RecipeSkillRoot>,
}

/// One authored skill reference and how compilation resolved it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeSkillRef {
    /// Original authored reference string.
    pub raw: String,
    /// Resolved filesystem path used during compilation.
    pub resolved_path: String,
    /// Resolution strategy name used by the compiler.
    pub resolution_kind: String,
}

/// Resolved skill root included in a rendered bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeSkillRoot {
    /// User-facing skill name.
    pub skill_name: String,
    /// Root directory used for the skill.
    pub skill_root: String,
    /// Primary `SKILL.md` path.
    pub skill_file: String,
    /// Materialized documents included from the skill root.
    pub documents: Vec<RecipeSkillDocument>,
}

/// One resolved document included in a skills bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeSkillDocument {
    /// Logical path inside the skill bundle.
    pub logical_path: String,
    /// Concrete filesystem path read during compilation.
    pub resolved_path: String,
    /// File content embedded into the bundle.
    pub content: String,
}
