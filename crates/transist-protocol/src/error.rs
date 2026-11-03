// SPDX-License-Identifier: MPL-2.0
//! Error types for `transist-protocol`.

use thiserror::Error;

/// Public error type returned by `transist-protocol` APIs.
///
/// Variants identify the specific failure mode where it adds value to callers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The encoded/decoded JSONL frame was empty.
    #[error("jsonl line is empty")]
    EmptyJsonlLine,
    /// Failed to encode/decode JSON.
    #[error("json codec error: {0}")]
    Json(#[from] serde_json::Error),
    /// Protocol-level validation or processing failure.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Convenience `Result` alias used throughout `transist-protocol`.
pub type Result<T, E = Error> = std::result::Result<T, E>;
