// SPDX-License-Identifier: MPL-2.0
use crate::util::{now_unix_ms, write_json_create_new};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::ports::{
    ControlRequestPort, ControlRequestRecord, ControlRequestType, ControlRequesterMetadata,
    RuntimeOwnerIdentity,
};
use uuid::Uuid;

const CANCEL_REQUEST_FILE: &str = "cancel-request.json";

/// Filesystem-backed durable control-request store.
#[derive(Debug, Clone)]
pub struct LocalControlRequestStore {
    root: PathBuf,
    owner: Option<RuntimeOwnerIdentity>,
}

impl LocalControlRequestStore {
    /// Creates a control-request store rooted at the given Transist home directory.
    pub fn new(root: impl Into<PathBuf>, owner: Option<RuntimeOwnerIdentity>) -> Self {
        Self {
            root: root.into(),
            owner,
        }
    }

    fn cancel_request_path(&self, root_run_id: &str) -> PathBuf {
        self.root
            .join("control")
            .join("runs")
            .join(root_run_id)
            .join(CANCEL_REQUEST_FILE)
    }
}

impl ControlRequestPort for LocalControlRequestStore {
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord> {
        let path = self.cancel_request_path(root_run_id);
        if path.exists() {
            return load_control_request(&path);
        }
        let requester = self.owner.as_ref().map(|owner| ControlRequesterMetadata {
            owner_id: owner.owner_id.clone(),
            pid: owner.pid,
            hostname: owner.hostname.clone(),
            command: owner.command.clone(),
            external: owner.external.clone(),
        });
        let record = ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: Uuid::new_v4().to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: now_unix_ms(),
            requester,
        };
        match write_json_create_new(&path, &record) {
            Ok(()) => Ok(record),
            Err(error) if path.exists() => load_control_request(&path).or(Err(error)),
            Err(error) => Err(error),
        }
    }

    fn inspect_cancel_request(&self, root_run_id: &str) -> Result<Option<ControlRequestRecord>> {
        let path = self.cancel_request_path(root_run_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(load_control_request(&path)?))
    }

    fn clear_cancel_request(&self, root_run_id: &str) -> Result<()> {
        let path = self.cancel_request_path(root_run_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed clearing control request: {}", path.display())),
        }
    }
}

fn load_control_request(path: &Path) -> Result<ControlRequestRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading control request: {}", path.display()))?;
    let record: ControlRequestRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid control request json: {}", path.display()))?;
    if record.schema_version != ControlRequestRecord::SCHEMA_VERSION {
        anyhow::bail!(
            "control request '{}' uses unsupported schema '{}'",
            path.display(),
            record.schema_version
        );
    }
    Ok(record)
}
