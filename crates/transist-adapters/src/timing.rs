// SPDX-License-Identifier: MPL-2.0
//! Timestamp and duration helpers for invocation timing.

use std::time::{SystemTime, UNIX_EPOCH};
use transist_protocol::InvokeTiming;

pub(crate) fn timing_now(duration_ms: u64) -> InvokeTiming {
    let now = unix_ms().to_string();
    InvokeTiming {
        started_at: now.clone(),
        finished_at: now,
        duration_ms,
    }
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
