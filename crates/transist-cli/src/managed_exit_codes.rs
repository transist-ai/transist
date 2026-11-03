// SPDX-License-Identifier: MPL-2.0
use std::error::Error as StdError;
use std::fmt;

/// Raised when CLI arguments or runtime activation settings are invalid.
#[derive(Debug)]
pub struct UsageError {
    message: String,
    source: Option<anyhow::Error>,
}

impl UsageError {
    /// Creates a usage error with only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Creates a usage error and preserves the original source error.
    pub fn with_source(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl StdError for UsageError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

/// Raised when workflow/inputs validation fails before execution can proceed.
#[derive(Debug)]
pub struct WorkflowValidationError {
    source: anyhow::Error,
}

impl WorkflowValidationError {
    /// Wraps a validation source error.
    pub fn from_source(source: anyhow::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for WorkflowValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "workflow validation failed")
    }
}

impl StdError for WorkflowValidationError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Raised when a command was canceled or interrupted.
#[derive(Debug)]
pub struct CanceledOrInterruptedError {
    source: Option<anyhow::Error>,
}

impl CanceledOrInterruptedError {
    /// Creates a cancellation/interruption marker.
    pub fn from_source(source: anyhow::Error) -> Self {
        Self {
            source: Some(source),
        }
    }
}

impl fmt::Display for CanceledOrInterruptedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "operation canceled or interrupted")
    }
}

impl StdError for CanceledOrInterruptedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

/// Raised for unexpected runtime failures.
#[derive(Debug)]
pub struct InternalError {
    message: String,
    source: Option<anyhow::Error>,
}

impl InternalError {
    /// Creates an internal error and preserves the original source error.
    pub fn with_source(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl StdError for InternalError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

/// Wraps a source error as a usage error.
pub(crate) fn wrap_usage_error(message: impl Into<String>, source: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(UsageError::with_source(message, source))
}

/// Wraps a source error as a workflow validation error.
pub(crate) fn wrap_workflow_validation_error(source: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(WorkflowValidationError::from_source(source))
}

/// Wraps a source error as a cancellation/interruption error.
pub(crate) fn wrap_canceled_or_interrupted_error(source: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(CanceledOrInterruptedError::from_source(source))
}

/// Wraps a source error as an internal error.
pub(crate) fn wrap_internal_error(
    message: impl Into<String>,
    source: anyhow::Error,
) -> anyhow::Error {
    anyhow::Error::new(InternalError::with_source(message, source))
}

/// Returns whether the error chain contains a typed CLI error marker.
pub fn cli_error_chain_contains<T: StdError + Send + Sync + 'static>(
    error: &anyhow::Error,
) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<T>().is_some())
}

/// Returns whether the error chain contains an interrupted I/O error.
pub fn is_interrupted_io_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::Interrupted)
    })
}
