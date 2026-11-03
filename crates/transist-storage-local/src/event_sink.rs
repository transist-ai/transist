// SPDX-License-Identifier: MPL-2.0
use crate::atomic_io::append_jsonl_durable;
use crate::run_paths::resolve_run_root;
use crate::run_store::LocalStorageConfig;
use crate::util::{EVENTS_JSONL, repair_stream_and_note};
use anyhow::{Context, Result};
use transist_core::ports::EventPort;
use transist_protocol::RuntimeEvent;

/// Append-only local runtime event sink.
#[derive(Debug)]
pub struct LocalEventSink {
    config: LocalStorageConfig,
}

impl LocalEventSink {
    /// Creates a local event sink.
    pub fn new(config: LocalStorageConfig) -> Self {
        Self { config }
    }
}

impl EventPort for LocalEventSink {
    fn publish(&self, event: &RuntimeEvent) -> Result<()> {
        let path = resolve_run_root(&self.config.root, &event.run_id)?.join(EVENTS_JSONL);
        repair_stream_and_note(&path, &path, &event.run_id, "events")
            .with_context(|| format!("failed repairing event stream: {}", path.display()))?;
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        append_jsonl_durable(&path, &line)
            .with_context(|| format!("failed appending event: {}", path.display()))?;
        Ok(())
    }
}
