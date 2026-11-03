// SPDX-License-Identifier: MPL-2.0
//! Shared output selection for API detail and server-rendered inspection.

use super::ApiProblem;
use crate::domain::api_models::StepOutput;
use transist::InspectionStep;

/// Selects latest output or an explicitly requested positive execution ordinal.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputExecution {
    /// Use the inspection layer's authoritative latest-output projection.
    #[default]
    Latest,
    /// Select an immutable execution record or the matching latest legacy result.
    Ordinal(u32),
}

/// Maps inspection output without altering persisted data or compact summaries.
pub(crate) fn select_output(
    step: &InspectionStep,
    selection: OutputExecution,
) -> Result<(StepOutput, Vec<u32>), ApiProblem> {
    let latest = step.latest_output.as_ref();
    let mut ordinals = step
        .executions
        .iter()
        .map(|record| record.execution)
        .filter(|ordinal| *ordinal > 0)
        .collect::<Vec<_>>();
    if let Some(snapshot) = latest.filter(|snapshot| snapshot.result_present)
        && let Some(ordinal) = snapshot.execution.filter(|ordinal| *ordinal > 0)
    {
        ordinals.push(ordinal);
    }
    ordinals.sort_unstable();
    ordinals.dedup();

    if let OutputExecution::Ordinal(ordinal) = selection {
        if ordinal > 0
            && let Some(record) = step
                .executions
                .iter()
                .find(|record| record.execution == ordinal)
        {
            return Ok((
                StepOutput {
                    execution: Some(ordinal),
                    status: record.status.clone(),
                    source: "execution_record".to_string(),
                    result_present: record.result.is_some(),
                    value: record.result.as_ref().map(|result| result.output.clone()),
                },
                ordinals,
            ));
        }
        if !latest.is_some_and(|snapshot| {
            ordinal > 0 && snapshot.execution == Some(ordinal) && snapshot.result_present
        }) {
            return Err(ApiProblem::resource_not_found(
                "selected execution was not found",
            ));
        }
    }

    let output = match latest {
        Some(snapshot) => StepOutput {
            execution: snapshot.execution,
            status: snapshot.status.clone(),
            source: snapshot.source.clone(),
            result_present: snapshot.result_present,
            value: snapshot.value.clone(),
        },
        None => StepOutput {
            execution: None,
            status: step.status.clone(),
            source: "state_snapshot".to_string(),
            result_present: false,
            value: None,
        },
    };
    Ok((output, ordinals))
}
