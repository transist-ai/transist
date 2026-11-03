// SPDX-License-Identifier: MPL-2.0
use crate::flow::FlowStep;
use anyhow::Result;
use std::path::Path;
use transist_protocol::{RecipePromptBinding, RecipePromptSource};

pub(crate) fn compile_prompt_binding(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<Option<RecipePromptBinding>> {
    if step.prompt.is_none() && step.prompt_file.is_none() {
        return Ok(None);
    }

    let (_, _, prompt_binding) = crate::validate::resolve_prompt_action(home_dir, step, step_idx)?;
    let source = if let Some(prompt) = step.prompt.as_ref() {
        RecipePromptSource::Inline {
            value: prompt.clone(),
        }
    } else if let Some(prompt_file) = step.prompt_file.as_ref() {
        RecipePromptSource::File {
            path: prompt_file.clone(),
        }
    } else {
        return Ok(None);
    };

    Ok(Some(RecipePromptBinding {
        inline_field: prompt_binding.inline_field,
        file_field: prompt_binding.file_field,
        source,
    }))
}
