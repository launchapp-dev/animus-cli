use chrono::Utc;
use orchestrator_core::{EpicItem, EpicStatus, RequirementPriority};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    parsing::{normalize_optional_string, normalize_string_list, parse_json_body},
    WebApiError, WebApiService,
};

#[derive(Debug, Deserialize)]
struct EpicCreateRequest {
    #[serde(default)]
    id: Option<String>,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    linked_requirement_ids: Vec<String>,
    #[serde(default)]
    linked_task_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct EpicPatchRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    linked_requirement_ids: Option<Vec<String>>,
    #[serde(default)]
    linked_task_ids: Option<Vec<String>>,
}

impl WebApiService {
    pub async fn epics_list(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.planning().list_epics().await?))
    }

    pub async fn epics_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.planning().get_epic(id).await?))
    }

    pub async fn epics_create(&self, body: Value) -> Result<Value, WebApiError> {
        let request: EpicCreateRequest = parse_json_body(body)?;
        let now = Utc::now();
        let epic = EpicItem {
            id: normalize_optional_string(request.id).unwrap_or_default(),
            title: request.title.trim().to_string(),
            description: request.description.unwrap_or_default(),
            priority: parse_requirement_priority_opt(request.priority.as_deref())?
                .unwrap_or(RequirementPriority::Should),
            status: parse_epic_status_opt(request.status.as_deref())?.unwrap_or(EpicStatus::Draft),
            source: normalize_optional_string(request.source)
                .unwrap_or_else(|| "ao-web".to_string()),
            tags: normalize_string_list(request.tags),
            linked_requirement_ids: normalize_string_list(request.linked_requirement_ids),
            linked_task_ids: normalize_string_list(request.linked_task_ids),
            created_at: now,
            updated_at: now,
        };
        let created = self.context.hub.planning().upsert_epic(epic).await?;
        self.publish_event("epic-create", json!({ "epic_id": created.id }));
        Ok(json!(created))
    }

    pub async fn epics_patch(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: EpicPatchRequest = parse_json_body(body)?;
        let mut epic = self.context.hub.planning().get_epic(id).await?;

        if let Some(title) = request.title {
            let title = title.trim().to_string();
            if title.is_empty() {
                return Err(WebApiError::new(
                    "invalid_input",
                    "epic title must be non-empty when provided",
                    2,
                ));
            }
            epic.title = title;
        }
        if let Some(description) = request.description {
            epic.description = description;
        }
        if let Some(priority) = request.priority {
            epic.priority = parse_requirement_priority_opt(Some(priority.as_str()))?
                .unwrap_or(RequirementPriority::Should);
        }
        if let Some(status) = request.status {
            epic.status =
                parse_epic_status_opt(Some(status.as_str()))?.unwrap_or(EpicStatus::Draft);
        }
        if let Some(source) = request.source {
            epic.source =
                normalize_optional_string(Some(source)).unwrap_or_else(|| "ao-web".to_string());
        }
        if let Some(tags) = request.tags {
            epic.tags = normalize_string_list(tags);
        }
        if let Some(linked_requirement_ids) = request.linked_requirement_ids {
            epic.linked_requirement_ids = normalize_string_list(linked_requirement_ids);
        }
        if let Some(linked_task_ids) = request.linked_task_ids {
            epic.linked_task_ids = normalize_string_list(linked_task_ids);
        }

        let updated = self.context.hub.planning().upsert_epic(epic).await?;
        self.publish_event("epic-update", json!({ "epic_id": updated.id }));
        Ok(json!(updated))
    }

    pub async fn epics_delete(&self, id: &str) -> Result<Value, WebApiError> {
        self.context.hub.planning().delete_epic(id).await?;
        self.publish_event("epic-delete", json!({ "epic_id": id }));
        Ok(json!({ "message": "epic deleted", "id": id }))
    }
}

fn parse_epic_status_opt(value: Option<&str>) -> Result<Option<EpicStatus>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| WebApiError::new("invalid_input", format!("invalid epic status: {value}"), 2))
}

fn parse_requirement_priority_opt(
    value: Option<&str>,
) -> Result<Option<RequirementPriority>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid requirement priority: {value}"),
                2,
            ))
        }
    };
    Ok(Some(parsed))
}
