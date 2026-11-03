// SPDX-License-Identifier: MPL-2.0
//! Opt-in, data-minimal measurement hooks for preflight benchmarks.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use transist_protocol::AdapterKind;

/// A readiness or run-birth path measured by the opt-in observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreflightMeasurementScope {
    /// The application-level, read-only workflow readiness pass.
    ApplicationReadiness,
    /// One kernel preflight recipe, identified only by recursive depth.
    KernelRecipe {
        /// Root is depth zero; each nested subflow increments the depth.
        depth: usize,
    },
    /// Fresh run creation before execution starts.
    RunBirth,
}

/// A fixed, non-sensitive preflight or run-birth phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreflightMeasurementPhase {
    /// Loading or materializing a recipe for the measured path.
    RecipeLoad,
    /// Binding workflow or subflow inputs.
    Binding,
    /// Resolving or checking trusted-execution state.
    Trust,
    /// Policy and authority evaluation before adapter probing.
    Policy,
    /// Adapter validation and live probes.
    Adapter,
    /// Kernel preflight performed before fresh run persistence.
    Preflight,
    /// Capturing detached workflow assets for run birth.
    AssetCapture,
    /// Persisting and publishing the complete run.
    DurablePersistence,
    /// End-to-end duration of the measured path.
    Total,
}

/// One safe measurement emitted while an observer is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightMeasurement {
    /// Elapsed time for one fixed phase.
    Phase {
        /// Measured execution path.
        scope: PreflightMeasurementScope,
        /// Measured phase.
        phase: PreflightMeasurementPhase,
        /// Elapsed time. This uses a monotonic clock and contains no wall-clock timestamp.
        elapsed: Duration,
    },
    /// Counts for one adapter preflight call.
    ProbeBatch {
        /// Readiness path that issued the adapter call.
        scope: PreflightMeasurementScope,
        /// Adapter selected for the batch.
        adapter_kind: AdapterKind,
        /// Number of live probe groups after static validation and grouping.
        probe_group_count: usize,
        /// Number of process spawns or remote probes performed by the batch.
        remote_probe_count: usize,
    },
}

struct MeasurementState {
    records: Arc<Mutex<Vec<PreflightMeasurement>>>,
    scope_stack: Vec<PreflightMeasurementScope>,
}

/// Data-minimal measurement context propagated to scoped preflight workers.
///
/// The context contains only the active fixed scope stack and a shared sink for
/// fixed measurement records. It cannot carry workflow data, paths, metadata,
/// secrets, or plugin output.
#[derive(Clone)]
#[doc(hidden)]
pub struct PreflightMeasurementContext {
    records: Arc<Mutex<Vec<PreflightMeasurement>>>,
    scope_stack: Vec<PreflightMeasurementScope>,
}

thread_local! {
    static ACTIVE_MEASUREMENT: RefCell<Option<MeasurementState>> = const { RefCell::new(None) };
}

struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        ACTIVE_MEASUREMENT.with(|active| {
            active.borrow_mut().take();
        });
    }
}

/// Runs an operation with opt-in preflight measurement enabled on the current thread.
///
/// The observer records only fixed enums, counts, and monotonic durations. Nested capture
/// sessions are rejected so measurements cannot leak across benchmark cases.
pub fn capture_preflight_measurements<T>(
    operation: impl FnOnce() -> T,
) -> (T, Vec<PreflightMeasurement>) {
    ACTIVE_MEASUREMENT.with(|active| {
        let mut active = active.borrow_mut();
        assert!(
            active.is_none(),
            "preflight measurement capture cannot be nested"
        );
        *active = Some(MeasurementState {
            records: Arc::new(Mutex::new(Vec::new())),
            scope_stack: Vec::new(),
        });
    });
    let guard = CaptureGuard;
    let output = operation();
    let records = ACTIVE_MEASUREMENT.with(|active| {
        let records = active
            .borrow_mut()
            .take()
            .expect("preflight measurement capture remained active")
            .records;
        records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    });
    std::mem::forget(guard);
    (output, records)
}

/// Drop guard used internally to finish one opt-in phase measurement.
#[doc(hidden)]
pub struct PreflightPhaseSpan {
    scope: PreflightMeasurementScope,
    phase: PreflightMeasurementPhase,
    started: Option<Instant>,
}

impl Drop for PreflightPhaseSpan {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        record(PreflightMeasurement::Phase {
            scope: self.scope,
            phase: self.phase,
            elapsed: started.elapsed(),
        });
    }
}

/// Starts one opt-in phase measurement and records it when the returned guard drops.
#[doc(hidden)]
pub fn phase_span(
    scope: PreflightMeasurementScope,
    phase: PreflightMeasurementPhase,
) -> PreflightPhaseSpan {
    let enabled = ACTIVE_MEASUREMENT.with(|active| active.borrow().is_some());
    PreflightPhaseSpan {
        scope,
        phase,
        started: enabled.then(Instant::now),
    }
}

/// Measures one fixed phase when a capture observer is active.
///
/// The operation's return value and error behavior are unchanged.
pub fn measure_preflight_phase<T>(
    scope: PreflightMeasurementScope,
    phase: PreflightMeasurementPhase,
    operation: impl FnOnce() -> T,
) -> T {
    let _span = phase_span(scope, phase);
    operation()
}

pub(crate) struct PreflightScopeGuard {
    active: bool,
}

impl Drop for PreflightScopeGuard {
    fn drop(&mut self) {
        if self.active {
            ACTIVE_MEASUREMENT.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    state.scope_stack.pop();
                }
            });
        }
    }
}

pub(crate) fn enter_scope(scope: PreflightMeasurementScope) -> PreflightScopeGuard {
    let active = ACTIVE_MEASUREMENT.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return false;
        };
        state.scope_stack.push(scope);
        true
    });
    PreflightScopeGuard { active }
}

/// Associates adapter probe counters with one fixed readiness scope while an observer is active.
pub fn with_preflight_measurement_scope<T>(
    scope: PreflightMeasurementScope,
    operation: impl FnOnce() -> T,
) -> T {
    let _guard = enter_scope(scope);
    operation()
}

/// Captures the current data-minimal observer context for a scoped worker.
#[doc(hidden)]
pub fn current_preflight_measurement_context() -> Option<PreflightMeasurementContext> {
    ACTIVE_MEASUREMENT.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|state| PreflightMeasurementContext {
                records: Arc::clone(&state.records),
                scope_stack: state.scope_stack.clone(),
            })
    })
}

/// Runs scoped worker work with a previously captured measurement context.
#[doc(hidden)]
pub fn with_preflight_measurement_context<T>(
    context: Option<PreflightMeasurementContext>,
    operation: impl FnOnce() -> T,
) -> T {
    let Some(context) = context else {
        return operation();
    };
    ACTIVE_MEASUREMENT.with(|active| {
        assert!(
            active.borrow().is_none(),
            "preflight worker measurement context cannot replace an active capture"
        );
        *active.borrow_mut() = Some(MeasurementState {
            records: context.records,
            scope_stack: context.scope_stack,
        });
    });
    let guard = CaptureGuard;
    let output = operation();
    drop(guard);
    output
}

/// Records a probe batch under the currently active readiness scope.
pub fn record_preflight_probe_batch(
    adapter_kind: AdapterKind,
    probe_group_count: usize,
    remote_probe_count: usize,
) {
    ACTIVE_MEASUREMENT.with(|active| {
        let mut active = active.borrow_mut();
        let Some(state) = active.as_mut() else {
            return;
        };
        let Some(scope) = state.scope_stack.last().copied() else {
            return;
        };
        state
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(PreflightMeasurement::ProbeBatch {
                scope,
                adapter_kind,
                probe_group_count,
                remote_probe_count,
            });
    });
}

/// Records a fixed phase duration when a capture observer is active.
pub fn record_preflight_phase_duration(
    scope: PreflightMeasurementScope,
    phase: PreflightMeasurementPhase,
    elapsed: Duration,
) {
    record(PreflightMeasurement::Phase {
        scope,
        phase,
        elapsed,
    });
}

fn record(measurement: PreflightMeasurement) {
    ACTIVE_MEASUREMENT.with(|active| {
        if let Some(state) = active.borrow_mut().as_mut() {
            state
                .records
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(measurement);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_records_only_safe_fixed_fields() {
        let (value, records) = capture_preflight_measurements(|| {
            with_preflight_measurement_scope(
                PreflightMeasurementScope::ApplicationReadiness,
                || record_preflight_probe_batch(AdapterKind::Process, 3, 3),
            );
            measure_preflight_phase(
                PreflightMeasurementScope::ApplicationReadiness,
                PreflightMeasurementPhase::Policy,
                || 42,
            )
        });

        assert_eq!(value, 42);
        assert_eq!(records.len(), 2);
        assert!(matches!(
            records[0],
            PreflightMeasurement::ProbeBatch {
                scope: PreflightMeasurementScope::ApplicationReadiness,
                adapter_kind: AdapterKind::Process,
                probe_group_count: 3,
                remote_probe_count: 3,
            }
        ));
        assert!(matches!(
            records[1],
            PreflightMeasurement::Phase {
                scope: PreflightMeasurementScope::ApplicationReadiness,
                phase: PreflightMeasurementPhase::Policy,
                ..
            }
        ));
    }

    #[test]
    fn disabled_observer_discards_measurements() {
        record_preflight_probe_batch(AdapterKind::Mcp, 1, 1);
        let span = phase_span(
            PreflightMeasurementScope::RunBirth,
            PreflightMeasurementPhase::Total,
        );
        assert!(span.started.is_none());
    }
}
