// SPDX-License-Identifier: MPL-2.0
//! Test-only execution-phase fault controls owned by the kernel.

use anyhow::{Result, bail};
use std::cell::Cell;

thread_local! {
    static INJECTED_POINT: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

/// Fault before publishing the step dispatch intent.
pub const FAULT_STEP_DISPATCH_INTENT: usize = 301;
/// Fault after publishing the step start event.
pub const FAULT_STEP_START_EVENT: usize = 302;
/// Fault after result promotion.
pub const FAULT_STEP_RESULT_PROMOTION: usize = 303;
/// Fault after persisting the immutable execution record.
pub const FAULT_STEP_EXECUTION_RECORD: usize = 304;
/// Fault after persisting the step projection.
pub const FAULT_STEP_STATE_PROJECTION: usize = 305;
/// Fault after appending a failure record.
pub const FAULT_STEP_FAILURE_APPEND: usize = 306;
/// Fault after publishing the completion event.
pub const FAULT_STEP_COMPLETION_EVENT: usize = 307;

/// Arms one execution-phase fault point.
pub fn set(point: usize) {
    set_occurrence(point, 1);
}

/// Arms one execution-phase fault point for a selected occurrence.
pub fn set_occurrence(point: usize, occurrence: usize) {
    INJECTED_POINT.with(|injected| injected.set((point, occurrence.max(1), 0)));
}

/// Clears the current execution fault point.
pub fn clear() {
    INJECTED_POINT.with(|injected| injected.set((0, 0, 0)));
}

/// Returns whether `point` is the selected occurrence of the armed fault.
pub fn matches(point: usize) -> bool {
    INJECTED_POINT.with(|injected| {
        let (armed, target, seen) = injected.get();
        if armed != point || target == 0 {
            return false;
        }
        let next_seen = seen.saturating_add(1);
        injected.set((armed, target, next_seen));
        next_seen == target
    })
}

/// Returns an injected fault error when the selected point is reached.
pub fn check_fault(point: usize) -> Result<()> {
    if matches(point) {
        bail!("injected fault at execution point {point}");
    }
    Ok(())
}
