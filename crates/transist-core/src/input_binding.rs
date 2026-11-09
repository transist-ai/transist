// SPDX-License-Identifier: MPL-2.0
//! Shared recipe-input binding for root and embedded subflow runs.

use std::collections::BTreeMap;
use std::fmt;

use transist_protocol::{KernelValue, Recipe};

/// Structured validation failure raised when runtime input binding fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeInputBindingError {
    /// Workflow id from the compiled recipe that rejected the provided inputs.
    pub workflow_id: String,
    /// Workflow version from the compiled recipe that rejected the provided inputs.
    pub workflow_version: String,
    /// Declared workflow inputs from the compiled recipe, in stable sort order.
    pub declared_inputs: Vec<String>,
    /// Specific validation failure kind.
    pub kind: RecipeInputBindingErrorKind,
}

impl RecipeInputBindingError {
    /// Returns the workflow reference in `<id>@<version>` form.
    pub fn workflow_ref(&self) -> String {
        format!("{}@{}", self.workflow_id, self.workflow_version)
    }
}

impl fmt::Display for RecipeInputBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            RecipeInputBindingErrorKind::UndeclaredInput { input_name } => write!(
                f,
                "workflow input '{}' is not declared by '{}'",
                input_name,
                self.workflow_ref()
            ),
            RecipeInputBindingErrorKind::MissingRequiredInput { input_name } => {
                write!(f, "missing required workflow input '{}'", input_name)
            }
            RecipeInputBindingErrorKind::TypeMismatch {
                input_name,
                expected_type,
                received_type,
            } => write!(
                f,
                "workflow input '{}' expected type '{}' but received '{}'",
                input_name, expected_type, received_type
            ),
            RecipeInputBindingErrorKind::UnsupportedDeclaredType {
                input_name,
                expected_type,
            } => write!(
                f,
                "workflow input '{}' declares unsupported type '{}'",
                input_name, expected_type
            ),
        }
    }
}

impl std::error::Error for RecipeInputBindingError {}

/// Structured failure raised when preparing authority for an embedded child
/// workflow fails before the child can be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildAuthorityPreparationError {
    /// Parent subflow step that attempted to prepare the child.
    pub parent_step_id: String,
    /// Child workflow reference that could not be prepared.
    pub child_workflow_ref: String,
    /// Stable preparation diagnostic.
    pub message: String,
}

impl fmt::Display for ChildAuthorityPreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "child authority preparation failed for '{}' at parent step '{}': {}",
            self.child_workflow_ref, self.parent_step_id, self.message
        )
    }
}

impl std::error::Error for ChildAuthorityPreparationError {}

/// Specific kind of structured recipe-input binding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeInputBindingErrorKind {
    /// The caller supplied an input key that the compiled recipe does not declare.
    UndeclaredInput {
        /// Unknown input key supplied by the caller.
        input_name: String,
    },
    /// The caller omitted a required input and the recipe did not provide a default.
    MissingRequiredInput {
        /// Required input key missing from the merged inputs.
        input_name: String,
    },
    /// The caller supplied a value whose runtime type does not match the declared input type.
    TypeMismatch {
        /// Input key whose value failed type validation.
        input_name: String,
        /// Declared input type from the compiled recipe.
        expected_type: String,
        /// Actual runtime value type detected during binding.
        received_type: String,
    },
    /// The compiled recipe itself contains an unsupported declared type.
    UnsupportedDeclaredType {
        /// Input key whose declared type is unsupported.
        input_name: String,
        /// Unsupported type string from the compiled recipe.
        expected_type: String,
    },
}

/// Binds caller-provided values to a compiled recipe's declared inputs.
///
/// This applies declared defaults, enforces required-input presence, rejects
/// undeclared keys, and validates each bound value against the compiled recipe
/// input type.
pub fn bind_recipe_inputs(
    recipe: &Recipe,
    provided_inputs: BTreeMap<String, KernelValue>,
) -> std::result::Result<BTreeMap<String, KernelValue>, Box<RecipeInputBindingError>> {
    for input_name in provided_inputs.keys() {
        if !recipe.inputs.contains_key(input_name) {
            return Err(Box::new(recipe_input_binding_error(
                recipe,
                RecipeInputBindingErrorKind::UndeclaredInput {
                    input_name: input_name.clone(),
                },
            )));
        }
    }

    let mut bound = BTreeMap::new();
    for (input_name, spec) in &recipe.inputs {
        let value = if let Some(value) = provided_inputs.get(input_name) {
            value.clone()
        } else if let Some(default) = spec.default.clone() {
            default
        } else if spec.required {
            return Err(Box::new(recipe_input_binding_error(
                recipe,
                RecipeInputBindingErrorKind::MissingRequiredInput {
                    input_name: input_name.clone(),
                },
            )));
        } else {
            KernelValue::Null
        };
        ensure_input_type(recipe, input_name, &spec.value_type, &value)?;
        bound.insert(input_name.clone(), value);
    }
    Ok(bound)
}

fn recipe_input_binding_error(
    recipe: &Recipe,
    kind: RecipeInputBindingErrorKind,
) -> RecipeInputBindingError {
    RecipeInputBindingError {
        workflow_id: recipe.workflow.id.clone(),
        workflow_version: recipe.workflow.version.clone(),
        declared_inputs: recipe.inputs.keys().cloned().collect(),
        kind,
    }
}

fn ensure_input_type(
    recipe: &Recipe,
    name: &str,
    expected_type: &str,
    value: &KernelValue,
) -> std::result::Result<(), Box<RecipeInputBindingError>> {
    let matches = match expected_type {
        "string" => matches!(value, KernelValue::String(_) | KernelValue::Null),
        "number" => matches!(value, KernelValue::Number(_) | KernelValue::Null),
        "boolean" => matches!(value, KernelValue::Bool(_) | KernelValue::Null),
        "object" => matches!(value, KernelValue::Object(_) | KernelValue::Null),
        "array" => matches!(value, KernelValue::Array(_) | KernelValue::Null),
        other => {
            return Err(Box::new(recipe_input_binding_error(
                recipe,
                RecipeInputBindingErrorKind::UnsupportedDeclaredType {
                    input_name: name.to_string(),
                    expected_type: other.to_string(),
                },
            )));
        }
    };
    if matches {
        Ok(())
    } else {
        Err(Box::new(recipe_input_binding_error(
            recipe,
            RecipeInputBindingErrorKind::TypeMismatch {
                input_name: name.to_string(),
                expected_type: expected_type.to_string(),
                received_type: kernel_value_type_name(value).to_string(),
            },
        )))
    }
}

fn kernel_value_type_name(value: &KernelValue) -> &'static str {
    match value {
        KernelValue::Null => "null",
        KernelValue::Bool(_) => "boolean",
        KernelValue::Number(_) => "number",
        KernelValue::String(_) => "string",
        KernelValue::Array(_) => "array",
        KernelValue::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{RecipeGraph, WorkflowRef};

    fn recipe_with_inputs(
        workflow_id: &str,
        inputs: BTreeMap<String, transist_protocol::RecipeInputSpec>,
    ) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: workflow_id.to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs,
            steps: Vec::new(),
            cleanup: Vec::new(),
            graph: Some(RecipeGraph {
                entry: "finish".to_string(),
                edges: Vec::new(),
                max_iterations: None,
                loop_routing_version: None,
            }),
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    #[test]
    fn bind_recipe_inputs_applies_defaults() {
        let recipe = recipe_with_inputs(
            "child-defaults",
            BTreeMap::from([
                (
                    "required_ticket".to_string(),
                    transist_protocol::RecipeInputSpec {
                        value_type: "string".to_string(),
                        required: true,
                        default: None,
                        description: None,
                    },
                ),
                (
                    "target_branch".to_string(),
                    transist_protocol::RecipeInputSpec {
                        value_type: "string".to_string(),
                        required: false,
                        default: Some(KernelValue::String("main".to_string())),
                        description: None,
                    },
                ),
            ]),
        );

        let bound = bind_recipe_inputs(
            &recipe,
            BTreeMap::from([(
                "required_ticket".to_string(),
                KernelValue::String("ISSUE-289".to_string()),
            )]),
        )
        .expect("bind inputs");

        assert_eq!(
            bound,
            BTreeMap::from([
                (
                    "required_ticket".to_string(),
                    KernelValue::String("ISSUE-289".to_string()),
                ),
                (
                    "target_branch".to_string(),
                    KernelValue::String("main".to_string()),
                ),
            ])
        );
    }

    #[test]
    fn bind_recipe_inputs_rejects_undeclared_inputs() {
        let recipe = recipe_with_inputs(
            "child-defaults",
            BTreeMap::from([(
                "required_ticket".to_string(),
                transist_protocol::RecipeInputSpec {
                    value_type: "string".to_string(),
                    required: true,
                    default: None,
                    description: None,
                },
            )]),
        );

        let error = bind_recipe_inputs(
            &recipe,
            BTreeMap::from([(
                "unexpected".to_string(),
                KernelValue::String("boom".to_string()),
            )]),
        )
        .expect_err("undeclared input should fail");

        assert_eq!(
            error.kind,
            RecipeInputBindingErrorKind::UndeclaredInput {
                input_name: "unexpected".to_string(),
            }
        );
        assert_eq!(error.declared_inputs, vec!["required_ticket".to_string()]);
    }
}
