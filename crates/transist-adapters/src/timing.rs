// SPDX-License-Identifier: MPL-2.0
//! Timestamp and duration helpers for invocation timing.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use transist_protocol::InvokeTiming;

/// Captures invocation wall time independently from monotonic elapsed time.
pub(crate) struct InvocationTimer {
    started_at_ms: u64,
    started: Instant,
}

impl InvocationTimer {
    /// Samples both clocks once at adapter entry without changing any deadline.
    pub(crate) fn new() -> Self {
        Self {
            started_at_ms: unix_ms(),
            started: Instant::now(),
        }
    }

    /// Samples completion clocks and returns timing for one terminal outcome.
    pub(crate) fn snapshot(&self) -> InvokeTiming {
        assemble_timing(self.started_at_ms, unix_ms(), self.started.elapsed())
    }
}

/// Preserves wall-clock samples, including regression, independently of elapsed time.
fn assemble_timing(started_at_ms: u64, finished_at_ms: u64, elapsed: Duration) -> InvokeTiming {
    InvokeTiming {
        started_at: started_at_ms.to_string(),
        finished_at: finished_at_ms.to_string(),
        duration_ms: elapsed.as_millis() as u64,
    }
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_wall_samples_and_monotonic_elapsed_are_independent() {
        let timing = assemble_timing(1_000, 1_250, Duration::from_millis(200));
        assert_eq!(timing.started_at, "1000");
        assert_eq!(timing.finished_at, "1250");
        assert_eq!(timing.duration_ms, 200);
    }

    #[test]
    fn zero_elapsed_and_equal_wall_samples_are_valid() {
        let timing = assemble_timing(1_000, 1_000, Duration::ZERO);
        assert_eq!(timing.started_at, "1000");
        assert_eq!(timing.finished_at, "1000");
        assert_eq!(timing.duration_ms, 0);
    }

    #[test]
    fn backward_wall_sample_is_preserved_with_positive_elapsed() {
        let timing = assemble_timing(1_000, 900, Duration::from_millis(200));
        assert_eq!(timing.started_at, "1000");
        assert_eq!(timing.finished_at, "900");
        assert_eq!(timing.duration_ms, 200);
    }
}
