use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use orchestrator_web_api::{WebApiError, WebApiService};
use orchestrator_web_contracts::{
    http_status_for_exit_code, CliEnvelopeService, DaemonEventRecord,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::models::WebServerConfig;
use crate::services::docs_html::render_openapi_docs_html;
use crate::services::openapi_spec::build_openapi_spec;

static EMBEDDED_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/embedded");

#[derive(Clone)]
struct AppState {
    api: WebApiService,
    assets_dir: Option<PathBuf>,
    api_only: bool,
}

pub struct WebServer {
    config: WebServerConfig,
    api: WebApiService,
}

impl WebServer {
    pub fn new(config: WebServerConfig, api: WebApiService) -> Self {
        Self { config, api }
    }

    pub async fn run(self) -> Result<()> {
        let state = AppState {
            api: self.api,
            assets_dir: self.config.assets_dir.map(PathBuf::from),
            api_only: self.config.api_only,
        };

        let router = build_router(state);
        let address = format!("{}:{}", self.config.host, self.config.port);
        let listener = tokio::net::TcpListener::bind(&address)
            .await
            .with_context(|| format!("failed to bind web server at {address}"))?;

        axum::serve(listener, router)
            .await
            .context("web server failed")?;

        Ok(())
    }
}

fn build_router(state: AppState) -> Router {
    let api_router = Router::new()
        .route("/system/info", get(system_info_handler))
        .route("/openapi.json", get(openapi_spec_handler))
        .route("/docs", get(openapi_docs_handler))
        .route("/events", get(events_handler))
        .route("/daemon/status", get(daemon_status_handler))
        .route("/daemon/health", get(daemon_health_handler))
        .route("/daemon/logs", get(daemon_logs_handler))
        .route("/daemon/logs", delete(daemon_clear_logs_handler))
        .route("/daemon/start", post(daemon_start_handler))
        .route("/daemon/stop", post(daemon_stop_handler))
        .route("/daemon/pause", post(daemon_pause_handler))
        .route("/daemon/resume", post(daemon_resume_handler))
        .route("/daemon/agents", get(daemon_agents_handler))
        .route("/projects", get(projects_list_handler))
        .route("/projects", post(projects_create_handler))
        .route("/projects/active", get(projects_active_handler))
        .route("/project-requirements", get(projects_requirements_handler))
        .route("/projects/:id", get(projects_get_handler))
        .route("/projects/:id/tasks", get(project_tasks_handler))
        .route("/projects/:id/workflows", get(project_workflows_handler))
        .route("/projects/:id", patch(projects_patch_handler))
        .route("/projects/:id", delete(projects_delete_handler))
        .route(
            "/project-requirements/:id",
            get(projects_requirements_by_id_handler),
        )
        .route(
            "/project-requirements/:project_id/:requirement_id",
            get(project_requirement_get_handler),
        )
        .route("/projects/:id/load", post(projects_load_handler))
        .route("/projects/:id/archive", post(projects_archive_handler))
        .route("/requirements", get(requirements_list_handler))
        .route("/requirements/:id", get(requirements_get_handler))
        .route("/tasks", get(tasks_list_handler))
        .route("/tasks", post(tasks_create_handler))
        .route("/tasks/prioritized", get(tasks_prioritized_handler))
        .route("/tasks/next", get(tasks_next_handler))
        .route("/tasks/stats", get(tasks_stats_handler))
        .route("/tasks/:id", get(tasks_get_handler))
        .route("/tasks/:id", patch(tasks_patch_handler))
        .route("/tasks/:id", delete(tasks_delete_handler))
        .route("/tasks/:id/status", post(tasks_status_handler))
        .route("/tasks/:id/assign-agent", post(tasks_assign_agent_handler))
        .route("/tasks/:id/assign-human", post(tasks_assign_human_handler))
        .route("/tasks/:id/checklist", post(tasks_checklist_add_handler))
        .route(
            "/tasks/:id/checklist/:item_id",
            patch(tasks_checklist_update_handler),
        )
        .route(
            "/tasks/:id/dependencies",
            post(tasks_dependency_add_handler),
        )
        .route(
            "/tasks/:id/dependencies/:dependency_id",
            delete(tasks_dependency_remove_handler),
        )
        .route("/workflows", get(workflows_list_handler))
        .route("/workflows/run", post(workflows_run_handler))
        .route("/workflows/:id", get(workflows_get_handler))
        .route("/workflows/:id/decisions", get(workflows_decisions_handler))
        .route(
            "/workflows/:id/checkpoints",
            get(workflows_checkpoints_handler),
        )
        .route(
            "/workflows/:id/checkpoints/:checkpoint",
            get(workflows_get_checkpoint_handler),
        )
        .route("/workflows/:id/resume", post(workflows_resume_handler))
        .route("/workflows/:id/pause", post(workflows_pause_handler))
        .route("/workflows/:id/cancel", post(workflows_cancel_handler))
        .route("/reviews/handoff", post(reviews_handoff_handler));

    Router::new()
        .nest("/api/v1", api_router)
        .route("/", get(root_handler))
        .route("/*path", get(static_handler))
        .with_state(state)
}

async fn system_info_handler(State(state): State<AppState>) -> Response {
    match state.api.system_info().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn openapi_spec_handler() -> Response {
    Json(build_openapi_spec()).into_response()
}

async fn openapi_docs_handler() -> Response {
    Html(render_openapi_docs_html()).into_response()
}

async fn daemon_status_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_status().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_health_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_health().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_logs_handler(
    State(state): State<AppState>,
    Query(query): Query<DaemonLogsQuery>,
) -> Response {
    match state.api.daemon_logs(query.limit).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_clear_logs_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_clear_logs().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_start_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_start().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_stop_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_stop().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_pause_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_pause().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_resume_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_resume().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn daemon_agents_handler(State(state): State<AppState>) -> Response {
    match state.api.daemon_agents().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_list_handler(State(state): State<AppState>) -> Response {
    match state.api.projects_list().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_active_handler(State(state): State<AppState>) -> Response {
    match state.api.projects_active().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_requirements_handler(State(state): State<AppState>) -> Response {
    match state.api.projects_requirements().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_get_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.projects_get(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn project_tasks_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<TasksListQuery>,
) -> Response {
    match state
        .api
        .project_tasks(
            &id,
            query.task_type,
            query.status,
            query.priority,
            query.risk,
            query.assignee_type,
            query.tag,
            query.linked_requirement,
            query.search,
        )
        .await
    {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn project_workflows_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.project_workflows(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_create_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.projects_create(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_load_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.projects_load(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_patch_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.projects_patch(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_archive_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.projects_archive(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_requirements_by_id_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.projects_requirements_by_id(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn project_requirement_get_handler(
    State(state): State<AppState>,
    AxumPath((project_id, requirement_id)): AxumPath<(String, String)>,
) -> Response {
    match state
        .api
        .project_requirement_get(&project_id, &requirement_id)
        .await
    {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_delete_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.projects_delete(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_list_handler(State(state): State<AppState>) -> Response {
    match state.api.requirements_list().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_get_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.requirements_get(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_list_handler(
    State(state): State<AppState>,
    Query(query): Query<TasksListQuery>,
) -> Response {
    match state
        .api
        .tasks_list(
            query.task_type,
            query.status,
            query.priority,
            query.risk,
            query.assignee_type,
            query.tag,
            query.linked_requirement,
            query.search,
        )
        .await
    {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_prioritized_handler(State(state): State<AppState>) -> Response {
    match state.api.tasks_prioritized().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_next_handler(State(state): State<AppState>) -> Response {
    match state.api.tasks_next().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_stats_handler(State(state): State<AppState>) -> Response {
    match state.api.tasks_stats().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_get_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.tasks_get(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_create_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.tasks_create(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_patch_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_patch(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_delete_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.tasks_delete(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_status_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_status(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_assign_agent_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_assign_agent(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_assign_human_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_assign_human(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_checklist_add_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_checklist_add(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_checklist_update_handler(
    State(state): State<AppState>,
    AxumPath((id, item_id)): AxumPath<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_checklist_update(&id, &item_id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_dependency_add_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.tasks_dependency_add(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_dependency_remove_handler(
    State(state): State<AppState>,
    AxumPath((id, dependency_id)): AxumPath<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    match state
        .api
        .tasks_dependency_remove(&id, &dependency_id, body.map(|json| json.0))
        .await
    {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_list_handler(State(state): State<AppState>) -> Response {
    match state.api.workflows_list().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_get_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_get(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_decisions_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_decisions(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_checkpoints_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_checkpoints(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_get_checkpoint_handler(
    State(state): State<AppState>,
    AxumPath((id, checkpoint)): AxumPath<(String, usize)>,
) -> Response {
    match state.api.workflows_get_checkpoint(&id, checkpoint).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_run_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.workflows_run(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_resume_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_resume(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_pause_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_pause(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_cancel_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.workflows_cancel(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn reviews_handoff_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.reviews_handoff(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn events_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let last_event_id = parse_last_event_id(&headers);
    let replay = match state.api.read_events_since(last_event_id) {
        Ok(events) => events,
        Err(error) => return error_response(error),
    };

    let mut receiver = state.api.subscribe_events();
    let stream = stream! {
        let mut cursor = last_event_id.unwrap_or(0);

        for event_record in replay {
            cursor = cursor.max(event_record.seq);
            yield Ok::<Event, Infallible>(to_sse_event(event_record));
        }

        loop {
            match receiver.recv().await {
                Ok(event_record) => {
                    if event_record.seq <= cursor {
                        continue;
                    }
                    cursor = event_record.seq;
                    yield Ok::<Event, Infallible>(to_sse_event(event_record));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

async fn root_handler(State(state): State<AppState>) -> Response {
    if state.api_only {
        return success_response(json!({
            "message": "ao web server running in api-only mode",
            "api_base": "/api/v1",
        }));
    }

    serve_static_asset(&state, "index.html").await
}

async fn static_handler(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    if state.api_only {
        return not_found_response("not found");
    }

    let relative_path = normalize_asset_path(&path);
    serve_static_asset(&state, &relative_path).await
}

async fn serve_static_asset(state: &AppState, requested_path: &str) -> Response {
    let normalized_path = normalize_asset_path(requested_path);

    if let Some(asset) = load_asset_from_disk(state, &normalized_path).await {
        return binary_response(asset.bytes, &asset.content_type);
    }

    if let Some(asset) = load_asset_from_embedded(&normalized_path) {
        return binary_response(asset.bytes, &asset.content_type);
    }

    if let Some(index_asset) = load_asset_from_disk(state, "index.html").await {
        return binary_response(index_asset.bytes, &index_asset.content_type);
    }

    if let Some(index_asset) = load_asset_from_embedded("index.html") {
        return binary_response(index_asset.bytes, &index_asset.content_type);
    }

    not_found_response("asset not found")
}

fn success_response(data: Value) -> Response {
    let envelope = CliEnvelopeService::ok(data);
    (StatusCode::OK, Json(envelope)).into_response()
}

fn error_response(error: WebApiError) -> Response {
    let status = http_status_for_exit_code(error.exit_code);
    let envelope = CliEnvelopeService::error(error.code, error.message, error.exit_code);
    (status, Json(envelope)).into_response()
}

fn not_found_response(message: &str) -> Response {
    let envelope = CliEnvelopeService::error("not_found", message, 3);
    (StatusCode::NOT_FOUND, Json(envelope)).into_response()
}

fn to_sse_event(record: DaemonEventRecord) -> Event {
    let payload = serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string());
    Event::default()
        .event("daemon-event")
        .id(record.seq.to_string())
        .data(payload)
}

fn parse_last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn normalize_asset_path(path: &str) -> String {
    let sanitized = sanitize_relative_path(path).unwrap_or_else(|| PathBuf::from("index.html"));
    let normalized = sanitized.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() {
        "index.html".to_string()
    } else {
        normalized
    }
}

fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(PathBuf::from("index.html"));
    }

    let candidate = Path::new(trimmed);
    let mut safe = PathBuf::new();

    for component in candidate.components() {
        match component {
            Component::Normal(segment) => safe.push(segment),
            Component::CurDir => continue,
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }

    if safe.as_os_str().is_empty() {
        return Some(PathBuf::from("index.html"));
    }

    Some(safe)
}

async fn load_asset_from_disk(state: &AppState, requested_path: &str) -> Option<AssetPayload> {
    let assets_dir = state.assets_dir.as_ref()?;
    let sanitized = sanitize_relative_path(requested_path)?;
    let full_path = assets_dir.join(sanitized);

    if !full_path.exists() || !full_path.is_file() {
        return None;
    }

    let bytes = tokio::fs::read(&full_path).await.ok()?;
    let content_type = mime_guess::from_path(&full_path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();

    Some(AssetPayload {
        bytes,
        content_type,
    })
}

fn load_asset_from_embedded(requested_path: &str) -> Option<AssetPayload> {
    let file = EMBEDDED_ASSETS.get_file(requested_path)?;
    let bytes = file.contents().to_vec();
    let content_type = mime_guess::from_path(requested_path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();

    Some(AssetPayload {
        bytes,
        content_type,
    })
}

fn binary_response(bytes: Vec<u8>, content_type: &str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let header_value = HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, header_value);
    response
}

#[derive(Debug)]
struct AssetPayload {
    bytes: Vec<u8>,
    content_type: String,
}

#[derive(Debug, Deserialize)]
struct DaemonLogsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TasksListQuery {
    task_type: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    risk: Option<String>,
    assignee_type: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    linked_requirement: Option<String>,
    search: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::header::CONTENT_TYPE;
    use axum::http::Request;
    use orchestrator_core::{InMemoryServiceHub, ServiceHub};
    use orchestrator_web_api::WebApiContext;
    use serde_json::Value;
    use tower::util::ServiceExt;

    use super::{build_router, AppState};

    #[tokio::test]
    async fn system_info_endpoint_returns_cli_envelope() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/system/info")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn openapi_endpoint_returns_spec_json() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/openapi.json")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let payload: Value =
            serde_json::from_slice(&body).expect("openapi endpoint should return valid JSON");
        assert_eq!(
            payload["openapi"].as_str(),
            Some("3.1.0"),
            "spec should declare OpenAPI 3.1"
        );
    }

    #[tokio::test]
    async fn openapi_docs_endpoint_returns_html() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/docs")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("text/html"),
            "docs endpoint should return HTML"
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let html = String::from_utf8(body.to_vec()).expect("docs response should be utf-8");
        assert!(
            html.contains("SwaggerUIBundle"),
            "docs response should include Swagger UI bootstrap"
        );
    }

    #[tokio::test]
    async fn reviews_handoff_endpoint_returns_enveloped_response() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/reviews/handoff")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "run_id": "",
                            "target_role": "em",
                            "question": "Is this ready?",
                            "context": {}
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

        assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(
            payload
                .get("data")
                .and_then(|data| data.get("status"))
                .and_then(Value::as_str),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn project_tasks_endpoint_returns_not_found_for_unknown_project() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/projects/does-not-exist/tasks")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn project_workflows_endpoint_returns_not_found_for_unknown_project() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/projects/does-not-exist/workflows")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tasks_list_rejects_invalid_risk_filter() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        let app = build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?risk=spicy")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}
