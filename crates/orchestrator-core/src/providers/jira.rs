use std::collections::HashMap;
use std::env;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::providers::TaskProvider;
use crate::types::{
    Assignee, DependencyType, OrchestratorTask, Priority, TaskCreateInput, TaskFilter,
    TaskStatus, TaskStatistics, TaskType, TaskUpdateInput,
};

#[derive(Debug, Clone)]
pub struct JiraConfig {
    pub base_url: String,
    pub project_key: String,
    pub api_token_env: String,
    pub email: String,
    pub status_mapping: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct JiraTaskProvider {
    config: JiraConfig,
    client: reqwest::Client,
}

#[cfg(feature = "jira")]
impl JiraTaskProvider {
    pub fn new(config: JiraConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn base_api_url(&self) -> String {
        let trimmed = self.config.base_url.trim_end_matches('/');
        format!("{trimmed}/rest/api/3")
    }

    fn issue_url(&self, issue_id: &str) -> String {
        format!("{}/issue/{}", self.base_api_url(), issue_id)
    }

    fn search_url(&self) -> String {
        format!("{}/search", self.base_api_url())
    }

    fn status_name_to_jira_status(&self, status: TaskStatus) -> String {
        self.config
            .status_mapping
            .get(&status.to_string())
            .cloned()
            .unwrap_or_else(|| status.to_string())
    }

    fn auth_token(&self) -> Result<String> {
        env::var(&self.config.api_token_env).with_context(|| {
            format!(
                "Missing Jira API token environment variable: {}",
                &self.config.api_token_env
            )
        })
    }

    async fn send_and_decode<T: for<'a> Deserialize<'a>>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<T> {
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to execute request for {operation}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unable to read error body>".to_string());
            return Err(anyhow!(
                "{operation} failed with status {status}: {body}"
            ));
        }

        response
            .json::<T>()
            .await
            .with_context(|| format!("failed to parse {operation} response"))
    }

    async fn send_and_expect_empty(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<()> {
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to execute request for {operation}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unable to read error body>".to_string());
            return Err(anyhow!(
                "{operation} failed with status {status}: {body}"
            ));
        }
        Ok(())
    }

    fn authorized_get(&self, endpoint: &str) -> Result<reqwest::RequestBuilder> {
        let token = self.auth_token()?;
        Ok(self
            .client
            .get(endpoint)
            .basic_auth(self.config.email.clone(), Some(token)))
    }

    fn authorized_put(&self, endpoint: &str) -> Result<reqwest::RequestBuilder> {
        let token = self.auth_token()?;
        Ok(self
            .client
            .put(endpoint)
            .basic_auth(self.config.email.clone(), Some(token)))
    }

    fn authorized_post(&self, endpoint: &str) -> Result<reqwest::RequestBuilder> {
        let token = self.auth_token()?;
        Ok(self
            .client
            .post(endpoint)
            .basic_auth(self.config.email.clone(), Some(token)))
    }

    fn authorized_delete(&self, endpoint: &str) -> Result<reqwest::RequestBuilder> {
        let token = self.auth_token()?;
        Ok(self
            .client
            .delete(endpoint)
            .basic_auth(self.config.email.clone(), Some(token)))
    }

    fn map_to_task(&self, issue: JiraIssue) -> OrchestratorTask {
        let fields = issue.fields;
        let summary = fields.summary.unwrap_or_default();
        let description = jira_text_to_plain(&fields.description);
        let status = jira_status_to_task_status(fields.status.as_ref());
        let assignee = fields
            .assignee
            .as_ref()
            .and_then(jira_assignee_to_ao)
            .unwrap_or(Assignee::Unassigned);
        let tags = fields.labels.unwrap_or_default();
        let task_type = infer_task_type(&summary, &tags);
        let created_at = fields
            .created
            .as_deref()
            .and_then(parse_jira_timestamp)
            .unwrap_or_else(Utc::now);
        let updated_at = fields
            .updated
            .as_deref()
            .and_then(parse_jira_timestamp)
            .unwrap_or_else(Utc::now);
        let created_by = fields
            .creator
            .as_ref()
            .and_then(|creator| {
                creator
                    .email_address
                    .clone()
                    .or_else(|| creator.display_name.clone())
            })
            .or_else(|| {
                fields.reporter.as_ref().and_then(|reporter| {
                    reporter.email_address.clone().or_else(|| reporter.display_name.clone())
                })
            })
            .unwrap_or_else(|| "jira".to_string());

        OrchestratorTask {
            id: issue.key,
            title: summary,
            description,
            task_type,
            status,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: fields
                .priority
                .as_ref()
                .and_then(|priority| jira_priority_to_ao(&priority.name))
                .unwrap_or(Priority::Medium),
            risk: Default::default(),
            scope: Default::default(),
            complexity: Default::default(),
            impact_area: vec![],
            assignee,
            estimated_effort: None,
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
            dependencies: vec![],
            checklist: vec![],
            tags,
            workflow_metadata: Default::default(),
            worktree_path: None,
            branch_name: None,
            metadata: crate::types::TaskMetadata {
                created_at,
                updated_at,
                created_by: created_by.clone(),
                updated_by: created_by,
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: Default::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: vec![],
        }
    }

    fn build_description_field(&self, description: &str) -> Value {
        let trimmed = description.trim();
        if trimmed.is_empty() {
            return json!({
                "type": "doc",
                "version": 1,
                "content": [],
            });
        }

        json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": trimmed,
                }],
            }],
        })
    }

    async fn resolve_transition_id(
        &self,
        issue_id: &str,
        target: &str,
    ) -> Result<Option<String>> {
        let request = self
            .authorized_get(&format!("{}/transitions", self.issue_url(issue_id)))?;
        let request = request.query(&[("expand", "transitions.names")]);
        let response: JiraTransitionsResponse = self
            .send_and_decode(request, "list issue transitions")
            .await?;

        let found = response
            .transitions
            .into_iter()
            .find(|transition| {
                transition.id == target
                    || transition.name.eq_ignore_ascii_case(target)
                    || transition.to.name.eq_ignore_ascii_case(target)
            })
            .map(|transition| transition.id);

        Ok(found)
    }

    async fn unsupported<T>(&self, operation: &str) -> Result<T> {
        Err(anyhow!("Jira provider does not support operation: {operation}"))
    }

    fn filter_task(task: &OrchestratorTask, filter: &TaskFilter) -> bool {
        if let Some(task_type) = filter.task_type {
            if task.task_type != task_type {
                return false;
            }
        }

        if let Some(status) = filter.status {
            if task.status != status {
                return false;
            }
        }

        if let Some(priority) = filter.priority {
            if task.priority != priority {
                return false;
            }
        }

        if let Some(assignee_type) = &filter.assignee_type {
            let allowed = match assignee_type.as_str() {
                "agent" => matches!(task.assignee, Assignee::Agent { .. }),
                "human" => matches!(task.assignee, Assignee::Human { .. }),
                "unassigned" => matches!(task.assignee, Assignee::Unassigned),
                _ => true,
            };
            if !allowed {
                return false;
            }
        }

        if let Some(search_text) = &filter.search_text {
            let haystack = format!("{} {}", task.title, task.description).to_lowercase();
            let needle = search_text.to_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }

        if let Some(tags) = &filter.tags {
            if !tags.iter().all(|tag| task.tags.contains(tag)) {
                return false;
            }
        }

        if let Some(linked_requirement) = &filter.linked_requirement {
            if !task.linked_requirements.contains(linked_requirement) {
                return false;
            }
        }

        if let Some(linked_architecture_entity) = &filter.linked_architecture_entity {
            if !task
                .linked_architecture_entities
                .contains(linked_architecture_entity)
            {
                return false;
            }
        }

        true
    }
}

#[cfg(feature = "jira")]
#[async_trait]
impl TaskProvider for JiraTaskProvider {
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        let mut query = HashMap::new();
        query.insert("jql", format!("project={}", self.config.project_key));
        query.insert(
            "fields",
            "summary,description,status,assignee,labels,priority,created,updated,creator,reporter".to_string(),
        );

        let request = self
            .authorized_get(&self.search_url())?
            .query(&query);

        let response: JiraSearchResponse = self
            .send_and_decode(request, "list jira issues")
            .await?;

        Ok(response
            .issues
            .into_iter()
            .map(|issue| self.map_to_task(issue))
            .collect())
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        let issues = self.list().await?;
        Ok(issues
            .into_iter()
            .filter(|task| Self::filter_task(task, &filter))
            .collect())
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        let mut tasks = self.list().await?;
        tasks.sort_by_key(|task| match task.priority {
            Priority::Critical => 0,
            Priority::High => 1,
            Priority::Medium => 2,
            Priority::Low => 3,
        });
        Ok(tasks)
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        let prioritized = self.list_prioritized().await?;
        if let Some(task) = prioritized
            .iter()
            .find(|task| matches!(task.status, TaskStatus::Backlog | TaskStatus::Ready))
            .cloned()
        {
            return Ok(Some(task));
        }

        Ok(prioritized
            .into_iter()
            .find(|task| task.status != TaskStatus::Done && task.status != TaskStatus::Cancelled))
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        let tasks = self.list().await?;
        let mut by_status = HashMap::new();
        let mut by_priority = HashMap::new();
        let mut by_type = HashMap::new();
        let mut in_progress = 0;
        let mut blocked = 0;
        let mut completed = 0;

        for task in tasks.iter() {
            *by_status.entry(task.status.to_string()).or_insert(0) += 1;
            *by_priority.entry(task.priority.as_str().to_string()).or_insert(0) += 1;
            *by_type.entry(task.task_type.as_str().to_string()).or_insert(0) += 1;

            if task.status == TaskStatus::InProgress {
                in_progress += 1;
            }
            if task.status == TaskStatus::Blocked || task.status == TaskStatus::OnHold {
                blocked += 1;
            }
            if task.status == TaskStatus::Done {
                completed += 1;
            }
        }

        Ok(TaskStatistics {
            total: by_status.values().sum(),
            by_status,
            by_priority,
            by_type,
            in_progress,
            blocked,
            completed,
        })
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        let mut query = HashMap::new();
        query.insert(
            "fields",
            "summary,description,status,assignee,labels,priority,created,updated,creator,reporter"
                .to_string(),
        );

        let request = self
            .authorized_get(&self.issue_url(id))?
            .query(&query);

        let issue: JiraIssue = self.send_and_decode(request, "get jira issue").await?;
        Ok(self.map_to_task(issue))
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        let description = self.build_description_field(&input.description);
        let request = self
            .authorized_post(&self.issue_url(""))?
            .json(&JiraCreateRequest {
                fields: JiraCreateFields {
                    summary: input.title,
                    description,
                    project: JiraProjectRef {
                        key: self.config.project_key.clone(),
                    },
                    issue_type: JiraIssueTypeRef {
                        name: "Task".to_string(),
                    },
                    labels: if input.tags.is_empty() {
                        None
                    } else {
                        Some(input.tags.clone())
                    },
                    priority: input.priority.map(priority_to_jira_name).map(|name| JiraPriorityRef {
                        name,
                    }),
                },
            });

        let created: JiraCreateResponse = self
            .send_and_decode(request, "create jira issue")
            .await?;
        self.get(&created.key).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let has_fields = input.title.is_some()
            || input.description.is_some()
            || input.priority.is_some()
            || input.tags.is_some();

        if has_fields {
            let request = self.authorized_put(&self.issue_url(id))?;
            let update_fields = JiraUpdateFields {
                summary: input.title,
                description: input
                    .description
                    .as_ref()
                    .map(|description| self.build_description_field(description)),
                assignee: None,
                priority: input.priority.map(priority_to_jira_name).map(|name| JiraPriorityRef {
                    name,
                }),
                labels: input.tags,
            };
            let request = request.json(&JiraUpdateRequest {
                fields: update_fields,
            });
            self.send_and_expect_empty(request, "update jira issue").await?;
        }

        if let Some(assignee) = input.assignee {
            self.assign(id, assignee).await?;
        }

        if let Some(status) = input.status {
            let _ = self.set_status(id, status).await?;
        }

        self.get(id).await
    }

    async fn replace(&self, _task: OrchestratorTask) -> Result<OrchestratorTask> {
        self.unsupported("replace").await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let request = self.authorized_delete(&self.issue_url(id))?;
        self.send_and_expect_empty(request, "delete jira issue").await
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        let request = self.authorized_put(&format!("{}/assignee", self.issue_url(id)))?;
        let request = request.json(&json!({
            "accountId": assignee,
        }));
        self.send_and_expect_empty(request, "assign jira issue").await?;
        self.get(id).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<OrchestratorTask> {
        let target_status = self.status_name_to_jira_status(status);
        let transition_id = self
            .resolve_transition_id(id, &target_status)
            .await?
            .context("could not find matching Jira transition for requested status")?;

        let request = self.authorized_post(&format!("{}/transitions", self.issue_url(id)))?;
        let request = request.json(&json!({
            "transition": {
                "id": transition_id
            }
        }));

        self.send_and_expect_empty(request, "set jira issue status").await?;
        self.get(id).await
    }

    async fn add_checklist_item(
        &self,
        _id: &str,
        _description: String,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.unsupported("add_checklist_item").await
    }

    async fn update_checklist_item(
        &self,
        _id: &str,
        _item_id: &str,
        _completed: bool,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.unsupported("update_checklist_item").await
    }

    async fn add_dependency(
        &self,
        _id: &str,
        _dependency_id: &str,
        _dependency_type: DependencyType,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.unsupported("add_dependency").await
    }

    async fn remove_dependency(
        &self,
        _id: &str,
        _dependency_id: &str,
        _updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.unsupported("remove_dependency").await
    }
}

#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    issues: Vec<JiraIssue>,
    #[serde(default)]
    total: usize,
}

#[derive(Debug, Deserialize)]
struct JiraIssue {
    #[serde(default)]
    id: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    fields: JiraIssueFields,
}

#[derive(Debug, Deserialize, Default)]
struct JiraIssueFields {
    summary: Option<String>,
    description: Option<Value>,
    status: Option<JiraStatus>,
    assignee: Option<JiraUser>,
    creator: Option<JiraUser>,
    reporter: Option<JiraUser>,
    labels: Option<Vec<String>>,
    priority: Option<JiraPriority>,
    created: Option<String>,
    updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraPriority {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraUser {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionsResponse {
    transitions: Vec<JiraTransition>,
}

#[derive(Debug, Deserialize)]
struct JiraTransition {
    id: String,
    name: String,
    to: JiraTransitionTarget,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionTarget {
    name: String,
}

#[derive(Debug, Serialize)]
struct JiraCreateRequest {
    fields: JiraCreateFields,
}

#[derive(Debug, Serialize)]
struct JiraCreateFields {
    summary: String,
    description: Value,
    project: JiraProjectRef,
    #[serde(rename = "issuetype")]
    issue_type: JiraIssueTypeRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<JiraPriorityRef>,
}

#[derive(Debug, Serialize)]
struct JiraProjectRef {
    key: String,
}

#[derive(Debug, Serialize)]
struct JiraIssueTypeRef {
    name: String,
}

#[derive(Debug, Serialize)]
struct JiraPriorityRef {
    name: String,
}

#[derive(Debug, Serialize)]
struct JiraUpdateRequest {
    fields: JiraUpdateFields,
}

#[derive(Debug, Serialize)]
struct JiraUpdateFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee: Option<JiraAssignee>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<JiraPriorityRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct JiraAssignee {
    #[serde(skip_serializing_if = "Option::is_none", rename = "accountId")]
    account_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JiraCreateResponse {
    id: String,
    key: String,
}

fn jira_text_to_plain(input: &Option<Value>) -> String {
    fn walk(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(text);
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, out);
                }
            }
            Value::Object(object) => {
                if let Some(text) = object.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
                if let Some(content) = object.get("content").and_then(Value::as_array) {
                    for value in content {
                        walk(value, out);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = String::new();
    if let Some(value) = input {
        walk(value, &mut out);
    }
    out.trim().to_string()
}

fn parse_jira_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn jira_status_to_task_status(status: Option<&JiraStatus>) -> TaskStatus {
    let status_name = status
        .map(|value| value.name.as_str())
        .unwrap_or("backlog")
        .to_lowercase();

    TaskStatus::from_str(&status_name).unwrap_or(TaskStatus::Backlog)
}

fn jira_priority_to_ao(name: &str) -> Option<Priority> {
    match name.to_lowercase().as_str() {
        "critical" | "highest" => Some(Priority::Critical),
        "high" => Some(Priority::High),
        "medium" => Some(Priority::Medium),
        "low" => Some(Priority::Low),
        _ => None,
    }
}

fn priority_to_jira_name(priority: Priority) -> String {
    match priority {
        Priority::Critical => "Highest".to_string(),
        Priority::High => "High".to_string(),
        Priority::Medium => "Medium".to_string(),
        Priority::Low => "Low".to_string(),
    }
}

fn jira_assignee_to_ao(user: &JiraUser) -> Option<Assignee> {
    user.email_address
        .as_ref()
        .or(user.display_name.as_ref())
        .map(|value| Assignee::Human {
            user_id: value.clone(),
        })
}

fn infer_task_type(summary: &str, tags: &[String]) -> TaskType {
    let text = format!("{summary} {}", tags.join(" ")).to_lowercase();
    if text.contains("bug") || text.contains("fix") {
        return TaskType::Bugfix;
    }
    if text.contains("hotfix") {
        return TaskType::Hotfix;
    }
    if text.contains("refactor") {
        return TaskType::Refactor;
    }
    if text.contains("doc") || text.contains("docs") {
        return TaskType::Docs;
    }
    if text.contains("test") {
        return TaskType::Test;
    }
    if text.contains("chore") {
        return TaskType::Chore;
    }
    if text.contains("experiment") {
        TaskType::Experiment
    } else {
        TaskType::Feature
    }
}
