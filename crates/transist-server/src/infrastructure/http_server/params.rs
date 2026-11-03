// SPDX-License-Identifier: MPL-2.0
use crate::application::ApiProblem;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub(crate) struct StepOutputParams {
    pub(crate) execution: Option<String>,
}

pub(crate) fn parse_output_execution(
    raw: Option<&str>,
) -> Result<crate::application::OutputExecution, ApiProblem> {
    use crate::application::OutputExecution;
    match raw {
        None | Some("latest") => Ok(OutputExecution::Latest),
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value
                .parse::<u32>()
                .ok()
                .filter(|ordinal| *ordinal > 0)
                .map(OutputExecution::Ordinal)
                .ok_or_else(|| {
                    ApiProblem::invalid_request("execution must be latest or a positive u32")
                })
        }
        Some(_) => Err(ApiProblem::invalid_request(
            "execution must be latest or a positive u32",
        )),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RunsQueryParams {
    pub(crate) limit: Option<String>,
    pub(crate) offset: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) workflow: Option<String>,
    pub(crate) created_after: Option<String>,
    pub(crate) created_before: Option<String>,
    pub(crate) updated_after: Option<String>,
    pub(crate) updated_before: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct EventsQueryParams {
    pub(crate) limit: Option<String>,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
    pub(crate) order: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PaginationParams {
    pub(crate) limit: Option<String>,
    pub(crate) offset: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BlobListQueryParams {
    pub(crate) scope: Option<String>,
}

pub(crate) fn ensure_valid_workflow_ref(workflow_ref: &str) -> Result<(), ApiProblem> {
    if transist::workflow_ref_parts(workflow_ref).is_none() {
        return Err(ApiProblem::invalid_request(
            "workflow path must use '<id>@<version>'",
        ));
    }
    Ok(())
}

pub(crate) fn parse_runs_query(
    params: RunsQueryParams,
) -> Result<transist::RunListQuery, ApiProblem> {
    let status = params
        .status
        .map(|status| normalize_run_status(&status))
        .transpose()?;
    Ok(transist::RunListQuery {
        pagination: parse_pagination_query(PaginationParams {
            limit: params.limit,
            offset: params.offset,
        })?,
        scope: parse_run_list_scope(params.scope.as_deref())?,
        status,
        workflow: params.workflow.filter(|value| !value.trim().is_empty()),
        created_after_ms: params
            .created_after
            .as_deref()
            .map(crate::application::parse_rfc3339_to_ms)
            .transpose()?,
        created_before_ms: params
            .created_before
            .as_deref()
            .map(crate::application::parse_rfc3339_to_ms)
            .transpose()?,
        updated_after_ms: params
            .updated_after
            .as_deref()
            .map(crate::application::parse_rfc3339_to_ms)
            .transpose()?,
        updated_before_ms: params
            .updated_before
            .as_deref()
            .map(crate::application::parse_rfc3339_to_ms)
            .transpose()?,
    })
}

pub(crate) fn parse_blob_list_scope(
    raw: Option<&str>,
) -> Result<transist::BlobListScope, ApiProblem> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("self") => Ok(transist::BlobListScope::SelfOnly),
        Some("subtree") => Ok(transist::BlobListScope::Subtree),
        Some(other) => Err(ApiProblem::unsupported_query_value(format!(
            "unsupported blob scope '{other}'; expected self or subtree"
        ))),
    }
}

fn parse_run_list_scope(raw: Option<&str>) -> Result<transist::RunListScope, ApiProblem> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("roots") => Ok(transist::RunListScope::Roots),
        Some("all") => Ok(transist::RunListScope::All),
        Some(other) => Err(ApiProblem::unsupported_query_value(format!(
            "unsupported run scope '{other}'; expected roots or all"
        ))),
    }
}

pub(crate) fn parse_events_query(
    params: EventsQueryParams,
) -> Result<transist::EventListQuery, ApiProblem> {
    let order = match params.order.as_deref() {
        None | Some("desc") => transist::SortOrder::Desc,
        Some("asc") => transist::SortOrder::Asc,
        Some(other) => {
            return Err(ApiProblem::unsupported_query_value(format!(
                "unsupported order '{other}'; expected asc or desc"
            )));
        }
    };
    Ok(transist::EventListQuery {
        limit: parse_usize_query(params.limit.as_deref(), "limit")?
            .unwrap_or(transist::DEFAULT_EVENT_LIMIT),
        before: parse_u64_query(params.before.as_deref(), "before")?,
        after: parse_u64_query(params.after.as_deref(), "after")?,
        order,
    })
}

pub(crate) fn parse_pagination_query(
    params: PaginationParams,
) -> Result<transist::PaginationQuery, ApiProblem> {
    Ok(transist::PaginationQuery {
        limit: parse_usize_query(params.limit.as_deref(), "limit")?
            .unwrap_or(transist::DEFAULT_PAGE_LIMIT),
        offset: parse_usize_query(params.offset.as_deref(), "offset")?.unwrap_or_default(),
    })
}

fn parse_usize_query(raw: Option<&str>, field: &str) -> Result<Option<usize>, ApiProblem> {
    raw.map(|value| {
        value.parse::<usize>().map_err(|_| {
            ApiProblem::invalid_request(format!("invalid numeric value for '{field}'"))
        })
    })
    .transpose()
}

fn parse_u64_query(raw: Option<&str>, field: &str) -> Result<Option<u64>, ApiProblem> {
    raw.map(|value| {
        value.parse::<u64>().map_err(|_| {
            ApiProblem::invalid_request(format!("invalid numeric value for '{field}'"))
        })
    })
    .transpose()
}

fn normalize_run_status(raw: &str) -> Result<String, ApiProblem> {
    let normalized = raw.trim();
    if matches!(
        normalized,
        "created"
            | "planned"
            | "running"
            | "waiting"
            | "blocked"
            | "success"
            | "failed"
            | "canceled"
    ) {
        return Ok(normalized.to_string());
    }
    Err(ApiProblem::unsupported_query_value(format!(
        "unsupported run status '{raw}'"
    )))
}
