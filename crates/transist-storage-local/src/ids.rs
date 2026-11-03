// SPDX-License-Identifier: MPL-2.0
use transist_core::ports::IdGeneratorPort;

/// UUID-backed identifier generator for local runtime use.
#[derive(Debug, Default)]
pub struct LocalIdGenerator;

impl IdGeneratorPort for LocalIdGenerator {
    fn generate_run_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn generate_step_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn generate_artifact_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}
