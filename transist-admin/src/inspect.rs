// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use std::path::Path;

use crate::signature::{SignatureInspectView, inspect_view, load_signature_envelope};

/// Loads and inspects a signature envelope, returning a human-readable view.
pub fn inspect_signature(path: &Path) -> Result<SignatureInspectView> {
    Ok(inspect_view(&load_signature_envelope(path)?))
}
