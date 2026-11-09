// SPDX-License-Identifier: MPL-2.0
use super::service::ApiProblem;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use transist::BlobFamily;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResourceLocatorPayload {
    family: String,
    step_id: String,
    execution: u32,
    relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLocator {
    pub step_id: String,
    pub execution: u32,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowFileLocator {
    pub key: String,
}

pub(crate) fn parse_artifact_id(raw: &str) -> Result<ResourceLocator, ApiProblem> {
    parse_resource_id(raw, BlobFamily::Artifact, "artifact")
}

pub(crate) fn parse_evidence_id(raw: &str) -> Result<ResourceLocator, ApiProblem> {
    parse_resource_id(raw, BlobFamily::Evidence, "evidence")
}

pub(crate) fn parse_workflow_file_id(raw: &str) -> Result<WorkflowFileLocator, ApiProblem> {
    let key = raw
        .strip_prefix("wf_")
        .filter(|key| {
            key.len() == 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or_else(|| ApiProblem::invalid_request("invalid workflow file id".to_string()))?;
    Ok(WorkflowFileLocator {
        key: key.to_string(),
    })
}

fn parse_resource_id(
    raw: &str,
    family: BlobFamily,
    resource_name: &'static str,
) -> Result<ResourceLocator, ApiProblem> {
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ApiProblem::invalid_request(format!("invalid {resource_name} id")))?;
    let payload: ResourceLocatorPayload = serde_json::from_slice(&decoded)
        .map_err(|_| ApiProblem::invalid_request(format!("invalid {resource_name} id")))?;
    if payload.family != family.dir_name() {
        return Err(ApiProblem::invalid_request(format!(
            "{resource_name} id family mismatch"
        )));
    }
    Ok(ResourceLocator {
        step_id: payload.step_id,
        execution: payload.execution,
        relative_path: payload.relative_path,
    })
}

pub(crate) fn encode_artifact_id(step_id: &str, execution: u32, relative_path: &str) -> String {
    encode_resource_id(BlobFamily::Artifact, step_id, execution, relative_path)
}

pub(crate) fn encode_evidence_id(step_id: &str, execution: u32, relative_path: &str) -> String {
    encode_resource_id(BlobFamily::Evidence, step_id, execution, relative_path)
}

pub(crate) fn encode_workflow_file_id(
    locator_kind: transist::InspectionWorkflowFileLocatorKind,
    step_id: &str,
    field_name: &str,
    relative_path: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"transist.workflow-file.v1\0");
    digest.update(match locator_kind {
        transist::InspectionWorkflowFileLocatorKind::Workspace => b"workspace".as_slice(),
        transist::InspectionWorkflowFileLocatorKind::RepositoryState => {
            b"repository_state".as_slice()
        }
        transist::InspectionWorkflowFileLocatorKind::Run => b"run".as_slice(),
    });
    digest.update([0]);
    digest.update(step_id.as_bytes());
    digest.update([0]);
    digest.update(field_name.as_bytes());
    digest.update([0]);
    digest.update(relative_path.as_bytes());
    let hex: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("wf_{hex}")
}

fn encode_resource_id(
    family: BlobFamily,
    step_id: &str,
    execution: u32,
    relative_path: &str,
) -> String {
    let payload = ResourceLocatorPayload {
        family: family.dir_name().to_string(),
        step_id: step_id.to_string(),
        execution,
        relative_path: relative_path.to_string(),
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("resource locator json"))
}

pub(crate) fn resource_not_found_for_family(family: BlobFamily, run_id: &str) -> ApiProblem {
    ApiProblem::resource_not_found(format!(
        "{} was not found for run '{run_id}'",
        family_resource_name(family)
    ))
}

fn family_resource_name(family: BlobFamily) -> &'static str {
    match family {
        BlobFamily::Artifact => "artifact",
        BlobFamily::Evidence => "evidence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_file_id_is_opaque() {
        let locator = "reports/nested.md";
        let id = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::Workspace,
            "step",
            "output_file",
            locator,
        );

        assert!(id.starts_with("wf_"));
        assert_eq!(id.len(), 67);
        assert!(!id.contains(locator));
        assert_eq!(parse_workflow_file_id(&id).expect("locator").key, &id[3..]);
    }

    #[test]
    fn workflow_file_id_includes_field_identity() {
        let locator = "report.md";
        let output_id = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::Workspace,
            "step",
            "output_file",
            locator,
        );
        let report_id = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::Workspace,
            "step",
            "report_file",
            locator,
        );

        assert_ne!(output_id, report_id);
        assert!(parse_workflow_file_id(&output_id).is_ok());
        assert!(parse_workflow_file_id(&output_id.to_ascii_uppercase()).is_err());
    }

    #[test]
    fn workflow_file_id_includes_locator_kind() {
        let workspace = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::Workspace,
            "step",
            "file",
            "report.md",
        );
        let run = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::Run,
            "step",
            "file",
            "report.md",
        );
        let repository_state = encode_workflow_file_id(
            transist::InspectionWorkflowFileLocatorKind::RepositoryState,
            "step",
            "file",
            "report.md",
        );
        assert_ne!(workspace, run);
        assert_ne!(workspace, repository_state);
        assert_ne!(run, repository_state);
    }
}
