// SPDX-License-Identifier: MPL-2.0
//! Stable schema identifiers for persisted core runtime records.

/// Schema identifier used for persisted run state records.
pub const RUN_STATE_RECORD_SCHEMA_V1: &str = "transist.run_state.v1";

/// Schema identifier used for persisted run provenance records.
pub const RUN_PROVENANCE_RECORD_SCHEMA_V1: &str = "transist.run_provenance.v1";

/// Schema identifier used for the live progress snapshot records.
pub const LIVE_PROGRESS_RECORD_SCHEMA_V1: &str = "transist.live_progress.v1";
