use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower_http::compression::CompressionLayer;

use crate::models::WebServerConfig;
use crate::services::docs_html::render_openapi_docs_html;
use crate::services::openapi_spec::build_openapi_spec;

static EMBEDDED_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/embedded");
const CACHE_CONTROL_REVALIDATE: &str = "private, no-cache";
const HEADER_PAGE_SIZE: &str = "x-ao-page-size";
const HEADER_NEXT_CURSOR: &str = "x-ao-next-cursor";
const HEADER_HAS_MORE: &str = "x-ao-has-more";
const HEADER_TOTAL_COUNT: &str = "x-ao-total-count";

#[derive(Clone)]
struct AppState {
    api: WebApiService,
    assets_dir: Option<PathBuf>,
    api_only: bool,
    default_page_size: usize,
    max_page_size: usize,
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
            default_page_size: self.config.default_page_size.max(1),
            max_page_size: self
                .config
                .max_page_size
                .max(self.config.default_page_size.max(1)),
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
        .route("/projects/{id}", get(projects_get_handler))
        .route("/projects/{id}/tasks", get(project_tasks_handler))
        .route("/projects/{id}/workflows", get(project_workflows_handler))
        .route("/projects/{id}", patch(projects_patch_handler))
        .route("/projects/{id}", delete(projects_delete_handler))
        .route(
            "/project-requirements/{id}",
            get(projects_requirements_by_id_handler),
        )
        .route(
            "/project-requirements/{project_id}/{requirement_id}",
            get(project_requirement_get_handler),
        )
        .route("/projects/{id}/load", post(projects_load_handler))
        .route("/projects/{id}/archive", post(projects_archive_handler))
        .route("/vision", get(vision_get_handler))
        .route("/vision", post(vision_save_handler))
        .route("/vision/refine", post(vision_refine_handler))
        .route("/requirements", get(requirements_list_handler))
        .route("/requirements", post(requirements_create_handler))
        .route("/requirements/draft", post(requirements_draft_handler))
        .route("/requirements/refine", post(requirements_refine_handler))
        .route("/requirements/{id}", get(requirements_get_handler))
        .route("/requirements/{id}", patch(requirements_patch_handler))
        .route("/requirements/{id}", delete(requirements_delete_handler))
        .route("/tasks", get(tasks_list_handler))
        .route("/tasks", post(tasks_create_handler))
        .route("/tasks/prioritized", get(tasks_prioritized_handler))
        .route("/tasks/next", get(tasks_next_handler))
        .route("/tasks/stats", get(tasks_stats_handler))
        .route("/tasks/{id}", get(tasks_get_handler))
        .route("/tasks/{id}", patch(tasks_patch_handler))
        .route("/tasks/{id}", delete(tasks_delete_handler))
        .route("/tasks/{id}/status", post(tasks_status_handler))
        .route("/tasks/{id}/assign-agent", post(tasks_assign_agent_handler))
        .route("/tasks/{id}/assign-human", post(tasks_assign_human_handler))
        .route("/tasks/{id}/checklist", post(tasks_checklist_add_handler))
        .route(
            "/tasks/{id}/checklist/{item_id}",
            patch(tasks_checklist_update_handler),
        )
        .route(
            "/tasks/{id}/dependencies",
            post(tasks_dependency_add_handler),
        )
        .route(
            "/tasks/{id}/dependencies/{dependency_id}",
            delete(tasks_dependency_remove_handler),
        )
        .route("/workflows", get(workflows_list_handler))
        .route("/workflows/run", post(workflows_run_handler))
        .route("/workflows/{id}", get(workflows_get_handler))
        .route(
            "/workflows/{id}/decisions",
            get(workflows_decisions_handler),
        )
        .route(
            "/workflows/{id}/checkpoints",
            get(workflows_checkpoints_handler),
        )
        .route(
            "/workflows/{id}/checkpoints/{checkpoint}",
            get(workflows_get_checkpoint_handler),
        )
        .route("/workflows/{id}/resume", post(workflows_resume_handler))
        .route("/workflows/{id}/pause", post(workflows_pause_handler))
        .route("/workflows/{id}/cancel", post(workflows_cancel_handler))
        .route("/reviews/handoff", post(reviews_handoff_handler));

    Router::new()
        .nest("/api/v1", api_router)
        .route("/", get(root_handler))
        .route("/{*path}", get(static_handler))
        .layer(CompressionLayer::new())
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

async fn daemon_status_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match state.api.daemon_status().await {
        Ok(data) => success_response_with_etag(data, &headers),
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
            query.linked_architecture_entity,
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

async fn vision_get_handler(State(state): State<AppState>) -> Response {
    match state.api.vision_get().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn vision_save_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.vision_save(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn vision_refine_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.vision_refine(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_list_handler(
    State(state): State<AppState>,
    Query(query): Query<ListPaginationQuery>,
) -> Response {
    let pagination =
        match normalize_pagination_query(&query, state.default_page_size, state.max_page_size) {
            Ok(pagination) => pagination,
            Err(error) => return error_response(error),
        };

    match state.api.requirements_list().await {
        Ok(data) => match paginated_success_response(data, pagination, None) {
            Ok(response) => response,
            Err(error) => error_response(error),
        },
        Err(error) => error_response(error),
    }
}

async fn requirements_create_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.requirements_create(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_draft_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.requirements_draft(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_refine_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.requirements_refine(body).await {
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

async fn requirements_patch_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.requirements_patch(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_delete_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.api.requirements_delete(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn tasks_list_handler(
    State(state): State<AppState>,
    Query(query): Query<TasksListQuery>,
    headers: HeaderMap,
) -> Response {
    let pagination = match normalize_pagination_query(
        &query.pagination,
        state.default_page_size,
        state.max_page_size,
    ) {
        Ok(pagination) => pagination,
        Err(error) => return error_response(error),
    };

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
            query.linked_architecture_entity,
            query.search,
        )
        .await
    {
        Ok(data) => match paginated_success_response(data, pagination, Some(&headers)) {
            Ok(response) => response,
            Err(error) => error_response(error),
        },
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

async fn workflows_list_handler(
    State(state): State<AppState>,
    Query(query): Query<ListPaginationQuery>,
) -> Response {
    let pagination =
        match normalize_pagination_query(&query, state.default_page_size, state.max_page_size) {
            Ok(pagination) => pagination,
            Err(error) => return error_response(error),
        };

    match state.api.workflows_list().await {
        Ok(data) => match paginated_success_response(data, pagination, None) {
            Ok(response) => response,
            Err(error) => error_response(error),
        },
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

fn success_response_with_etag(data: Value, request_headers: &HeaderMap) -> Response {
    let etag = compute_etag(&data);
    if if_none_match_matches(request_headers, &etag) {
        return not_modified_response(&etag);
    }

    let mut response = success_response(data);
    attach_cache_headers(&mut response, &etag);
    response
}

fn paginated_success_response(
    data: Value,
    pagination: PaginationRequest,
    conditional_headers: Option<&HeaderMap>,
) -> std::result::Result<Response, WebApiError> {
    let paginated = paginate_array_data(data, pagination)?;
    let etag = conditional_headers.map(|_| compute_etag(&paginated));
    let PaginatedArray {
        items,
        page_size,
        total_count,
        next_cursor,
    } = paginated;

    if let (Some(headers), Some(etag)) = (conditional_headers, etag.as_deref()) {
        if if_none_match_matches(headers, etag) {
            let mut response = not_modified_response(etag);
            attach_pagination_headers(
                &mut response,
                page_size,
                total_count,
                next_cursor.as_deref(),
            );
            return Ok(response);
        }
    }

    let mut response = success_response(Value::Array(items));
    if let Some(etag) = etag.as_deref() {
        attach_cache_headers(&mut response, etag);
    }
    attach_pagination_headers(
        &mut response,
        page_size,
        total_count,
        next_cursor.as_deref(),
    );
    Ok(response)
}

fn normalize_pagination_query(
    query: &ListPaginationQuery,
    default_page_size: usize,
    max_page_size: usize,
) -> std::result::Result<PaginationRequest, WebApiError> {
    let max_page_size = max_page_size.max(1);
    let default_page_size = default_page_size.max(1).min(max_page_size);
    let requested_page_size = match query.page_size.as_deref() {
        None => default_page_size,
        Some(page_size) => parse_page_size(page_size)?,
    };

    let page_size = requested_page_size.min(max_page_size);
    let start = match query.cursor.as_deref() {
        None => 0,
        Some(cursor) => parse_pagination_cursor(cursor)?,
    };

    Ok(PaginationRequest { start, page_size })
}

fn parse_pagination_cursor(cursor: &str) -> std::result::Result<usize, WebApiError> {
    cursor.parse::<usize>().map_err(|_| {
        WebApiError::new(
            "invalid_input",
            format!("invalid cursor `{cursor}`: expected unsigned integer"),
            2,
        )
    })
}

fn parse_page_size(page_size: &str) -> std::result::Result<usize, WebApiError> {
    let parsed = page_size.parse::<usize>().map_err(|_| {
        WebApiError::new(
            "invalid_input",
            format!("invalid page_size `{page_size}`: expected unsigned integer"),
            2,
        )
    })?;

    if parsed == 0 {
        return Err(WebApiError::new(
            "invalid_input",
            "page_size must be at least 1",
            2,
        ));
    }

    Ok(parsed)
}

fn paginate_array_data(
    data: Value,
    pagination: PaginationRequest,
) -> std::result::Result<PaginatedArray, WebApiError> {
    let items = data.as_array().ok_or_else(|| {
        WebApiError::new(
            "internal",
            "expected list endpoint payload to be an array",
            1,
        )
    })?;

    let total_count = items.len();
    let start = pagination.start.min(total_count);
    let end = start.saturating_add(pagination.page_size).min(total_count);
    let page_items = items[start..end].to_vec();
    let next_cursor = if end < total_count {
        Some(end.to_string())
    } else {
        None
    };

    Ok(PaginatedArray {
        items: page_items,
        page_size: pagination.page_size,
        total_count,
        next_cursor,
    })
}

fn attach_pagination_headers(
    response: &mut Response,
    page_size: usize,
    total_count: usize,
    next_cursor: Option<&str>,
) {
    set_response_header(response, HEADER_PAGE_SIZE, &page_size.to_string());
    set_response_header(response, HEADER_TOTAL_COUNT, &total_count.to_string());
    set_response_header(
        response,
        HEADER_HAS_MORE,
        if next_cursor.is_some() {
            "true"
        } else {
            "false"
        },
    );
    if let Some(next_cursor) = next_cursor {
        set_response_header(response, HEADER_NEXT_CURSOR, next_cursor);
    }
}

fn compute_etag<T: Serialize>(data: &T) -> String {
    let payload = serde_json::to_vec(data).unwrap_or_default();
    let digest = Sha256::digest(payload);
    format!("\"{:x}\"", digest)
}

fn if_none_match_matches(headers: &HeaderMap, current_etag: &str) -> bool {
    let normalized_current = normalize_etag(current_etag);
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value.split(',').map(str::trim).any(|candidate| {
                candidate == "*" || normalize_etag(candidate) == normalized_current
            })
        })
        .unwrap_or(false)
}

fn normalize_etag(value: &str) -> &str {
    value.trim().strip_prefix("W/").unwrap_or(value.trim())
}

fn not_modified_response(etag: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    attach_cache_headers(&mut response, etag);
    response
}

fn attach_cache_headers(response: &mut Response, etag: &str) {
    if let Ok(etag_value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(ETAG, etag_value);
    }
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_REVALIDATE),
    );
}

fn set_response_header(response: &mut Response, name: &'static str, value: &str) {
    if let Ok(header_value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, header_value);
    }
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

#[derive(Debug, Default, Deserialize)]
struct ListPaginationQuery {
    cursor: Option<String>,
    page_size: Option<String>,
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
    linked_architecture_entity: Option<String>,
    search: Option<String>,
    #[serde(flatten)]
    pagination: ListPaginationQuery,
}

#[derive(Debug, Clone, Copy)]
struct PaginationRequest {
    start: usize,
    page_size: usize,
}

#[derive(Debug, Serialize)]
struct PaginatedArray {
    items: Vec<Value>,
    page_size: usize,
    total_count: usize,
    next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
    use axum::http::Request;
    use axum::Router;
    use orchestrator_core::{InMemoryServiceHub, ServiceHub, TaskCreateInput};
    use orchestrator_web_api::WebApiContext;
    use serde_json::Value;
    use tower::util::ServiceExt;

    use super::{
        build_router, AppState, HEADER_HAS_MORE, HEADER_NEXT_CURSOR, HEADER_PAGE_SIZE,
        HEADER_TOTAL_COUNT,
    };

    fn build_test_app(
        hub: Arc<dyn ServiceHub>,
        api_only: bool,
        default_page_size: usize,
        max_page_size: usize,
    ) -> Router {
        let context = Arc::new(WebApiContext {
            hub,
            project_root: "/tmp/project".to_string(),
            app_version: "test-version".to_string(),
        });
        let api = orchestrator_web_api::WebApiService::new(context);
        build_router(AppState {
            api,
            assets_dir: None,
            api_only,
            default_page_size,
            max_page_size,
        })
    }

    async fn seed_tasks(hub: &Arc<dyn ServiceHub>, count: usize) {
        let base_index = hub
            .tasks()
            .list()
            .await
            .expect("tasks should list for seeding")
            .len();
        for index in 0..count {
            hub.tasks()
                .create(TaskCreateInput {
                    title: format!("Task {}", base_index + index),
                    description: "Task for pagination test".to_string(),
                    task_type: None,
                    priority: None,
                    created_by: Some("test".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                })
                .await
                .expect("task should be created");
        }
    }

    async fn seed_requirements(app: &Router, count: usize) {
        for index in 0..count {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/requirements")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "title": format!("Requirement {index}"),
                                "description": "Requirement for pagination test"
                            })
                            .to_string(),
                        ))
                        .expect("request should be built"),
                )
                .await
                .expect("request should succeed");

            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "requirement seed request should succeed at index {index}"
            );
        }
    }

    async fn seed_workflows(app: &Router, count: usize) {
        for index in 0..count {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/workflows/run")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "task_id": format!("TASK-{}", index + 1)
                            })
                            .to_string(),
                        ))
                        .expect("request should be built"),
                )
                .await
                .expect("request should succeed");

            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "workflow seed request should succeed at index {index}"
            );
        }
    }

    #[test]
    fn tasks_list_query_deserializes_positive_page_size() {
        let uri: axum::http::Uri = "/api/v1/tasks?page_size=200"
            .parse()
            .expect("uri should parse");
        let query = axum::extract::Query::<super::TasksListQuery>::try_from_uri(&uri)
            .expect("query extraction should parse page_size")
            .0;
        assert_eq!(query.pagination.page_size.as_deref(), Some("200"));
        assert_eq!(query.pagination.cursor, None);
    }

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
            default_page_size: 50,
            max_page_size: 200,
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
            default_page_size: 50,
            max_page_size: 200,
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
            default_page_size: 50,
            max_page_size: 200,
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
            default_page_size: 50,
            max_page_size: 200,
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
    async fn planning_mutation_endpoints_round_trip_successfully() {
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
            default_page_size: 50,
            max_page_size: 200,
        });

        let vision_save_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/vision")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "project_name": "AO",
                            "problem_statement": "Planning is fragmented",
                            "target_users": ["PM"],
                            "goals": ["Ship planning UI"],
                            "constraints": ["Keep deterministic state"],
                            "value_proposition": "Faster planning loops"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(vision_save_response.status(), axum::http::StatusCode::OK);

        let vision_refine_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/vision/refine")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "focus": "quality gates"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(vision_refine_response.status(), axum::http::StatusCode::OK);

        let requirement_create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/requirements")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "title": "Planning route coverage",
                            "description": "Add deep links for planning surfaces",
                            "acceptance_criteria": ["Route is directly addressable"],
                            "priority": "must",
                            "status": "draft"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            requirement_create_response.status(),
            axum::http::StatusCode::OK
        );

        let requirement_create_body = to_bytes(requirement_create_response.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let requirement_create_payload: Value = serde_json::from_slice(&requirement_create_body)
            .expect("response should be valid json");
        let requirement_id = requirement_create_payload["data"]["id"]
            .as_str()
            .expect("created requirement should include an id")
            .to_string();

        let requirement_patch_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/requirements/{requirement_id}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "status": "planned",
                            "title": "Planning route and mutation coverage"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            requirement_patch_response.status(),
            axum::http::StatusCode::OK
        );

        let requirement_refine_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/requirements/refine")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "requirement_ids": [requirement_id]
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            requirement_refine_response.status(),
            axum::http::StatusCode::OK
        );

        let requirement_delete_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/requirements/{requirement_id}"))
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            requirement_delete_response.status(),
            axum::http::StatusCode::OK
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
            default_page_size: 50,
            max_page_size: 200,
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
            default_page_size: 50,
            max_page_size: 200,
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
            default_page_size: 50,
            max_page_size: 200,
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

    #[tokio::test]
    async fn ui_deep_links_return_spa_html_when_ui_enabled() {
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
            api_only: false,
            default_page_size: 50,
            max_page_size: 200,
        });

        let routes = [
            "/dashboard",
            "/daemon",
            "/projects",
            "/projects/proj-1",
            "/projects/proj-1/requirements/REQ-1",
            "/planning",
            "/planning/vision",
            "/planning/requirements",
            "/planning/requirements/new",
            "/planning/requirements/REQ-1",
            "/tasks",
            "/tasks/TASK-1",
            "/workflows",
            "/workflows/wf-1",
            "/workflows/wf-1/checkpoints/2",
            "/events",
            "/reviews/handoff",
        ];

        for route in routes {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(route)
                        .body(Body::empty())
                        .expect("request should be built"),
                )
                .await
                .expect("request should succeed");

            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "{route} should return SPA html"
            );

            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            assert!(
                content_type.starts_with("text/html"),
                "{route} should return text/html content type"
            );
        }
    }

    #[tokio::test]
    async fn api_only_mode_rejects_ui_deep_links() {
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
            default_page_size: 50,
            max_page_size: 200,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");
        assert_eq!(payload.get("ok"), Some(&Value::Bool(false)));
    }

    #[tokio::test]
    async fn tasks_list_supports_cursor_pagination_with_default_page_size() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 60).await;
        let app = build_test_app(hub, true, 50, 200);

        let first_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(first_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("50")
        );
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        let next_cursor = first_page
            .headers()
            .get(HEADER_NEXT_CURSOR)
            .and_then(|value| value.to_str().ok())
            .expect("first page should return next cursor")
            .to_string();

        let first_page_body = to_bytes(first_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let first_page_payload: Value =
            serde_json::from_slice(&first_page_body).expect("response should be valid json");
        assert_eq!(
            first_page_payload["data"]
                .as_array()
                .expect("tasks list payload should be an array")
                .len(),
            50
        );

        let second_page = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/tasks?cursor={next_cursor}"))
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(second_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            second_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );
        assert!(
            second_page.headers().get(HEADER_NEXT_CURSOR).is_none(),
            "final page should not include a next cursor header"
        );

        let second_page_body = to_bytes(second_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let second_page_payload: Value =
            serde_json::from_slice(&second_page_body).expect("response should be valid json");
        assert_eq!(
            second_page_payload["data"]
                .as_array()
                .expect("tasks list payload should be an array")
                .len(),
            10
        );
    }

    #[tokio::test]
    async fn tasks_list_rejects_invalid_cursor() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?cursor=not-a-number")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tasks_list_rejects_zero_page_size() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?page_size=0")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tasks_list_caps_requested_page_size_to_configured_maximum() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 45).await;
        let app = build_test_app(hub, true, 25, 30);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?page_size=500")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("30")
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_TOTAL_COUNT)
                .and_then(|value| value.to_str().ok()),
            Some("45")
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_NEXT_CURSOR)
                .and_then(|value| value.to_str().ok()),
            Some("30")
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");
        assert_eq!(
            payload["data"]
                .as_array()
                .expect("tasks list payload should be an array")
                .len(),
            30
        );
    }

    #[tokio::test]
    async fn requirements_list_applies_pagination_headers() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/requirements")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("50")
        );
    }

    #[tokio::test]
    async fn requirements_list_supports_cursor_pagination_with_page_size_cap() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 25, 30);
        seed_requirements(&app, 45).await;

        let first_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/requirements?page_size=500")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(first_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("30")
        );
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_TOTAL_COUNT)
                .and_then(|value| value.to_str().ok()),
            Some("45")
        );
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        let next_cursor = first_page
            .headers()
            .get(HEADER_NEXT_CURSOR)
            .and_then(|value| value.to_str().ok())
            .expect("first page should include next cursor")
            .to_string();

        let first_page_body = to_bytes(first_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let first_page_payload: Value =
            serde_json::from_slice(&first_page_body).expect("response should be valid json");
        assert_eq!(
            first_page_payload["data"]
                .as_array()
                .expect("requirements list payload should be an array")
                .len(),
            30
        );

        let second_page = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/requirements?cursor={next_cursor}&page_size=500"))
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(second_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            second_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );
        assert!(
            second_page.headers().get(HEADER_NEXT_CURSOR).is_none(),
            "final page should not include next cursor"
        );

        let second_page_body = to_bytes(second_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let second_page_payload: Value =
            serde_json::from_slice(&second_page_body).expect("response should be valid json");
        assert_eq!(
            second_page_payload["data"]
                .as_array()
                .expect("requirements list payload should be an array")
                .len(),
            15
        );
    }

    #[tokio::test]
    async fn workflows_list_applies_pagination_headers() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/workflows")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("50")
        );
    }

    #[tokio::test]
    async fn workflows_list_supports_cursor_pagination_with_page_size_cap() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 25, 30);
        seed_workflows(&app, 45).await;

        let first_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/workflows?page_size=500")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(first_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("30")
        );
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_TOTAL_COUNT)
                .and_then(|value| value.to_str().ok()),
            Some("45")
        );
        assert_eq!(
            first_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        let next_cursor = first_page
            .headers()
            .get(HEADER_NEXT_CURSOR)
            .and_then(|value| value.to_str().ok())
            .expect("first page should include next cursor")
            .to_string();

        let first_page_body = to_bytes(first_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let first_page_payload: Value =
            serde_json::from_slice(&first_page_body).expect("response should be valid json");
        assert_eq!(
            first_page_payload["data"]
                .as_array()
                .expect("workflows list payload should be an array")
                .len(),
            30
        );

        let second_page = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/workflows?cursor={next_cursor}&page_size=500"))
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(second_page.status(), axum::http::StatusCode::OK);
        assert_eq!(
            second_page
                .headers()
                .get(HEADER_HAS_MORE)
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );
        assert!(
            second_page.headers().get(HEADER_NEXT_CURSOR).is_none(),
            "final page should not include next cursor"
        );

        let second_page_body = to_bytes(second_page.into_body(), usize::MAX)
            .await
            .expect("response body should load");
        let second_page_payload: Value =
            serde_json::from_slice(&second_page_body).expect("response should be valid json");
        assert_eq!(
            second_page_payload["data"]
                .as_array()
                .expect("workflows list payload should be an array")
                .len(),
            15
        );
    }

    #[tokio::test]
    async fn daemon_status_etag_returns_not_modified_for_matching_if_none_match() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let app = build_test_app(hub, true, 50, 200);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/daemon/status")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let etag = first_response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("daemon status response should include etag")
            .to_string();

        let not_modified = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/daemon/status")
                    .header(IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(not_modified.status(), axum::http::StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn tasks_list_etag_changes_when_task_data_changes() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 1).await;
        let app = build_test_app(hub.clone(), true, 50, 200);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        let initial_etag = first_response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("tasks list should include etag")
            .to_string();

        let cached_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .header(IF_NONE_MATCH, initial_etag.as_str())
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            cached_response.status(),
            axum::http::StatusCode::NOT_MODIFIED
        );

        seed_tasks(&hub, 1).await;

        let refreshed_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .header(IF_NONE_MATCH, initial_etag.as_str())
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(refreshed_response.status(), axum::http::StatusCode::OK);
        let refreshed_etag = refreshed_response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("tasks list should include refreshed etag");
        assert_ne!(refreshed_etag, initial_etag);
    }

    #[tokio::test]
    async fn tasks_list_conditional_etag_accounts_for_pagination_metadata() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 1).await;
        let app = build_test_app(hub, true, 50, 200);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?page_size=200")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let initial_etag = first_response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("tasks list should include etag")
            .to_string();
        assert_eq!(
            first_response
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("200")
        );

        let second_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks?page_size=50")
                    .header(IF_NONE_MATCH, initial_etag.as_str())
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(second_response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            second_response
                .headers()
                .get(HEADER_PAGE_SIZE)
                .and_then(|value| value.to_str().ok()),
            Some("50")
        );
        assert_eq!(
            second_response
                .headers()
                .get(HEADER_TOTAL_COUNT)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );

        let refreshed_etag = second_response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("tasks list should include refreshed etag");
        assert_ne!(
            refreshed_etag, initial_etag,
            "etag should vary across different pagination metadata"
        );
    }

    #[tokio::test]
    async fn api_supports_gzip_compression() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 3).await;
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .header("accept-encoding", "gzip")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
    }

    #[tokio::test]
    async fn api_supports_brotli_compression() {
        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        seed_tasks(&hub, 3).await;
        let app = build_test_app(hub, true, 50, 200);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/tasks")
                    .header("accept-encoding", "br")
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("br")
        );
    }
}
