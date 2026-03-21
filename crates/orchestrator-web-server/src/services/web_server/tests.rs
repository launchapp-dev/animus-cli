use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::Request;
use axum::response::Response;
use axum::Router;
use orchestrator_core::{InMemoryServiceHub, Priority, ServiceHub, TaskCreateInput, TaskStatus, TaskType};
use orchestrator_web_api::WebApiContext;
use serde_json::Value;
use tower::util::ServiceExt;

use super::{build_router, AppState, HEADER_HAS_MORE, HEADER_NEXT_CURSOR, HEADER_PAGE_SIZE, HEADER_TOTAL_COUNT};

fn test_metrics_handle() -> metrics_exporter_prometheus::PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder().handle()
}

fn build_test_app(hub: Arc<dyn ServiceHub>, api_only: bool, default_page_size: usize, max_page_size: usize) -> Router {
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
        metrics_handle: test_metrics_handle(),
    })
}

async fn seed_tasks(hub: &Arc<dyn ServiceHub>, count: usize) {
    let base_index = hub.tasks().list().await.expect("tasks should list for seeding").len();
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

async fn seed_workflows(app: &Router, task_ids: &[String]) {
    for (index, task_id) in task_ids.iter().enumerate() {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/workflows/run")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "task_id": task_id
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

async fn response_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    serde_json::from_slice(&body).expect("response should be valid json")
}

async fn response_body_text(response: Response) -> String {
    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    String::from_utf8_lossy(&body).to_string()
}

fn response_header(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|value| value.to_str().ok()).map(ToOwned::to_owned)
}

fn data_ids(payload: &Value) -> Vec<String> {
    payload["data"]
        .as_array()
        .expect("response payload should contain data array")
        .iter()
        .map(|item| item["id"].as_str().expect("array item should contain id").to_string())
        .collect()
}

#[test]
fn tasks_list_query_deserializes_positive_page_size() {
    let uri: axum::http::Uri = "/api/v1/tasks?page_size=200".parse().expect("uri should parse");
    let query = axum::extract::Query::<super::TasksListQuery>::try_from_uri(&uri)
        .expect("query extraction should parse page_size")
        .0;
    assert_eq!(query.pagination.page_size.as_deref(), Some("200"));
    assert_eq!(query.pagination.cursor, None);
}

#[test]
fn tasks_list_query_deserializes_full_filter_set() {
    let uri: axum::http::Uri = "/api/v1/tasks?task_type=feature&status=ready&priority=high&tag=api&linked_requirement=REQ-100&search=critical&sort=updated_at&page_size=1"
            .parse()
            .expect("uri should parse");
    let query = axum::extract::Query::<super::TasksListQuery>::try_from_uri(&uri)
        .expect("query extraction should parse full task filter set")
        .0;
    assert_eq!(query.task_type.as_deref(), Some("feature"));
    assert_eq!(query.status.as_deref(), Some("ready"));
    assert_eq!(query.priority.as_deref(), Some("high"));
    assert_eq!(query.tag, vec!["api".to_string()]);
    assert_eq!(query.linked_requirement.as_deref(), Some("REQ-100"));
    assert_eq!(query.search.as_deref(), Some("critical"));
    assert_eq!(query.sort.as_deref(), Some("updated_at"));
    assert_eq!(query.pagination.page_size.as_deref(), Some("1"));
}

#[test]
fn requirements_list_query_deserializes_full_filter_set() {
    let uri: axum::http::Uri = "/api/v1/requirements?status=draft&priority=must&category=runtime&type=technical&tag=backend&linked_task_id=TASK-123&search=query&sort=updated_at&page_size=1"
            .parse()
            .expect("uri should parse");
    let query = axum::extract::Query::<super::RequirementsListQuery>::try_from_uri(&uri)
        .expect("query extraction should parse full requirement filter set")
        .0;
    assert_eq!(query.status.as_deref(), Some("draft"));
    assert_eq!(query.priority.as_deref(), Some("must"));
    assert_eq!(query.category.as_deref(), Some("runtime"));
    assert_eq!(query.requirement_type.as_deref(), Some("technical"));
    assert_eq!(query.tag, vec!["backend".to_string()]);
    assert_eq!(query.linked_task_id.as_deref(), Some("TASK-123"));
    assert_eq!(query.search.as_deref(), Some("query"));
    assert_eq!(query.sort.as_deref(), Some("updated_at"));
    assert_eq!(query.pagination.page_size.as_deref(), Some("1"));
}

#[tokio::test]
async fn requirements_list_filters_by_status_priority_and_category() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    for requirement in [
        serde_json::json!({
            "title": "Draft requirement",
            "description": "baseline",
            "status": "draft",
            "priority": "should",
            "category": "ops"
        }),
        serde_json::json!({
            "title": "API requirement",
            "description": "target",
            "status": "in-progress",
            "priority": "must",
            "category": "api"
        }),
        serde_json::json!({
            "title": "Other in-progress requirement",
            "description": "noise",
            "status": "in-progress",
            "priority": "could",
            "category": "api"
        }),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/requirements")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(requirement.to_string()))
                    .expect("request should be built"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/requirements?status=in-progress&priority=must&category=api")
                .body(Body::empty())
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");
    let items = payload["data"].as_array().expect("requirements payload should be an array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], Value::String("API requirement".to_string()));
}

#[tokio::test]
async fn graphql_tasks_paginated_uses_shared_query_page_metadata() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    seed_tasks(&hub, 3).await;
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "query": "{ tasksPaginated(limit: 2, offset: 1) { totalCount limit offset returned hasMore nextOffset items { id } } }"
                        })
                        .to_string(),
                    ))
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");
    assert_eq!(payload["data"]["tasksPaginated"]["totalCount"], Value::from(3));
    assert_eq!(
        payload["data"]["tasksPaginated"]["items"].as_array().expect("graphql tasks page should include items").len(),
        2
    );
    assert_eq!(payload["data"]["tasksPaginated"]["limit"], Value::from(2));
    assert_eq!(payload["data"]["tasksPaginated"]["offset"], Value::from(1));
    assert_eq!(payload["data"]["tasksPaginated"]["returned"], Value::from(2));
    assert_eq!(payload["data"]["tasksPaginated"]["hasMore"], Value::from(false));
    assert_eq!(payload["data"]["tasksPaginated"]["nextOffset"], Value::Null);
}

#[tokio::test]
async fn tasks_rest_and_graphql_share_filter_sort_and_pagination_semantics() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub.clone(), true, 50, 200);

    let noise = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Noise task".to_string(),
            description: "does not match".to_string(),
            task_type: Some(TaskType::Bugfix),
            priority: Some(Priority::High),
            created_by: Some("test".to_string()),
            tags: vec!["ops".to_string()],
            linked_requirements: vec!["REQ-999".to_string()],
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("noise task should be created");
    hub.tasks().set_status(&noise.id, TaskStatus::Ready, false).await.expect("noise task should be ready");

    let first = hub
        .tasks()
        .create(TaskCreateInput {
            title: "API query parity one".to_string(),
            description: "critical path".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("test".to_string()),
            tags: vec!["api".to_string()],
            linked_requirements: vec!["REQ-100".to_string()],
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("first task should be created");
    hub.tasks().set_status(&first.id, TaskStatus::Ready, false).await.expect("first task should be ready");

    let second = hub
        .tasks()
        .create(TaskCreateInput {
            title: "API query parity two".to_string(),
            description: "critical path".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("test".to_string()),
            tags: vec!["api".to_string()],
            linked_requirements: vec!["REQ-100".to_string()],
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("second task should be created");
    hub.tasks().set_status(&second.id, TaskStatus::Ready, false).await.expect("second task should be ready");

    let rest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(
                        "/api/v1/tasks?task_type=feature&status=ready&priority=high&tag=api&linked_requirement=REQ-100&search=critical&sort=updated_at&page_size=1",
                    )
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("rest request should succeed");
    let rest_status = rest_response.status();
    let rest_total = response_header(&rest_response, HEADER_TOTAL_COUNT);
    let rest_page_size = response_header(&rest_response, HEADER_PAGE_SIZE);
    let rest_has_more = response_header(&rest_response, HEADER_HAS_MORE);
    let rest_next_cursor = response_header(&rest_response, HEADER_NEXT_CURSOR);
    let rest_body = response_body_text(rest_response).await;
    assert_eq!(rest_status, axum::http::StatusCode::OK, "rest task parity query should succeed: {rest_body}");
    let rest_payload: Value = serde_json::from_str(&rest_body).expect("rest task parity body should be valid json");
    let rest_total = rest_total.expect("rest total count header");
    let rest_page_size = rest_page_size.expect("rest page size header");
    let rest_has_more = rest_has_more.expect("rest has more header");
    let rest_next_cursor = rest_next_cursor.expect("rest next cursor header");

    let graphql_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "query": "{ tasksPaginated(taskType: \"feature\", status: \"ready\", priority: \"high\", tags: [\"api\"], linkedRequirement: \"REQ-100\", search: \"critical\", sort: \"updated_at\", limit: 1, offset: 0) { totalCount limit offset returned hasMore nextOffset items { id } } }"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("graphql request should succeed");
    let graphql_status = graphql_response.status();
    let graphql_payload = response_json(graphql_response).await;
    assert_eq!(
        graphql_status,
        axum::http::StatusCode::OK,
        "graphql task parity query should succeed: {graphql_payload}"
    );

    assert_eq!(data_ids(&rest_payload), vec![second.id.clone()]);
    assert_eq!(
        graphql_payload["data"]["tasksPaginated"]["items"]
            .as_array()
            .expect("graphql items should be present")
            .iter()
            .map(|item| item["id"].as_str().expect("graphql item id").to_string())
            .collect::<Vec<_>>(),
        vec![second.id]
    );
    assert_eq!(rest_total, graphql_payload["data"]["tasksPaginated"]["totalCount"].to_string());
    assert_eq!(rest_page_size, graphql_payload["data"]["tasksPaginated"]["limit"].to_string());
    assert_eq!(rest_has_more, graphql_payload["data"]["tasksPaginated"]["hasMore"].to_string());
    assert_eq!(rest_next_cursor, graphql_payload["data"]["tasksPaginated"]["nextOffset"].to_string());
}

#[tokio::test]
async fn requirements_rest_and_graphql_share_filter_sort_and_pagination_semantics() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    for requirement in [
        serde_json::json!({
            "title": "Noise requirement",
            "description": "ignore me",
            "status": "draft",
            "priority": "must",
            "category": "ops",
            "requirement_type": "technical",
            "tags": ["ops"],
            "linked_task_ids": ["TASK-999"]
        }),
        serde_json::json!({
            "title": "Requirements query parity one",
            "description": "shared query behavior",
            "status": "draft",
            "priority": "must",
            "category": "runtime",
            "requirement_type": "technical",
            "tags": ["backend"],
            "linked_task_ids": ["TASK-123"]
        }),
        serde_json::json!({
            "title": "Requirements query parity two",
            "description": "shared query behavior",
            "status": "draft",
            "priority": "must",
            "category": "runtime",
            "requirement_type": "technical",
            "tags": ["backend"],
            "linked_task_ids": ["TASK-123"]
        }),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/requirements")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(requirement.to_string()))
                    .expect("request should be built"),
            )
            .await
            .expect("seed request should succeed");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    let rest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(
                        "/api/v1/requirements?status=draft&priority=must&category=runtime&type=technical&tag=backend&linked_task_id=TASK-123&search=query&sort=updated_at&page_size=1",
                    )
                    .body(Body::empty())
                    .expect("request should be built"),
            )
            .await
            .expect("rest request should succeed");
    let rest_status = rest_response.status();
    let rest_total = response_header(&rest_response, HEADER_TOTAL_COUNT);
    let rest_page_size = response_header(&rest_response, HEADER_PAGE_SIZE);
    let rest_has_more = response_header(&rest_response, HEADER_HAS_MORE);
    let rest_next_cursor = response_header(&rest_response, HEADER_NEXT_CURSOR);
    let rest_body = response_body_text(rest_response).await;
    assert_eq!(rest_status, axum::http::StatusCode::OK, "rest requirement parity query should succeed: {rest_body}");
    let rest_payload: Value =
        serde_json::from_str(&rest_body).expect("rest requirement parity body should be valid json");
    let rest_total = rest_total.expect("rest total count header");
    let rest_page_size = rest_page_size.expect("rest page size header");
    let rest_has_more = rest_has_more.expect("rest has more header");
    let rest_next_cursor = rest_next_cursor.expect("rest next cursor header");
    let rest_ids = data_ids(&rest_payload);

    let graphql_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "query": "{ requirementsPaginated(status: \"draft\", priority: \"must\", category: \"runtime\", requirementType: \"technical\", tags: [\"backend\"], linkedTaskId: \"TASK-123\", search: \"query\", sort: \"updated_at\", limit: 1, offset: 0) { totalCount limit offset returned hasMore nextOffset items { id } } }"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("graphql request should succeed");
    let graphql_status = graphql_response.status();
    let graphql_payload = response_json(graphql_response).await;
    assert_eq!(
        graphql_status,
        axum::http::StatusCode::OK,
        "graphql requirement parity query should succeed: {graphql_payload}"
    );
    let graphql_ids = graphql_payload["data"]["requirementsPaginated"]["items"]
        .as_array()
        .expect("graphql items should be present")
        .iter()
        .map(|item| item["id"].as_str().expect("graphql item id").to_string())
        .collect::<Vec<_>>();

    assert_eq!(rest_ids, graphql_ids);
    assert_eq!(rest_total, graphql_payload["data"]["requirementsPaginated"]["totalCount"].to_string());
    assert_eq!(rest_page_size, graphql_payload["data"]["requirementsPaginated"]["limit"].to_string());
    assert_eq!(rest_has_more, graphql_payload["data"]["requirementsPaginated"]["hasMore"].to_string());
    assert_eq!(rest_next_cursor, graphql_payload["data"]["requirementsPaginated"]["nextOffset"].to_string());
}

#[tokio::test]
async fn workflows_rest_and_graphql_share_filter_sort_and_pagination_semantics() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub.clone(), true, 50, 200);

    let task_a = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Workflow parity A".to_string(),
            description: "seed".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("test".to_string()),
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("task a should be created");
    let task_b = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Workflow parity B".to_string(),
            description: "seed".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("test".to_string()),
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("task b should be created");
    let task_c = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Workflow noise".to_string(),
            description: "seed".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("test".to_string()),
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("task c should be created");

    let mut workflow_ids = Vec::new();
    for task_id in [task_a.id.clone(), task_b.id.clone(), task_c.id.clone()] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/workflows/run")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "task_id": task_id,
                            "workflow_ref": "query-parity"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("workflow run should succeed");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let payload = response_json(response).await;
        workflow_ids.push(payload["data"]["id"].as_str().expect("workflow response should include id").to_string());
    }

    hub.workflows().pause(&workflow_ids[0]).await.expect("first workflow should pause");
    hub.workflows().pause(&workflow_ids[1]).await.expect("second workflow should pause");
    hub.workflows().cancel(&workflow_ids[2]).await.expect("third workflow should cancel");

    let rest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(
                    "/api/v1/workflows?status=paused&workflow_ref=query-parity&search=query-parity&sort=id&page_size=1",
                )
                .body(Body::empty())
                .expect("request should be built"),
        )
        .await
        .expect("rest request should succeed");
    assert_eq!(rest_response.status(), axum::http::StatusCode::OK);
    let rest_total = response_header(&rest_response, HEADER_TOTAL_COUNT).expect("rest total count header");
    let rest_page_size = response_header(&rest_response, HEADER_PAGE_SIZE).expect("rest page size header");
    let rest_has_more = response_header(&rest_response, HEADER_HAS_MORE).expect("rest has more header");
    let rest_next_cursor = response_header(&rest_response, HEADER_NEXT_CURSOR).expect("rest next cursor header");
    let rest_payload = response_json(rest_response).await;
    let rest_ids = data_ids(&rest_payload);

    let graphql_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "query": "{ workflowsPaginated(status: \"paused\", workflowRef: \"query-parity\", search: \"query-parity\", sort: \"id\", limit: 1, offset: 0) { totalCount limit offset returned hasMore nextOffset items { id } } }"
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("graphql request should succeed");
    assert_eq!(graphql_response.status(), axum::http::StatusCode::OK);
    let graphql_payload = response_json(graphql_response).await;
    let graphql_ids = graphql_payload["data"]["workflowsPaginated"]["items"]
        .as_array()
        .expect("graphql items should be present")
        .iter()
        .map(|item| item["id"].as_str().expect("graphql item id").to_string())
        .collect::<Vec<_>>();

    assert_eq!(rest_ids, graphql_ids);
    assert_eq!(rest_total, graphql_payload["data"]["workflowsPaginated"]["totalCount"].to_string());
    assert_eq!(rest_page_size, graphql_payload["data"]["workflowsPaginated"]["limit"].to_string());
    assert_eq!(rest_has_more, graphql_payload["data"]["workflowsPaginated"]["hasMore"].to_string());
    assert_eq!(rest_next_cursor, graphql_payload["data"]["workflowsPaginated"]["nextOffset"].to_string());
}

#[tokio::test]
async fn graphql_task_nested_requirements_resolves_linked_requirement_ids() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub.clone(), true, 50, 200);

    let requirement_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/requirements")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Nested requirement parity",
                        "description": "resolve linked requirement ids"
                    })
                    .to_string(),
                ))
                .expect("request should be built"),
        )
        .await
        .expect("requirement request should succeed");
    assert_eq!(requirement_response.status(), axum::http::StatusCode::OK);
    let requirement_payload = response_json(requirement_response).await;
    let requirement_id =
        requirement_payload["data"]["id"].as_str().expect("requirement response should include id").to_string();

    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Task with linked requirement".to_string(),
            description: "nested graphql".to_string(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("test".to_string()),
            tags: Vec::new(),
            linked_requirements: vec![requirement_id.clone()],
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("task should be created");

    let graphql_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "query": format!("{{ task(id: \"{}\") {{ linkedRequirementIds requirements {{ id }} }} }}", task.id)
                        })
                        .to_string(),
                    ))
                    .expect("request should be built"),
            )
            .await
            .expect("graphql request should succeed");
    assert_eq!(graphql_response.status(), axum::http::StatusCode::OK);
    let graphql_payload = response_json(graphql_response).await;

    assert_eq!(
        graphql_payload["data"]["task"]["linkedRequirementIds"],
        Value::Array(vec![Value::String(requirement_id.clone())])
    );
    assert_eq!(graphql_payload["data"]["task"]["requirements"][0]["id"], Value::String(requirement_id));
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
    let payload: Value = serde_json::from_slice(&body).expect("openapi endpoint should return valid JSON");
    assert_eq!(payload["openapi"].as_str(), Some("3.1.0"), "spec should declare OpenAPI 3.1");
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
        });

    let response = app
        .oneshot(
            Request::builder().method("GET").uri("/api/v1/docs").body(Body::empty()).expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let content_type = response.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or_default();
    assert!(content_type.starts_with("text/html"), "docs endpoint should return HTML");

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
    let html = String::from_utf8(body.to_vec()).expect("docs response should be utf-8");
    assert!(html.contains("SwaggerUIBundle"), "docs response should include Swagger UI bootstrap");
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(payload.get("data").and_then(|data| data.get("status")).and_then(Value::as_str), Some("failed"));
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
    assert_eq!(requirement_create_response.status(), axum::http::StatusCode::OK);

    let requirement_create_body =
        to_bytes(requirement_create_response.into_body(), usize::MAX).await.expect("response body should load");
    let requirement_create_payload: Value =
        serde_json::from_slice(&requirement_create_body).expect("response should be valid json");
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
    assert_eq!(requirement_patch_response.status(), axum::http::StatusCode::OK);

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
    assert_eq!(requirement_refine_response.status(), axum::http::StatusCode::OK);

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
    assert_eq!(requirement_delete_response.status(), axum::http::StatusCode::OK);
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: false,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
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
            .oneshot(Request::builder().method("GET").uri(route).body(Body::empty()).expect("request should be built"))
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK, "{route} should return SPA html");

        let content_type =
            response.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or_default();
        assert!(content_type.starts_with("text/html"), "{route} should return text/html content type");
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
    let app =
        build_router(AppState {
            api,
            assets_dir: None,
            api_only: true,
            default_page_size: 50,
            max_page_size: 200,
            metrics_handle: test_metrics_handle(),
        });

    let response = app
        .oneshot(Request::builder().method("GET").uri("/events").body(Body::empty()).expect("request should be built"))
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
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
            Request::builder().method("GET").uri("/api/v1/tasks").body(Body::empty()).expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(first_page.status(), axum::http::StatusCode::OK);
    assert_eq!(first_page.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("50"));
    assert_eq!(first_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("true"));
    let next_cursor = first_page
        .headers()
        .get(HEADER_NEXT_CURSOR)
        .and_then(|value| value.to_str().ok())
        .expect("first page should return next cursor")
        .to_string();

    let first_page_body = to_bytes(first_page.into_body(), usize::MAX).await.expect("response body should load");
    let first_page_payload: Value = serde_json::from_slice(&first_page_body).expect("response should be valid json");
    assert_eq!(first_page_payload["data"].as_array().expect("tasks list payload should be an array").len(), 50);

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
    assert_eq!(second_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("false"));
    assert!(
        second_page.headers().get(HEADER_NEXT_CURSOR).is_none(),
        "final page should not include a next cursor header"
    );

    let second_page_body = to_bytes(second_page.into_body(), usize::MAX).await.expect("response body should load");
    let second_page_payload: Value = serde_json::from_slice(&second_page_body).expect("response should be valid json");
    assert_eq!(second_page_payload["data"].as_array().expect("tasks list payload should be an array").len(), 10);
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
    assert_eq!(response.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("30"));
    assert_eq!(response.headers().get(HEADER_TOTAL_COUNT).and_then(|value| value.to_str().ok()), Some("45"));
    assert_eq!(response.headers().get(HEADER_NEXT_CURSOR).and_then(|value| value.to_str().ok()), Some("30"));

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");
    assert_eq!(payload["data"].as_array().expect("tasks list payload should be an array").len(), 30);
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
    assert_eq!(response.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("50"));
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
    assert_eq!(first_page.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("30"));
    assert_eq!(first_page.headers().get(HEADER_TOTAL_COUNT).and_then(|value| value.to_str().ok()), Some("45"));
    assert_eq!(first_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("true"));
    let next_cursor = first_page
        .headers()
        .get(HEADER_NEXT_CURSOR)
        .and_then(|value| value.to_str().ok())
        .expect("first page should include next cursor")
        .to_string();

    let first_page_body = to_bytes(first_page.into_body(), usize::MAX).await.expect("response body should load");
    let first_page_payload: Value = serde_json::from_slice(&first_page_body).expect("response should be valid json");
    assert_eq!(first_page_payload["data"].as_array().expect("requirements list payload should be an array").len(), 30);

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
    assert_eq!(second_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("false"));
    assert!(second_page.headers().get(HEADER_NEXT_CURSOR).is_none(), "final page should not include next cursor");

    let second_page_body = to_bytes(second_page.into_body(), usize::MAX).await.expect("response body should load");
    let second_page_payload: Value = serde_json::from_slice(&second_page_body).expect("response should be valid json");
    assert_eq!(second_page_payload["data"].as_array().expect("requirements list payload should be an array").len(), 15);
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
    assert_eq!(response.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("50"));
}

#[tokio::test]
async fn workflows_list_supports_cursor_pagination_with_page_size_cap() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    seed_tasks(&hub, 45).await;
    let task_ids = hub
        .tasks()
        .list()
        .await
        .expect("tasks should list after seeding")
        .into_iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let app = build_test_app(hub, true, 25, 30);
    seed_workflows(&app, &task_ids).await;

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
    assert_eq!(first_page.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("30"));
    assert_eq!(first_page.headers().get(HEADER_TOTAL_COUNT).and_then(|value| value.to_str().ok()), Some("45"));
    assert_eq!(first_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("true"));
    let next_cursor = first_page
        .headers()
        .get(HEADER_NEXT_CURSOR)
        .and_then(|value| value.to_str().ok())
        .expect("first page should include next cursor")
        .to_string();

    let first_page_body = to_bytes(first_page.into_body(), usize::MAX).await.expect("response body should load");
    let first_page_payload: Value = serde_json::from_slice(&first_page_body).expect("response should be valid json");
    assert_eq!(first_page_payload["data"].as_array().expect("workflows list payload should be an array").len(), 30);

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
    assert_eq!(second_page.headers().get(HEADER_HAS_MORE).and_then(|value| value.to_str().ok()), Some("false"));
    assert!(second_page.headers().get(HEADER_NEXT_CURSOR).is_none(), "final page should not include next cursor");

    let second_page_body = to_bytes(second_page.into_body(), usize::MAX).await.expect("response body should load");
    let second_page_payload: Value = serde_json::from_slice(&second_page_body).expect("response should be valid json");
    assert_eq!(second_page_payload["data"].as_array().expect("workflows list payload should be an array").len(), 15);
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
            Request::builder().method("GET").uri("/api/v1/tasks").body(Body::empty()).expect("request should be built"),
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
    assert_eq!(cached_response.status(), axum::http::StatusCode::NOT_MODIFIED);

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
    assert_eq!(first_response.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("200"));

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
    assert_eq!(second_response.headers().get(HEADER_PAGE_SIZE).and_then(|value| value.to_str().ok()), Some("50"));
    assert_eq!(second_response.headers().get(HEADER_TOTAL_COUNT).and_then(|value| value.to_str().ok()), Some("1"));

    let refreshed_etag = second_response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .expect("tasks list should include refreshed etag");
    assert_ne!(refreshed_etag, initial_etag, "etag should vary across different pagination metadata");
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
    assert_eq!(response.headers().get(CONTENT_ENCODING).and_then(|value| value.to_str().ok()), Some("gzip"));
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
    assert_eq!(response.headers().get(CONTENT_ENCODING).and_then(|value| value.to_str().ok()), Some("br"));
}

#[tokio::test]
async fn queue_list_endpoint_returns_empty_queue_when_no_state() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder().method("GET").uri("/api/v1/queue").body(Body::empty()).expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let data = payload.get("data").expect("data should exist");
    assert_eq!(data.get("entries").and_then(Value::as_array).map(Vec::len), Some(0));
    let stats = data.get("stats").expect("stats should exist");
    assert_eq!(stats.get("total").and_then(Value::as_u64), Some(0));
}

#[tokio::test]
async fn queue_stats_endpoint_returns_zeros_when_no_state() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/queue/stats")
                .body(Body::empty())
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let data = payload.get("data").expect("data should exist");
    assert_eq!(data.get("depth").and_then(Value::as_u64), Some(0));
    assert_eq!(data.get("pending").and_then(Value::as_u64), Some(0));
    assert_eq!(data.get("throughput_last_hour").and_then(Value::as_u64), Some(0));
}

#[tokio::test]
async fn queue_reorder_endpoint_accepts_valid_request() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/queue/reorder")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "task_ids": ["TASK-001", "TASK-002"]
                    })
                    .to_string(),
                ))
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let data = payload.get("data").expect("data should exist");
    assert_eq!(data.get("reordered").and_then(Value::as_bool), Some(false));
}

#[tokio::test]
async fn queue_hold_endpoint_accepts_valid_request() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/queue/hold/TASK-001")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({}).to_string()))
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let data = payload.get("data").expect("data should exist");
    assert_eq!(data.get("held").and_then(Value::as_bool), Some(false));
    assert_eq!(data.get("task_id").and_then(Value::as_str), Some("TASK-001"));
}

#[tokio::test]
async fn queue_release_endpoint_accepts_valid_request() {
    let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
    let app = build_test_app(hub, true, 50, 200);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/queue/release/TASK-001")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({}).to_string()))
                .expect("request should be built"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.expect("response body should load");
    let payload: Value = serde_json::from_slice(&body).expect("response should be valid json");

    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let data = payload.get("data").expect("data should exist");
    assert_eq!(data.get("released").and_then(Value::as_bool), Some(false));
    assert_eq!(data.get("task_id").and_then(Value::as_str), Some("TASK-001"));
}
