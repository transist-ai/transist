// SPDX-License-Identifier: MPL-2.0
use crate::util::now_unix_ms;
use transist_core::ports::ClockPort;

/// Clock implementation backed by the local system time.
#[derive(Debug, Default)]
pub struct LocalClock;

impl ClockPort for LocalClock {
    fn now_unix_ms(&self) -> u64 {
        now_unix_ms()
    }
}
