// SPDX-License-Identifier: MPL-2.0
use super::handlers;
use super::routes::{ApiRoute, COMPANION_API_ROUTES, INSPECTION_GET_ROUTES};
use super::views as ssr;
use axum::{Router, routing::get};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
/// Runtime configuration for launching the companion HTTP server.
pub struct ServerConfig {
    /// Workspace root used to discover workflows and related inspection data.
    pub repo_root: PathBuf,
    /// Effective Transist home directory used for local inspection storage.
    pub home_dir: PathBuf,
    /// IP address that the server listener binds to.
    pub bind_ip: IpAddr,
    /// TCP port that the server listener binds to.
    pub port: u16,
}

impl ServerConfig {
    /// Builds a config with the default loopback bind and default port.
    pub fn with_paths(repo_root: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            repo_root,
            home_dir,
            bind_ip: "127.0.0.1".parse().expect("default ip"),
            port: 7777,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) inspection: crate::application::InspectionApiService,
    pub(crate) metrics: crate::application::MetricsApiService,
}

/// Builds a router from explicit workspace and home directory paths.
pub fn app_for_paths(repo_root: PathBuf, home_dir: PathBuf) -> Router {
    let inspection = Arc::new(transist::TransistInspectionContext::with_home(
        repo_root.clone(),
        home_dir.clone(),
    ));
    let metrics = Arc::new(transist::TransistMetricsContext::with_home(home_dir));
    app_with_services_and_workspace(inspection, metrics, Some(repo_root))
}

/// Builds a router from an inspection query port and an empty metrics provider.
pub fn app_with_query(query: Arc<dyn transist::InspectionQueryPort>) -> Router {
    app_with_services(query, Arc::new(crate::application::EmptyMetricsQuery))
}

/// Builds a router from explicit inspection and metrics query services.
pub fn app_with_services(
    inspection_query: Arc<dyn transist::InspectionQueryPort>,
    metrics_query: Arc<dyn transist::MetricsQueryPort>,
) -> Router {
    app_with_services_and_workspace(inspection_query, metrics_query, None)
}

fn app_with_services_and_workspace(
    inspection_query: Arc<dyn transist::InspectionQueryPort>,
    metrics_query: Arc<dyn transist::MetricsQueryPort>,
    workspace_root: Option<PathBuf>,
) -> Router {
    let state = Arc::new(AppState {
        inspection: crate::application::InspectionApiService::from_query_with_workspace_root(
            inspection_query,
            workspace_root,
        ),
        metrics: crate::application::MetricsApiService::from_query(metrics_query),
    });

    let mut app = Router::new();
    for route in COMPANION_API_ROUTES
        .iter()
        .chain(INSPECTION_GET_ROUTES.iter())
    {
        app = app.route(route.path, registered_get_handler(route));
    }

    app.with_state(state)
}

/// Maps one companion API inventory entry to its Axum `GET` handler.
///
/// Every `operation_id` in [`COMPANION_API_ROUTES`] must have a matching arm
/// here; the route inventory test guards that mapping.
fn api_v1_handler(route: &ApiRoute) -> axum::routing::MethodRouter<Arc<AppState>> {
    match route.operation_id {
        "listRuns" => get(handlers::list_runs),
        "getRun" => get(handlers::get_run),
        "getRunTrust" => get(handlers::get_run_trust),
        "getRunLocality" => get(handlers::get_run_locality),
        "listRunEvents" => get(handlers::list_events),
        "listRunSteps" => get(handlers::list_steps),
        "getRunStep" => get(handlers::get_step),
        "listRunGates" => get(handlers::list_gates),
        "getRunDocument" => get(handlers::get_document),
        "getRunDocumentContent" => get(handlers::get_document_content),
        "listRunFailures" => get(handlers::list_failures),
        "getRunFailure" => get(handlers::get_failure),
        "listRunArtifacts" => get(handlers::list_artifacts),
        "getRunArtifact" => get(handlers::get_artifact),
        "getRunArtifactContent" => get(handlers::get_artifact_content),
        "listRunEvidence" => get(handlers::list_evidence),
        "getRunEvidence" => get(handlers::get_evidence),
        "getRunEvidenceContent" => get(handlers::get_evidence_content),
        "getRunWorkflowFile" => get(handlers::get_workflow_file),
        "getRunWorkflowFileContent" => get(handlers::get_workflow_file_content),
        "listWorkflows" => get(handlers::list_workflows),
        "getWorkflow" => get(handlers::get_workflow),
        "getWorkflowSource" => get(handlers::get_workflow_source),
        "getWorkflowCompiledRecipe" => get(handlers::get_workflow_compiled_recipe),
        other => unreachable!("unmapped companion API operation_id: {other}"),
    }
}

fn registered_get_handler(route: &ApiRoute) -> axum::routing::MethodRouter<Arc<AppState>> {
    match route.operation_id {
        "health" => get(handlers::health),
        "metrics" => get(handlers::metrics),
        "inspectRoot" => get(ssr::inspect_root_redirect),
        "inspectRuns" => get(ssr::runs_list),
        "inspectRun" => get(ssr::run_detail),
        "inspectRunFailures" => get(ssr::run_failures),
        "inspectStep" => get(ssr::step_detail),
        "inspectArtifact" => get(ssr::artifact_detail),
        "inspectEvidence" => get(ssr::evidence_detail),
        "inspectWorkflowFile" => get(ssr::workflow_file_detail),
        "inspectWorkflows" => get(ssr::workflows_list),
        "inspectWorkflow" => get(ssr::workflow_detail),
        "inspectWorkflowSource" => get(ssr::workflow_source),
        "inspectWorkflowCompiled" => get(ssr::workflow_compiled),
        "inspectStatic" => get(ssr::inspect_static),
        _ if route.path.starts_with("/api/v1/") => api_v1_handler(route),
        other => unreachable!("unmapped registered GET operation_id: {other}"),
    }
}

/// Runs the Axum server with local query implementations and background metric refresh.
pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    let inspection = Arc::new(transist::TransistInspectionContext::with_home(
        config.repo_root.clone(),
        config.home_dir.clone(),
    ));
    let metrics = Arc::new(transist::TransistMetricsContext::with_home(
        config.home_dir.clone(),
    ));
    let refresh_metrics = metrics.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            if let Err(error) = refresh_metrics.refresh_changed_runs() {
                tracing::error!(error = %error, "background metrics refresh failed");
                let _ = refresh_metrics
                    .increment_global_counter("transist_metrics_rebuild_errors_total");
            }
        }
    });
    let app = app_with_services_and_workspace(inspection, metrics, Some(config.repo_root.clone()));
    let addr = SocketAddr::from((config.bind_ip, config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(anyhow::Error::from)?;
    tracing::info!("{}", startup_message(&config, addr));
    axum::serve(listener, app)
        .await
        .map_err(anyhow::Error::from)
}

fn startup_message(config: &ServerConfig, addr: SocketAddr) -> String {
    format!(
        "transist-server listening on http://{addr}\
       \n  inspect:   http://{addr}/inspect\
       \n  health:    http://{addr}/health\
       \n  repo_root: {}\
       \n  home:      {}",
        config.repo_root.display(),
        config.home_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_message_reports_urls_and_paths() {
        let config = ServerConfig {
            repo_root: PathBuf::from("/tmp/repo"),
            home_dir: PathBuf::from("/tmp/home"),
            bind_ip: "127.0.0.1".parse().expect("bind ip"),
            port: 7777,
        };

        let message = startup_message(&config, SocketAddr::from((config.bind_ip, config.port)));

        assert!(message.contains("transist-server listening on http://127.0.0.1:7777"));
        assert!(message.contains("inspect:"));
        assert!(message.contains("http://127.0.0.1:7777/inspect"));
        assert!(message.contains("health:"));
        assert!(message.contains("http://127.0.0.1:7777/health"));
        assert!(message.contains("repo_root: /tmp/repo"));
        assert!(message.contains("home:"));
        assert!(message.contains("/tmp/home"));
    }
}
