// SPDX-License-Identifier: MPL-2.0
//! File-based debug logging for diagnosing TUI staleness/freeze bugs.
//!
//! Gated by env var `TRANSIST_TUI_DEBUG=1`. When enabled, appends to the
//! path in `TRANSIST_TUI_DEBUG_PATH` (default `/tmp/transist-tui-debug.log`).
//! When disabled, every call compiles down to a single env-var check and
//! short-circuits with no I/O.
//!
//! This is intentionally separate from any structured tracing facility:
//! we want zero-config plain-text capture across a single Approve scenario.
//!
//! Format: `<unix_ms> <site> <key=value> ...`
//!
//! Sites currently instrumented (see `screens.rs` and `run_detail.rs`):
//! * `action.outcome`         — mutating-action Stay/RefreshNow branch entered
//! * `loop.refresh_spawn`     — background refresh thread spawned
//! * `loop.refresh_skip`      — refresh spawn short-circuited by fingerprint
//! * `loop.refresh_recv`      — async refresh received and being applied
//! * `apply.enter`            — `apply_run_detail_loaded_data` entered
//! * `apply.fingerprint`      — old/new frame fingerprints + draw decision
//! * `show_loading.flip`      — modal visibility flipped (forces redraw)
//! * `poll.timeout`           — `event::poll` timed out (no key event)

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Cached result of inspecting `TRANSIST_TUI_DEBUG` once at startup.
/// Avoids per-call env lookups in the hot poll loop when debugging is off.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("TRANSIST_TUI_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn log_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::var("TRANSIST_TUI_DEBUG_PATH")
            .unwrap_or_else(|_| "/tmp/transist-tui-debug.log".to_string())
    })
}

/// Append a single debug line. Cheap no-op when disabled.
pub(super) fn log(site: &str, fields: &str) {
    if !enabled() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!("{ts} {site} {fields}\n");
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = f.write_all(line.as_bytes());
    }
}
