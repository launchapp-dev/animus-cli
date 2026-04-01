use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use orchestrator_core::{ListPage, ListPageRequest};
use orchestrator_web_api::{WebApiError, WebApiService};
use orchestrator_web_contracts::{http_status_for_exit_code, CliEnvelopeService, DaemonEventRecord};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::models::WebServerConfig;
use crate::services::docs_html::render_openapi_docs_html;
use crate::services::graphql;
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
            max_page_size: self.config.max_page_size.max(self.config.default_page_size.max(1)),
        };

        let router = build_router(state);
        let address = format!("{}:{}", self.config.host, self.config.port);
        let listener = tokio::net::TcpListener::bind(&address)
            .await
            .with_context(|| format!("failed to bind web server at {address}"))?;

        axum::serve(listener, router).await.context("web server failed")?;

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
        .route("/project-requirements/{id}", get(projects_requirements_by_id_handler))
        .route("/project-requirements/{project_id}/{requirement_id}", get(project_requirement_get_handler))
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
        .route("/tasks/{id}/checklist/{item_id}", patch(tasks_checklist_update_handler))
        .route("/tasks/{id}/dependencies", post(tasks_dependency_add_handler))
        .route("/tasks/{id}/dependencies/{dependency_id}", delete(tasks_dependency_remove_handler))
        .route("/workflows", get(workflows_list_handler))
        .route("/workflows/run", post(workflows_run_handler))
        .route("/workflows/{id}", get(workflows_get_handler))
        .route("/workflows/{id}/decisions", get(workflows_decisions_handler))
        .route("/workflows/{id}/checkpoints", get(workflows_checkpoints_handler))
        .route("/workflows/{id}/checkpoints/{checkpoint}", get(workflows_get_checkpoint_handler))
        .route("/workflows/{id}/resume", post(workflows_resume_handler))
        .route("/workflows/{id}/pause", post(workflows_pause_handler))
        .route("/workflows/{id}/cancel", post(workflows_cancel_handler))
        .route("/reviews/handoff", post(reviews_handoff_handler))
        .route("/queue", get(queue_list_handler))
        .route("/queue/stats", get(queue_stats_handler))
        .route("/queue/reorder", post(queue_reorder_handler))
        .route("/queue/hold/{id}", post(queue_hold_handler))
        .route("/queue/release/{id}", post(queue_release_handler))
        .route("/triggers/{trigger_id}", post(trigger_webhook_handler));

    let gql_schema = graphql::build_schema(state.api.clone());

    Router::new()
        .nest("/api/v1", api_router)
        .route("/graphql", get(graphql::graphql_playground).post(graphql::graphql_handler))
        .route_service("/graphql/ws", graphql::ws_subscription(gql_schema.clone()))
        .route("/graphql/schema", get(graphql::graphql_sdl_handler))
        .route("/", get(root_handler))
        .route("/{*path}", get(static_handler))
        .layer(axum::Extension(gql_schema))
        .layer(CompressionLayer::new())
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|origin, _| {
                    origin
                        .to_str()
                        .map(|o| o.starts_with("http://localhost") || o.starts_with("http://127.0.0.1"))
                        .unwrap_or(false)
                }))
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::OPTIONS])
                .allow_headers([axum::http::header::CONTENT_TYPE, axum::http::header::AUTHORIZATION])
                .max_age(Duration::from_secs(3600)),
        )
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

async fn daemon_logs_handler(State(state): State<AppState>, Query(query): Query<DaemonLogsQuery>) -> Response {
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

async fn projects_get_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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

async fn project_workflows_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.project_workflows(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_create_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.projects_create(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_load_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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

async fn projects_archive_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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
    match state.api.project_requirement_get(&project_id, &requirement_id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn projects_delete_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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
    Query(query): Query<RequirementsListQuery>,
) -> Response {
    let page = match normalize_pagination_query(&query.pagination, state.default_page_size, state.max_page_size) {
        Ok(page) => page,
        Err(error) => return error_response(error),
    };
    let query = match state.api.build_requirement_query(
        query.status,
        query.priority,
        query.category,
        query.requirement_type,
        query.tag,
        query.linked_task_id,
        query.search,
        page,
        query.sort,
    ) {
        Ok(query) => query,
        Err(error) => return error_response(error),
    };

    match state.api.requirements_list(query).await {
        Ok(data) => match paginated_success_response(data, None) {
            Ok(response) => response,
            Err(error) => error_response(error),
        },
        Err(error) => error_response(error),
    }
}

async fn requirements_create_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.requirements_create(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_draft_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.requirements_draft(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_refine_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.requirements_refine(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn requirements_get_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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

async fn requirements_delete_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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
    let page = match normalize_pagination_query(&query.pagination, state.default_page_size, state.max_page_size) {
        Ok(page) => page,
        Err(error) => return error_response(error),
    };
    let query = match state.api.build_task_query(
        query.task_type,
        query.status,
        query.priority,
        query.risk,
        query.assignee_type,
        query.tag,
        query.linked_requirement,
        query.linked_architecture_entity,
        query.search,
        page,
        query.sort,
    ) {
        Ok(query) => query,
        Err(error) => return error_response(error),
    };

    match state.api.tasks_list(query).await {
        Ok(data) => match paginated_success_response(data, Some(&headers)) {
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

async fn tasks_get_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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

async fn tasks_delete_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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
    match state.api.tasks_dependency_remove(&id, &dependency_id, body.map(|json| json.0)).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_list_handler(State(state): State<AppState>, Query(query): Query<WorkflowsListQuery>) -> Response {
    let page = match normalize_pagination_query(&query.pagination, state.default_page_size, state.max_page_size) {
        Ok(page) => page,
        Err(error) => return error_response(error),
    };
    let query = match state.api.build_workflow_query(
        query.status,
        query.workflow_ref,
        query.task_id,
        query.phase_id,
        query.search,
        page,
        query.sort,
    ) {
        Ok(query) => query,
        Err(error) => return error_response(error),
    };

    match state.api.workflows_list(query).await {
        Ok(data) => match paginated_success_response(data, None) {
            Ok(response) => response,
            Err(error) => error_response(error),
        },
        Err(error) => error_response(error),
    }
}

async fn workflows_get_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.workflows_get(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_decisions_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.workflows_decisions(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_checkpoints_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
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

async fn workflows_resume_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.workflows_resume(&id, None).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_pause_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.workflows_pause(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn workflows_cancel_handler(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.api.workflows_cancel(&id).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn reviews_handoff_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.reviews_handoff(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn queue_list_handler(State(state): State<AppState>) -> Response {
    match state.api.queue_list().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn queue_stats_handler(State(state): State<AppState>) -> Response {
    match state.api.queue_stats().await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn queue_reorder_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.api.queue_reorder(body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn queue_hold_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.queue_hold(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

async fn queue_release_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    match state.api.queue_release(&id, body).await {
        Ok(data) => success_response(data),
        Err(error) => error_response(error),
    }
}

/// Handler for `POST /api/v1/triggers/{trigger_id}`.
///
/// Accepts an inbound webhook payload and queues it for the next daemon tick.
///
/// # Request format
/// - Body: any JSON payload (forwarded as `webhook_payload` in the task input)
/// - Optional header `X-AO-Signature: sha256=<hex>` for HMAC verification
///
/// # Response
/// - **202 Accepted** — event queued successfully
/// - **404 Not Found** — trigger not found or is not a webhook type
/// - **401 Unauthorized** — HMAC signature mismatch
/// - **429 Too Many Requests** — rate limit exceeded
async fn trigger_webhook_handler(
    State(state): State<AppState>,
    AxumPath(trigger_id): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let project_root = state.api.project_root();

    // Load trigger config to check secret_env for HMAC verification.
    let config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
    let trigger = config.config.triggers.iter().find(|t| t.id.eq_ignore_ascii_case(&trigger_id));

    if let Some(trigger) = trigger {
        let wh_config = orchestrator_core::WebhookTriggerConfig::from_value(&trigger.config);

        // HMAC verification (only when secret_env is configured).
        if let Some(ref secret_env_name) = wh_config.secret_env {
            let secret = match std::env::var(secret_env_name) {
                Ok(value) => value,
                Err(_) => {
                    let envelope = orchestrator_web_contracts::CliEnvelopeService::error(
                        "configuration_error",
                        format!("signing secret env var '{}' is not set", secret_env_name),
                        1,
                    );
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(envelope)).into_response();
                }
            };

            let signature_header = headers
                .get("x-ao-signature")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            let expected_sig = compute_hmac_sha256_hex(secret.as_bytes(), &body);
            let expected_header = format!("sha256={}", expected_sig);

            if !constant_time_eq(signature_header.as_bytes(), expected_header.as_bytes()) {
                let envelope = orchestrator_web_contracts::CliEnvelopeService::error(
                    "unauthorized",
                    "webhook signature verification failed",
                    4,
                );
                return (StatusCode::UNAUTHORIZED, Json(envelope)).into_response();
            }
        }
    }

    // Parse body as JSON (or wrap raw bytes in a JSON object).
    let payload: serde_json::Value = if body.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                // Non-JSON body: store as base64-encoded string.
                serde_json::json!({ "raw": String::from_utf8_lossy(&body).as_ref() })
            }
        }
    };

    let now = chrono::Utc::now();
    match state.api.trigger_webhook_enqueue(&trigger_id, payload, now) {
        Ok(data) => (StatusCode::ACCEPTED, Json(orchestrator_web_contracts::CliEnvelopeService::ok(data))).into_response(),
        Err(error) => error_response(error),
    }
}

/// Compute HMAC-SHA256 of `data` using `key` and return the lowercase hex string.
fn compute_hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let result = mac.finalize().into_bytes();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time byte slice comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping")).into_response()
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

async fn static_handler(State(state): State<AppState>, AxumPath(path): AxumPath<String>) -> Response {
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

fn paginated_success_response<T: Serialize>(
    page: ListPage<T>,
    conditional_headers: Option<&HeaderMap>,
) -> std::result::Result<Response, WebApiError> {
    let etag = conditional_headers.map(|_| compute_etag(&page));
    let page_size = page.limit.unwrap_or(page.returned);
    let total_count = page.total;
    let next_cursor = page.next_offset.map(|offset| offset.to_string());
    let items = serde_json::to_value(page.items)
        .map_err(|error| WebApiError::new("internal", format!("failed to serialize paginated items: {error}"), 1))?
        .as_array()
        .cloned()
        .ok_or_else(|| WebApiError::new("internal", "expected paginated items to serialize as an array", 1))?;

    if let (Some(headers), Some(etag)) = (conditional_headers, etag.as_deref()) {
        if if_none_match_matches(headers, etag) {
            let mut response = not_modified_response(etag);
            attach_pagination_headers(&mut response, page_size, total_count, next_cursor.as_deref());
            return Ok(response);
        }
    }

    let mut response = success_response(Value::Array(items));
    if let Some(etag) = etag.as_deref() {
        attach_cache_headers(&mut response, etag);
    }
    attach_pagination_headers(&mut response, page_size, total_count, next_cursor.as_deref());
    Ok(response)
}

fn normalize_pagination_query(
    query: &ListPaginationQuery,
    default_page_size: usize,
    max_page_size: usize,
) -> std::result::Result<ListPageRequest, WebApiError> {
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

    Ok(ListPageRequest { limit: Some(page_size), offset: start })
}

fn parse_pagination_cursor(cursor: &str) -> std::result::Result<usize, WebApiError> {
    cursor.parse::<usize>().map_err(|_| {
        WebApiError::new("invalid_input", format!("invalid cursor `{cursor}`: expected unsigned integer"), 2)
    })
}

fn parse_page_size(page_size: &str) -> std::result::Result<usize, WebApiError> {
    let parsed = page_size.parse::<usize>().map_err(|_| {
        WebApiError::new("invalid_input", format!("invalid page_size `{page_size}`: expected unsigned integer"), 2)
    })?;

    if parsed == 0 {
        return Err(WebApiError::new("invalid_input", "page_size must be at least 1", 2));
    }

    Ok(parsed)
}

fn attach_pagination_headers(response: &mut Response, page_size: usize, total_count: usize, next_cursor: Option<&str>) {
    set_response_header(response, HEADER_PAGE_SIZE, &page_size.to_string());
    set_response_header(response, HEADER_TOTAL_COUNT, &total_count.to_string());
    set_response_header(response, HEADER_HAS_MORE, if next_cursor.is_some() { "true" } else { "false" });
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
            value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == "*" || normalize_etag(candidate) == normalized_current)
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
    response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_REVALIDATE));
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
    Event::default().event("daemon-event").id(record.seq.to_string()).data(payload)
}

fn parse_last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers.get("last-event-id").and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok())
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
    let content_type = mime_guess::from_path(&full_path).first_or_octet_stream().essence_str().to_string();

    Some(AssetPayload { bytes, content_type })
}

fn load_asset_from_embedded(requested_path: &str) -> Option<AssetPayload> {
    let file = EMBEDDED_ASSETS.get_file(requested_path)?;
    let bytes = file.contents().to_vec();
    let content_type = mime_guess::from_path(requested_path).first_or_octet_stream().essence_str().to_string();

    Some(AssetPayload { bytes, content_type })
}

fn binary_response(bytes: Vec<u8>, content_type: &str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let header_value =
        HeaderValue::from_str(content_type).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
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

fn deserialize_query_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        One(String),
        Many(Vec<String>),
    }

    let raw = Option::<StringOrVec>::deserialize(deserializer)?;
    Ok(match raw {
        Some(StringOrVec::One(value)) => vec![value],
        Some(StringOrVec::Many(values)) => values,
        None => Vec::new(),
    })
}

#[derive(Debug, Deserialize)]
struct TasksListQuery {
    task_type: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    risk: Option<String>,
    assignee_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_query_string_list")]
    tag: Vec<String>,
    linked_requirement: Option<String>,
    linked_architecture_entity: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    #[serde(flatten)]
    pagination: ListPaginationQuery,
}

#[derive(Debug, Deserialize)]
struct RequirementsListQuery {
    status: Option<String>,
    priority: Option<String>,
    category: Option<String>,
    #[serde(alias = "type")]
    requirement_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_query_string_list")]
    tag: Vec<String>,
    linked_task_id: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    #[serde(flatten)]
    pagination: ListPaginationQuery,
}

#[derive(Debug, Deserialize)]
struct WorkflowsListQuery {
    status: Option<String>,
    workflow_ref: Option<String>,
    task_id: Option<String>,
    phase_id: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    #[serde(flatten)]
    pagination: ListPaginationQuery,
}

#[cfg(test)]
mod tests;
