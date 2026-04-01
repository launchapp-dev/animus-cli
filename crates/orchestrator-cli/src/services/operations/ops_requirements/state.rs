use crate::cli_types::{RequirementCreateArgs, RequirementUpdateArgs};
use crate::{invalid_input_error, not_found_error, parse_input_json_or, COMMAND_HELP_HINT};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    delete_requirement as sqlite_delete_requirement, save_requirement as sqlite_save_requirement, RequirementItem,
    RequirementPriority, RequirementStatus, RequirementType,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequirementCreateInputCli {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(rename = "type", default)]
    requirement_type: Option<String>,
    #[serde(default)]
    priority: Option<RequirementPriority>,
    #[serde(default)]
    status: Option<RequirementStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    linked_task_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RequirementUpdateInputCli {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    category: Option<String>,
    #[serde(rename = "type", default)]
    requirement_type: Option<String>,
    #[serde(default)]
    priority: Option<RequirementPriority>,
    #[serde(default)]
    status: Option<RequirementStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    linked_task_ids: Option<Vec<String>>,
}

// Re-export atomic write utilities from orchestrator-core to avoid duplication
pub(super) use orchestrator_core::{project_state_dir, read_json_or_default, write_json_pretty};

fn core_state_path(project_root: &str) -> PathBuf {
    let root = Path::new(project_root);
    let base = protocol::scoped_state_root(root).unwrap_or_else(|| root.join(".ao"));
    base.join("core-state.json")
}

fn load_core_state_value(project_root: &str) -> Result<Value> {
    let path = core_state_path(project_root);
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let content = fs::read_to_string(&path)?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse JSON at {}; file is likely corrupt", path.display()))
}

fn save_core_state_value(project_root: &str, state: &Value) -> Result<()> {
    let path = core_state_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

pub(super) fn load_requirements_map_from_core_state(project_root: &str) -> Result<HashMap<String, RequirementItem>> {
    let state = load_core_state_value(project_root)?;
    let requirements_value = state.get("requirements").cloned().unwrap_or_else(|| serde_json::json!({}));
    let mut requirements: HashMap<String, RequirementItem> =
        serde_json::from_value(requirements_value).unwrap_or_default();
    if requirements.is_empty() {
        let generated_requirements = load_requirements_map_from_generated_docs(project_root)?;
        if !generated_requirements.is_empty() {
            requirements = generated_requirements;
        }
    }
    Ok(requirements)
}

pub(super) fn save_requirements_map_to_core_state(
    project_root: &str,
    requirements: &HashMap<String, RequirementItem>,
) -> Result<()> {
    let mut state = load_core_state_value(project_root)?;
    let state_obj = state.as_object_mut().ok_or_else(|| anyhow!("invalid core state shape"))?;
    state_obj.insert("requirements".to_string(), serde_json::to_value(requirements)?);
    save_core_state_value(project_root, &state)?;
    write_requirements_docs(project_root, requirements)?;
    write_generated_requirement_docs(project_root, requirements)?;
    Ok(())
}

fn write_requirements_docs(project_root: &str, requirements: &HashMap<String, RequirementItem>) -> Result<()> {
    let root = Path::new(project_root);
    let base = protocol::scoped_state_root(root).unwrap_or_else(|| root.join(".ao"));
    let docs_dir = base.join("docs");
    fs::create_dir_all(&docs_dir)?;
    let mut items: Vec<_> = requirements.values().cloned().collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    fs::write(docs_dir.join("requirements.json"), serde_json::to_string_pretty(&items)?)?;
    Ok(())
}

fn generated_requirements_dir(project_root: &str) -> PathBuf {
    let root = Path::new(project_root);
    let base = protocol::scoped_state_root(root).unwrap_or_else(|| root.join(".ao"));
    base.join("requirements").join("generated")
}

fn load_generated_requirement(path: &Path) -> Result<RequirementItem> {
    let content = fs::read_to_string(path)?;
    let mut payload: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse generated requirement JSON at {}", path.display()))?;

    if let Some(object) = payload.as_object_mut() {
        if object.get("description").is_some_and(serde_json::Value::is_null) {
            object.insert("description".to_string(), Value::String(String::new()));
        }

        if !object.contains_key("linked_task_ids") {
            let linked_task_ids = object
                .get("links")
                .and_then(|value| value.get("tasks"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]));
            object.insert("linked_task_ids".to_string(), linked_task_ids);
        }

        if !object.contains_key("relative_path") {
            if let Some(id) = object.get("id").and_then(Value::as_str) {
                object.insert("relative_path".to_string(), Value::String(format!("generated/{id}.json")));
            }
        }
    }

    serde_json::from_value(payload)
        .with_context(|| format!("generated requirement at {} does not match expected schema", path.display()))
}

fn load_requirements_map_from_generated_docs(project_root: &str) -> Result<HashMap<String, RequirementItem>> {
    let dir = generated_requirements_dir(project_root);
    if !dir.exists() {
        return Ok(HashMap::new());
    }

    let mut entries: Vec<PathBuf> =
        fs::read_dir(&dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<std::result::Result<Vec<_>, _>>()?;
    entries.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    entries.sort();

    let mut requirements = HashMap::new();
    for path in entries {
        let requirement = load_generated_requirement(&path)?;
        requirements.insert(requirement.id.clone(), requirement);
    }
    Ok(requirements)
}

fn legacy_requirement_status(status: RequirementStatus) -> &'static str {
    match status {
        RequirementStatus::Draft | RequirementStatus::Refined | RequirementStatus::Planned => "draft",
        RequirementStatus::InProgress => "em-review",
        RequirementStatus::Done | RequirementStatus::Implemented => "implemented",
        RequirementStatus::PoReview => "po-review",
        RequirementStatus::EmReview => "em-review",
        RequirementStatus::NeedsRework => "needs-rework",
        RequirementStatus::Approved => "approved",
        RequirementStatus::Deprecated => "deprecated",
    }
}

fn legacy_requirement_payload(requirement: &RequirementItem) -> Value {
    let mut tasks = requirement.links.tasks.clone();
    tasks.extend(requirement.linked_task_ids.clone());
    tasks.sort();
    tasks.dedup();

    serde_json::json!({
        "id": requirement.id,
        "title": requirement.title,
        "description": if requirement.description.trim().is_empty() { Value::Null } else { Value::String(requirement.description.clone()) },
        "legacy_id": requirement.legacy_id,
        "category": requirement.category,
        "type": requirement.requirement_type,
        "priority": requirement.priority,
        "status": legacy_requirement_status(requirement.status),
        "acceptance_criteria": requirement.acceptance_criteria,
        "tags": requirement.tags,
        "links": {
            "tasks": tasks,
            "workflows": requirement.links.workflows,
            "tests": requirement.links.tests,
            "mockups": requirement.links.mockups,
            "flows": requirement.links.flows,
            "related_requirements": requirement.links.related_requirements,
        },
        "comments": requirement.comments,
        "created_at": requirement.created_at,
        "updated_at": requirement.updated_at,
    })
}

fn write_generated_requirement_docs(project_root: &str, requirements: &HashMap<String, RequirementItem>) -> Result<()> {
    let generated_dir = generated_requirements_dir(project_root);
    fs::create_dir_all(&generated_dir)?;

    let expected_files: HashSet<String> = requirements.keys().map(|id| format!("{id}.json")).collect();
    let entries: Vec<PathBuf> = fs::read_dir(&generated_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for path in entries {
        let is_requirement_json = path.extension().is_some_and(|ext| ext == "json");
        if !is_requirement_json {
            continue;
        }

        let is_expected = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|file_name| expected_files.contains(file_name));
        if !is_expected {
            fs::remove_file(path)?;
        }
    }

    let mut items: Vec<_> = requirements.values().cloned().collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    for requirement in items {
        let file_path = generated_dir.join(format!("{}.json", requirement.id));
        let payload = legacy_requirement_payload(&requirement);
        fs::write(file_path, serde_json::to_string_pretty(&payload)?)?;
    }
    Ok(())
}

fn next_requirement_id_local(requirements: &HashMap<String, RequirementItem>) -> String {
    let next_seq = requirements
        .keys()
        .filter_map(|id| id.strip_prefix("REQ-"))
        .filter_map(|seq| seq.parse::<u32>().ok())
        .max()
        .map_or(1, |max| max.saturating_add(1));
    format!("REQ-{next_seq:03}")
}

const REQUIREMENT_PRIORITY_EXPECTED: &str = "must|should|could|wont|won't";
const REQUIREMENT_STATUS_EXPECTED: &str = "draft|refined|planned|in-progress|in_progress|done";
const REQUIREMENT_CATEGORY_EXPECTED: &str = "documentation|usability|runtime|integration|quality|release|security";
const REQUIREMENT_TYPE_EXPECTED: &str =
    "product|functional|non-functional|nonfunctional|non_functional|technical|other";

fn invalid_requirement_value_error(domain: &str, value: &str, expected: &str) -> anyhow::Error {
    let value = value.trim();
    let normalized_value = if value.is_empty() { "<empty>" } else { value };
    invalid_input_error(format!(
        "invalid requirement {domain} '{normalized_value}'; expected one of: {expected}; {COMMAND_HELP_HINT}"
    ))
}

fn parse_requirement_priority(value: &str) -> Result<RequirementPriority> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => return Err(invalid_requirement_value_error("priority", value, REQUIREMENT_PRIORITY_EXPECTED)),
    };
    Ok(parsed)
}

fn parse_requirement_type(value: &str) -> Result<RequirementType> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "product" => RequirementType::Product,
        "functional" => RequirementType::Functional,
        "non-functional" | "nonfunctional" | "non_functional" => RequirementType::NonFunctional,
        "technical" => RequirementType::Technical,
        "other" => RequirementType::Other,
        _ => return Err(invalid_requirement_value_error("type", value, REQUIREMENT_TYPE_EXPECTED)),
    };
    Ok(parsed)
}

fn parse_requirement_category(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let parsed = match normalized.as_str() {
        "documentation" | "usability" | "runtime" | "integration" | "quality" | "release" | "security" => normalized,
        _ => return Err(invalid_requirement_value_error("category", value, REQUIREMENT_CATEGORY_EXPECTED)),
    };
    Ok(parsed)
}

fn parse_requirement_status(value: &str) -> Result<RequirementStatus> {
    value.parse().map_err(|_| invalid_requirement_value_error("status", value, REQUIREMENT_STATUS_EXPECTED))
}

fn parse_requirement_priority_opt(value: Option<&str>) -> Result<Option<RequirementPriority>> {
    match value {
        Some(value) => Ok(Some(parse_requirement_priority(value)?)),
        None => Ok(None),
    }
}

fn parse_requirement_status_opt(value: Option<&str>) -> Result<Option<RequirementStatus>> {
    match value {
        Some(value) => Ok(Some(parse_requirement_status(value)?)),
        None => Ok(None),
    }
}

fn parse_requirement_type_opt(value: Option<&str>) -> Result<Option<RequirementType>> {
    match value {
        Some(value) => Ok(Some(parse_requirement_type(value)?)),
        None => Ok(None),
    }
}

fn parse_requirement_category_opt(value: Option<&str>) -> Result<Option<String>> {
    match value {
        Some(value) => Ok(Some(parse_requirement_category(value)?)),
        None => Ok(None),
    }
}

pub(super) fn create_requirement_cli(project_root: &str, args: RequirementCreateArgs) -> Result<RequirementItem> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(RequirementCreateInputCli {
            title: args.title,
            description: args.description.unwrap_or_default(),
            acceptance_criteria: args.acceptance_criterion,
            category: args.category,
            requirement_type: args.requirement_type,
            priority: parse_requirement_priority_opt(args.priority.as_deref())?,
            status: None,
            source: args.source,
            linked_task_ids: Vec::new(),
        })
    })?;

    if input.title.trim().is_empty() {
        return Err(invalid_input_error("requirement title is required"));
    }

    let category = parse_requirement_category_opt(input.category.as_deref())?;
    let requirement_type = parse_requirement_type_opt(input.requirement_type.as_deref())?;

    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    let id = next_requirement_id_local(&requirements);
    let now = Utc::now();
    let requirement = RequirementItem {
        id: id.clone(),
        title: input.title,
        description: input.description,
        body: None,
        legacy_id: None,
        category,
        requirement_type,
        acceptance_criteria: input.acceptance_criteria,
        priority: input.priority.unwrap_or(RequirementPriority::Should),
        status: input.status.unwrap_or(RequirementStatus::Draft),
        source: input.source.unwrap_or_else(|| "manual".to_string()),
        tags: Vec::new(),
        links: Default::default(),
        comments: Vec::new(),
        relative_path: None,
        linked_task_ids: input.linked_task_ids,
        created_at: now,
        updated_at: now,
    };

    requirements.insert(id, requirement.clone());
    save_requirements_map_to_core_state(project_root, &requirements)?;
    let _ = sqlite_save_requirement(Path::new(project_root), &requirement);
    Ok(requirement)
}

pub(super) fn update_requirement_cli(project_root: &str, args: RequirementUpdateArgs) -> Result<RequirementItem> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(RequirementUpdateInputCli {
            title: args.title,
            description: args.description,
            acceptance_criteria: if args.acceptance_criterion.is_empty() {
                None
            } else {
                Some(args.acceptance_criterion)
            },
            category: args.category,
            requirement_type: args.requirement_type,
            priority: parse_requirement_priority_opt(args.priority.as_deref())?,
            status: parse_requirement_status_opt(args.status.as_deref())?,
            source: args.source,
            linked_task_ids: if args.linked_task_id.is_empty() { None } else { Some(args.linked_task_id) },
        })
    })?;

    let category = parse_requirement_category_opt(input.category.as_deref())?;
    let requirement_type = parse_requirement_type_opt(input.requirement_type.as_deref())?;

    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    let requirement =
        requirements.get_mut(&args.id).ok_or_else(|| not_found_error(format!("requirement not found: {}", args.id)))?;

    if let Some(title) = input.title {
        requirement.title = title;
    }
    if let Some(description) = input.description {
        requirement.description = description;
    }

    if let Some(criteria) = input.acceptance_criteria {
        if args.replace_acceptance_criteria {
            requirement.acceptance_criteria = criteria;
        } else {
            for criterion in criteria {
                if !requirement.acceptance_criteria.iter().any(|existing| existing == &criterion) {
                    requirement.acceptance_criteria.push(criterion);
                }
            }
        }
    }
    if let Some(priority) = input.priority {
        requirement.priority = priority;
    }
    if let Some(status) = input.status {
        requirement.status = status;
    }
    if let Some(category) = category {
        requirement.category = Some(category);
    }
    if let Some(requirement_type) = requirement_type {
        requirement.requirement_type = Some(requirement_type);
    }
    if let Some(source) = input.source {
        requirement.source = source;
    }
    if let Some(linked_task_ids) = input.linked_task_ids {
        requirement.linked_task_ids = linked_task_ids;
    }
    requirement.updated_at = Utc::now();

    let updated = requirement.clone();
    save_requirements_map_to_core_state(project_root, &requirements)?;
    let _ = sqlite_save_requirement(Path::new(project_root), &updated);
    Ok(updated)
}

pub(super) fn delete_requirement_cli(project_root: &str, id: &str) -> Result<()> {
    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    if requirements.remove(id).is_none() {
        return Err(not_found_error(format!("requirement not found: {id}")));
    }
    save_requirements_map_to_core_state(project_root, &requirements)?;
    let _ = sqlite_delete_requirement(Path::new(project_root), id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{classify_cli_error_kind, CliErrorKind};

    use protocol::test_utils::EnvVarGuard;

    fn test_requirement_item(id: &str) -> RequirementItem {
        let now = Utc::now();
        RequirementItem {
            id: id.to_string(),
            title: format!("Requirement {id}"),
            description: "requirement description".to_string(),
            body: None,
            legacy_id: None,
            category: Some("quality".to_string()),
            requirement_type: Some(RequirementType::Technical),
            acceptance_criteria: vec!["criterion".to_string()],
            priority: RequirementPriority::Must,
            status: RequirementStatus::Done,
            source: "test".to_string(),
            tags: Vec::new(),
            links: Default::default(),
            comments: Vec::new(),
            relative_path: Some(format!("generated/{id}.json")),
            linked_task_ids: vec!["TASK-001".to_string()],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn parse_requirement_priority_reports_canonical_help_hint() {
        let error = parse_requirement_priority("urgent").expect_err("invalid priority should fail");
        let message = error.to_string();
        assert!(message.contains("invalid requirement priority"));
        assert!(message.contains(REQUIREMENT_PRIORITY_EXPECTED));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_requirement_status_reports_canonical_help_hint() {
        let error = parse_requirement_status("queued").expect_err("invalid status should fail");
        let message = error.to_string();
        assert!(message.contains("invalid requirement status"));
        assert!(message.contains(REQUIREMENT_STATUS_EXPECTED));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_requirement_category_reports_canonical_help_hint() {
        let error = parse_requirement_category("platform").expect_err("invalid category should fail");
        let message = error.to_string();
        assert!(message.contains("invalid requirement category"));
        assert!(message.contains(REQUIREMENT_CATEGORY_EXPECTED));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_requirement_type_reports_canonical_help_hint() {
        let error = parse_requirement_type("business").expect_err("invalid type should fail");
        let message = error.to_string();
        assert!(message.contains("invalid requirement type"));
        assert!(message.contains(REQUIREMENT_TYPE_EXPECTED));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn load_requirements_map_falls_back_to_generated_docs_when_core_state_is_empty() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("create project dir");
        let project_root = project_root.to_str().expect("project root str");
        let generated_dir = generated_requirements_dir(project_root);
        fs::create_dir_all(&generated_dir).expect("create generated requirements dir");

        let now = Utc::now();
        let payload = serde_json::json!({
            "id": "REQ-007",
            "title": "Enforce Rust-only dependency guardrails",
            "description": "Prevent desktop-wrapper drift.",
            "legacy_id": null,
            "category": "security",
            "type": "technical",
            "priority": "must",
            "status": "implemented",
            "acceptance_criteria": ["CI blocks prohibited dependencies."],
            "tags": [],
            "links": {
                "tasks": ["TASK-007"],
                "workflows": [],
                "tests": [],
                "mockups": [],
                "flows": [],
                "related_requirements": []
            },
            "comments": [],
            "created_at": now,
            "updated_at": now
        });
        fs::write(
            generated_dir.join("REQ-007.json"),
            serde_json::to_string_pretty(&payload).expect("serialize requirement payload"),
        )
        .expect("write generated requirement");

        let requirements =
            load_requirements_map_from_core_state(project_root).expect("load requirements from generated docs");
        let requirement = requirements.get("REQ-007").expect("requirement should load from generated docs");

        assert_eq!(requirement.category.as_deref(), Some("security"));
        assert_eq!(requirement.requirement_type, Some(RequirementType::Technical));
        assert_eq!(requirement.linked_task_ids, vec!["TASK-007".to_string()]);
    }

    #[test]
    fn requirement_parse_errors_are_typed_as_invalid_input() {
        let error = parse_requirement_priority("urgent").expect_err("invalid priority should fail");
        assert_eq!(classify_cli_error_kind(&error), CliErrorKind::InvalidInput);
    }

    #[test]
    fn write_generated_requirement_docs_prunes_stale_requirement_files() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("create project dir");
        let project_root = project_root.to_str().expect("project root");
        let generated_dir = generated_requirements_dir(project_root);
        fs::create_dir_all(&generated_dir).expect("create generated requirements dir");

        fs::write(
            generated_dir.join("REQ-999.json"),
            serde_json::json!({
                "id": "REQ-999",
                "title": "stale requirement"
            })
            .to_string(),
        )
        .expect("seed stale generated requirement");

        let mut requirements = HashMap::new();
        requirements.insert("REQ-001".to_string(), test_requirement_item("REQ-001"));

        write_generated_requirement_docs(project_root, &requirements).expect("write generated requirement docs");

        assert!(generated_dir.join("REQ-001.json").exists());
        assert!(!generated_dir.join("REQ-999.json").exists());
    }
}
