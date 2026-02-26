use crate::cli_types::{RequirementCreateArgs, RequirementUpdateArgs};
use crate::{parse_input_json_or, COMMAND_HELP_HINT};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{RequirementItem, RequirementPriority, RequirementStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequirementCreateInputCli {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
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
    priority: Option<RequirementPriority>,
    #[serde(default)]
    status: Option<RequirementStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    linked_task_ids: Option<Vec<String>>,
}

pub(super) fn project_state_dir(project_root: &str) -> PathBuf {
    Path::new(project_root).join(".ao").join("state")
}

pub(super) fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse JSON at {}; file is likely corrupt",
            path.display()
        )
    })
}

pub(super) fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state.json");
    let tmp_path = path.with_file_name(format!("{file_name}.{}.tmp", Uuid::new_v4()));
    let payload = serde_json::to_string_pretty(value)?;

    fs::write(&tmp_path, payload)?;
    match fs::rename(&tmp_path, path) {
        Ok(()) => {}
        Err(original_error) => {
            if path.exists() {
                fs::remove_file(path).with_context(|| {
                    format!("failed to replace {} after rename failure", path.display())
                })?;
                fs::rename(&tmp_path, path).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                })?;
            } else {
                return Err(original_error).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                });
            }
        }
    }

    Ok(())
}

fn core_state_path(project_root: &str) -> PathBuf {
    Path::new(project_root).join(".ao").join("core-state.json")
}

fn load_core_state_value(project_root: &str) -> Result<Value> {
    let path = core_state_path(project_root);
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let content = fs::read_to_string(&path)?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse JSON at {}; file is likely corrupt",
            path.display()
        )
    })
}

fn save_core_state_value(project_root: &str, state: &Value) -> Result<()> {
    let path = core_state_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

pub(super) fn load_requirements_map_from_core_state(
    project_root: &str,
) -> Result<HashMap<String, RequirementItem>> {
    let state = load_core_state_value(project_root)?;
    let requirements = state
        .get("requirements")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::from_value(requirements).unwrap_or_default())
}

pub(super) fn save_requirements_map_to_core_state(
    project_root: &str,
    requirements: &HashMap<String, RequirementItem>,
) -> Result<()> {
    let mut state = load_core_state_value(project_root)?;
    let state_obj = state
        .as_object_mut()
        .ok_or_else(|| anyhow!("invalid core state shape"))?;
    state_obj.insert(
        "requirements".to_string(),
        serde_json::to_value(requirements)?,
    );
    save_core_state_value(project_root, &state)?;
    write_requirements_docs(project_root, requirements)?;
    Ok(())
}

fn write_requirements_docs(
    project_root: &str,
    requirements: &HashMap<String, RequirementItem>,
) -> Result<()> {
    let docs_dir = Path::new(project_root).join(".ao").join("docs");
    fs::create_dir_all(&docs_dir)?;
    let mut items: Vec<_> = requirements.values().cloned().collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    fs::write(
        docs_dir.join("requirements.json"),
        serde_json::to_string_pretty(&items)?,
    )?;
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

fn invalid_requirement_value_error(domain: &str, value: &str, expected: &str) -> anyhow::Error {
    let value = value.trim();
    let normalized_value = if value.is_empty() { "<empty>" } else { value };
    anyhow!(
        "invalid requirement {domain} '{normalized_value}'; expected one of: {expected}; {COMMAND_HELP_HINT}"
    )
}

fn parse_requirement_priority(value: &str) -> Result<RequirementPriority> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => {
            return Err(invalid_requirement_value_error(
                "priority",
                value,
                REQUIREMENT_PRIORITY_EXPECTED,
            ))
        }
    };
    Ok(parsed)
}

fn parse_requirement_status(value: &str) -> Result<RequirementStatus> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "draft" => RequirementStatus::Draft,
        "refined" => RequirementStatus::Refined,
        "planned" => RequirementStatus::Planned,
        "in-progress" | "in_progress" => RequirementStatus::InProgress,
        "done" => RequirementStatus::Done,
        _ => {
            return Err(invalid_requirement_value_error(
                "status",
                value,
                REQUIREMENT_STATUS_EXPECTED,
            ))
        }
    };
    Ok(parsed)
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

pub(super) fn create_requirement_cli(
    project_root: &str,
    args: RequirementCreateArgs,
) -> Result<RequirementItem> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(RequirementCreateInputCli {
            title: args.title,
            description: args.description,
            acceptance_criteria: args.acceptance_criterion,
            priority: parse_requirement_priority_opt(args.priority.as_deref())?,
            status: None,
            source: args.source,
            linked_task_ids: Vec::new(),
        })
    })?;

    if input.title.trim().is_empty() {
        anyhow::bail!("requirement title is required");
    }

    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    let id = next_requirement_id_local(&requirements);
    let now = Utc::now();
    let requirement = RequirementItem {
        id: id.clone(),
        title: input.title,
        description: input.description,
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
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
    Ok(requirement)
}

pub(super) fn update_requirement_cli(
    project_root: &str,
    args: RequirementUpdateArgs,
) -> Result<RequirementItem> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(RequirementUpdateInputCli {
            title: args.title,
            description: args.description,
            acceptance_criteria: if args.acceptance_criterion.is_empty() {
                None
            } else {
                Some(args.acceptance_criterion)
            },
            priority: parse_requirement_priority_opt(args.priority.as_deref())?,
            status: parse_requirement_status_opt(args.status.as_deref())?,
            source: args.source,
            linked_task_ids: if args.linked_task_id.is_empty() {
                None
            } else {
                Some(args.linked_task_id)
            },
        })
    })?;

    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    let requirement = requirements
        .get_mut(&args.id)
        .ok_or_else(|| anyhow!("requirement not found: {}", args.id))?;

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
                if !requirement
                    .acceptance_criteria
                    .iter()
                    .any(|existing| existing == &criterion)
                {
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
    if let Some(source) = input.source {
        requirement.source = source;
    }
    if let Some(linked_task_ids) = input.linked_task_ids {
        requirement.linked_task_ids = linked_task_ids;
    }
    requirement.updated_at = Utc::now();

    let updated = requirement.clone();
    save_requirements_map_to_core_state(project_root, &requirements)?;
    Ok(updated)
}

pub(super) fn delete_requirement_cli(project_root: &str, id: &str) -> Result<()> {
    let mut requirements = load_requirements_map_from_core_state(project_root)?;
    if requirements.remove(id).is_none() {
        anyhow::bail!("requirement not found: {id}");
    }
    save_requirements_map_to_core_state(project_root, &requirements)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
