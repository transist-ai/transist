// SPDX-License-Identifier: MPL-2.0
use std::fmt;
use std::path::PathBuf;

/// Structured validation failure raised when `prepare_workflow` cannot bind provided inputs
/// to the compiled recipe before a run is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInputBindingError {
    /// Workflow id from the compiled recipe that rejected the provided inputs.
    pub workflow_id: String,
    /// Workflow version from the compiled recipe that rejected the provided inputs.
    pub workflow_version: String,
    /// Absolute path to the compiled recipe that was used for binding.
    pub recipe_path: PathBuf,
    /// Declared workflow inputs from the compiled recipe, in stable sort order.
    pub declared_inputs: Vec<String>,
    /// Specific validation failure kind.
    pub kind: WorkflowInputBindingErrorKind,
}

impl WorkflowInputBindingError {
    /// Returns the workflow reference in `<id>@<version>` form.
    pub fn workflow_ref(&self) -> String {
        format!("{}@{}", self.workflow_id, self.workflow_version)
    }
}

impl fmt::Display for WorkflowInputBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WorkflowInputBindingErrorKind::UndeclaredInput { input_name } => write!(
                f,
                "workflow input '{}' is not declared by '{}'",
                input_name,
                self.workflow_ref()
            ),
            WorkflowInputBindingErrorKind::MissingRequiredInput { input_name } => {
                write!(f, "missing required workflow input '{}'", input_name)
            }
            WorkflowInputBindingErrorKind::TypeMismatch {
                input_name,
                expected_type,
                received_type,
            } => write!(
                f,
                "workflow input '{}' expected type '{}' but received '{}'",
                input_name, expected_type, received_type
            ),
            WorkflowInputBindingErrorKind::UnsupportedDeclaredType {
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

impl std::error::Error for WorkflowInputBindingError {}

/// Structured uninstall failure raised when a compiled recipe is still referenced by active runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowUninstallError {
    /// The recipe is still referenced by one or more non-terminal runs.
    InUse {
        /// Workflow id for the recipe that cannot be removed yet.
        workflow_id: String,
        /// Workflow version for the recipe that cannot be removed yet.
        workflow_version: String,
        /// Non-terminal runs that still reference the workflow.
        run_ids: Vec<String>,
    },
}

impl WorkflowUninstallError {
    /// Returns the workflow reference in `<id>@<version>` form.
    pub fn workflow_ref(&self) -> String {
        match self {
            Self::InUse {
                workflow_id,
                workflow_version,
                ..
            } => format!("{}@{}", workflow_id, workflow_version),
        }
    }
}

impl fmt::Display for WorkflowUninstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InUse { run_ids, .. } => write!(
                f,
                "cannot uninstall compiled workflow '{}' while protected runs still reference it: {}",
                self.workflow_ref(),
                run_ids.join(", ")
            ),
        }
    }
}

impl std::error::Error for WorkflowUninstallError {}

/// Specific kind of structured workflow-input binding failure raised during `prepare_workflow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowInputBindingErrorKind {
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
