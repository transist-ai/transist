// SPDX-License-Identifier: MPL-2.0
//! Independent wall-clock bounds for adapter integration timing assertions.

use std::time::{SystemTime, UNIX_EPOCH};
use transist_protocol::InvokeTiming;

/// Samples test wall time without using adapter timing code.
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test wall time after Unix epoch")
        .as_millis() as u64
}

/// Checks parseable invocation timestamps against independently sampled bounds.
/// Delayed fixtures additionally require distinct timestamps and positive work.
pub fn assert_timing(timing: &InvokeTiming, before: u64, after: u64, delayed: bool) {
    let start = timing.started_at.parse::<u64>().expect("Unix-ms start");
    let finish = timing.finished_at.parse::<u64>().expect("Unix-ms finish");
    assert!(
        (before..=after).contains(&start),
        "start {start} outside {before}..={after}"
    );
    assert!(
        (before..=after).contains(&finish),
        "finish {finish} outside {before}..={after}"
    );
    if delayed {
        assert!(
            finish > start,
            "delayed fixture must have distinct wall samples: {timing:?}"
        );
        assert!(
            timing.duration_ms > 0,
            "delayed fixture must have positive monotonic duration"
        );
    }
}
