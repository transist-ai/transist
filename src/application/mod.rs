// SPDX-License-Identifier: MPL-2.0
//! Application-layer entry points.
//!
//! This module keeps the public use cases separated from the lower-level runtime
//! composition so callers can work with workflow operations and inspection queries
//! without knowing the underlying storage or kernel wiring.

/// Read models and query ports for inspecting persisted runs, steps, events, and workflows.
pub use transist_inspection as inspection;
/// Serializable inspection snapshot envelope builder for outbound sync use cases.
pub mod inspection_snapshot;
/// Read models and rebuild logic for durable and live metrics.
pub use transist_metrics as metrics;
/// Installed-plugin lifecycle operations.
pub mod plugins;
mod run_protection;
/// Run lifecycle operations and execution helpers.
pub mod runs;
/// Runtime trust-root import and listing helpers.
pub mod trust;
/// Workflow validation, compilation, and start helpers.
pub mod workflows;
