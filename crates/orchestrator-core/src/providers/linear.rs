use std::collections::HashMap;
use std::env;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Map, Value};

use crate::providers::TaskProvider;
use crate::types::{
    Assignee, DependencyType, DispatchHistoryEntry, OrchestratorTask, Priority, ResourceRequirements,
    RiskLevel, Scope, TaskCreateInput, TaskFilter, TaskMetadata, TaskStatistics, TaskStatus,
    TaskType, TaskUpdateInput, WorkflowMetadata, Complexity,
};

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

#[derive(Debug, Clone)]
pub struct LinearConfig {
    pub api_key_env: String,
    pub team_id: String,
    pub status_mapping: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct LinearTaskProvider {
    pub config: LinearConfig,
    client: Client,
}

impl LinearTaskProvider {
    pub fn new(config: LinearConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    fn normalize_status_key(raw: &str) -> String {
        raw.trim()
            .to_ascii_lowercase()
            .replace(' ', "-")
            .replace('_', "-")
    }

    fn normalize_priority(raw: &str) -> String {
        raw.trim().to_ascii_lowercase().replace(' ', "-")
    }

    fn map_status_name(raw: &str) -> TaskStatus {
        match TaskStatus::from_str(&Self::normalize_status_key(raw)) {
            Ok(status) => status,
            Err(_) => TaskStatus::Backlog,
        }
    }

    fn map_priority(raw: Option<&Value>) -> Priority {
        match raw.and_then(|raw| {
            raw.as_str()
                .map(|value| Self::normalize_priority(value))
                .or_else(|| raw.as_i64().map(|value| value.to_string()))
        }) {
            Some(raw) => match raw.as_str() {
                "critical" | "urgent" | "high" => Priority::High,
                "medium" => Priority::Medium,
                "low" => Priority::Low,
                _ => Priority::Medium,
            },
            None => Priority::Medium,
        }
    }

    fn map_status_id(&self, status: &TaskStatus) -> Result<String> {
        let normalized = Self::normalize_status_key(&status.to_string());
        self.config
            .status_mapping
            .iter()
            .find_map(|(key, value)| {
                (Self::normalize_status_key(key) == normalized).then_some(value.clone())
            })
            .ok_or_else(|| anyhow!("no Linear state mapping configured for status: {status}"))
    }

    fn build_task_from_issue(&self, issue: LinearIssue) -> OrchestratorTask {
        let now = Utc::now();
        let assignee = issue
            .assignee
            .and_then(|assignee| assignee.name)
            .filter(|value| !value.is_empty())
            .map_or(Assignee::Unassigned, |name| Assignee::Human { user_id: name });

        OrchestratorTask {
            id: issue.id,
            title: issue.title,
            description: issue.description.unwrap_or_default(),
            task_type: TaskType::Feature,
            status: issue
                .state
                .and_then(|state| state.name)
                .as_deref()
                .map(Self::map_status_name)
                .unwrap_or(TaskStatus::Backlog),
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Self::map_priority(issue.priority.as_ref()),
            risk: RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: Complexity::Medium,
            impact_area: Vec::new(),
            assignee,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: "linear".to_string(),
                updated_by: "linear".to_string(),
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::<DispatchHistoryEntry>::new(),
        }
    }

    async fn execute_graphql<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Option<Value>,
    ) -> Result<T> {
        let api_key = env::var(&self.config.api_key_env).with_context(|| {
            format!("Linear API key env var missing: {}", self.config.api_key_env)
        })?;

        let request_body = variables.map_or_else(
            || json!({ "query": query }),
            |value| json!({ "query": query, "variables": value }),
        );

        let response: GraphqlEnvelope<T> = self
            .client
            .post(LINEAR_GRAPHQL_URL)
            .bearer_auth(api_key)
            .json(&request_body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if let Some(errors) = response.errors {
            if !errors.is_empty() {
                let message = errors
                    .into_iter()
                    .map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("Linear GraphQL error: {message}");
            }
        }

        Ok(response.data)
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlEnvelope<T> {
    data: T,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct LinearIssuesResponse {
    issues: LinearIssueConnection,
}

#[derive(Debug, Deserialize)]
struct LinearIssueConnection {
    nodes: Vec<LinearIssue>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueResponse {
    issue: LinearIssue,
}

#[derive(Debug, Deserialize)]
struct LinearIssueCreateResponse {
    #[serde(rename = "issueCreate")]
    issue_create: LinearIssueIdPayload,
}

#[derive(Debug, Deserialize)]
struct LinearIssueUpdateResponse {
    #[serde(rename = "issueUpdate")]
    issue_update: LinearIssueIdPayload,
}

#[derive(Debug, Deserialize)]
struct LinearIssueIdPayload {
    issue: LinearIssueId,
}

#[derive(Debug, Deserialize)]
struct LinearIssueId {
    id: String,
}

#[derive(Debug, Deserialize)]
struct LinearIssue {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    state: Option<LinearIssueState>,
    #[serde(default)]
    priority: Option<Value>,
    #[serde(default)]
    assignee: Option<LinearIssueAssignee>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueState {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueAssignee {
    #[serde(default)]
    name: Option<String>,
}

#[async_trait]
impl TaskProvider for LinearTaskProvider {
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        let query = r#"
            query($teamId: ID!) {
                issues(filter: { team: { id: { eq: $teamId } } }) {
                    nodes {
                        id
                        title
                        description
                        state { name }
                        priority
                        assignee { name }
                    }
                }
            }
        "#;

        let response: GraphqlEnvelope<LinearIssuesResponse> = self
            .execute_graphql(query, Some(json!({ "teamId": self.config.team_id.clone() })))
            .await?;

        let tasks = response
            .data
            .issues
            .nodes
            .into_iter()
            .map(|issue| self.build_task_from_issue(issue))
            .collect();
        Ok(tasks)
    }

    async fn list_filtered(&self, _filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        bail!("not supported by Linear provider");
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        bail!("not supported by Linear provider");
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        bail!("not supported by Linear provider");
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        bail!("not supported by Linear provider");
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        let query = r#"
            query($id: ID!) {
                issue(id: $id) {
                    id
                    title
                    description
                    state { name }
                    priority
                    assignee { name }
                }
            }
        "#;

        let response: GraphqlEnvelope<LinearIssueResponse> = self
            .execute_graphql(query, Some(json!({ "id": id })))
            .await?;

        Ok(self.build_task_from_issue(response.data.issue))
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        let query = r#"
            mutation($teamId: ID!, $title: String!, $description: String!) {
                issueCreate(input: { teamId: $teamId, title: $title, description: $description }) {
                    issue { id }
                }
            }
        "#;

        let response: GraphqlEnvelope<LinearIssueCreateResponse> = self
            .execute_graphql(
                query,
                Some(json!({
                    "teamId": self.config.team_id.clone(),
                    "title": input.title,
                    "description": input.description,
                })),
            )
            .await?;

        self.get(&response.data.issue_create.issue.id).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let mut issue_input = Map::new();

        if let Some(title) = input.title {
            issue_input.insert("title".to_string(), json!(title));
        }
        if let Some(description) = input.description {
            issue_input.insert("description".to_string(), json!(description));
        }

        let response: GraphqlEnvelope<LinearIssueUpdateResponse> = self
            .execute_graphql(
                r#"
                    mutation($id: ID!, $input: IssueUpdateInput!) {
                        issueUpdate(id: $id, input: $input) {
                            issue { id }
                        }
                    }
                "#,
                Some(json!({
                    "id": id,
                    "input": Value::Object(issue_input),
                })),
            )
            .await?;

        self.get(&response.data.issue_update.issue.id).await
    }

    async fn replace(&self, _task: OrchestratorTask) -> Result<OrchestratorTask> {
        bail!("not supported by Linear provider");
    }

    async fn delete(&self, _id: &str) -> Result<()> {
        bail!("not supported by Linear provider");
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        let response: GraphqlEnvelope<LinearIssueUpdateResponse> = self
            .execute_graphql(
                r#"
                    mutation($id: ID!, $assigneeId: ID!) {
                        issueUpdate(id: $id, input: { assigneeId: $assigneeId }) {
                            issue { id }
                        }
                    }
                "#,
                Some(json!({
                    "id": id,
                    "assigneeId": assignee,
                })),
            )
            .await?;

        self.get(&response.data.issue_update.issue.id).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<OrchestratorTask> {
        let mapped_status = self.map_status_id(&status)?;
        let response: GraphqlEnvelope<LinearIssueUpdateResponse> = self
            .execute_graphql(
                r#"
                    mutation($id: ID!, $stateId: ID!) {
                        issueUpdate(id: $id, input: { stateId: $stateId }) {
                            issue { id }
                        }
                    }
                "#,
                Some(json!({
                    "id": id,
                    "stateId": mapped_status,
                })),
            )
            .await?;

        self.get(&response.data.issue_update.issue.id).await
    }

    async fn add_checklist_item(
        &self,
        _id: &str,
        _description: String,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        bail!("not supported by Linear provider");
    }

    async fn update_checklist_item(
        &self,
        _id: &str,
        _item_id: &str,
        _completed: bool,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        bail!("not supported by Linear provider");
    }

    async fn add_dependency(
        &self,
        _id: &str,
        _dependency_id: &str,
        _dependency_type: DependencyType,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        bail!("not supported by Linear provider");
    }

    async fn remove_dependency(
        &self,
        _id: &str,
        _dependency_id: &str,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        bail!("not supported by Linear provider");
    }
}
