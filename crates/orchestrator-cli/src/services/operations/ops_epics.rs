use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, EpicItem, EpicStatus, RequirementPriority};

use crate::{
    parse_input_json_or, print_ok, print_value, EpicCommand, EpicCreateArgs, EpicUpdateArgs,
};

pub(crate) async fn handle_epics(
    command: EpicCommand,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        EpicCommand::List => print_value(planning.list_epics().await?, json),
        EpicCommand::Get(args) => print_value(planning.get_epic(&args.id).await?, json),
        EpicCommand::Create(args) => {
            let input_json = args.input_json.clone();
            let epic = parse_input_json_or(input_json, || build_epic_from_create_args(args))?;
            print_value(planning.upsert_epic(epic).await?, json)
        }
        EpicCommand::Update(args) => {
            let input_json = args.input_json.clone();
            let patch = parse_input_json_or(input_json, || build_epic_patch_args(args))?;
            let epic_id = patch.id.clone();
            let mut epic = planning.get_epic(&epic_id).await?;
            if let Some(title) = patch.title {
                epic.title = title;
            }
            if let Some(description) = patch.description {
                epic.description = description;
            }
            if let Some(priority) = patch.priority {
                epic.priority = priority;
            }
            if let Some(status) = patch.status {
                epic.status = status;
            }
            if let Some(source) = patch.source {
                epic.source = source;
            }
            if let Some(tags) = patch.tags {
                epic.tags = tags;
            }
            if let Some(linked_requirement_ids) = patch.linked_requirement_ids {
                epic.linked_requirement_ids = linked_requirement_ids;
            }
            if let Some(linked_task_ids) = patch.linked_task_ids {
                epic.linked_task_ids = linked_task_ids;
            }
            epic.updated_at = Utc::now();
            print_value(planning.upsert_epic(epic).await?, json)
        }
        EpicCommand::Delete(args) => {
            planning.delete_epic(&args.id).await?;
            print_ok("epic deleted", json);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct EpicPatchInput {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<RequirementPriority>,
    #[serde(default)]
    status: Option<EpicStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    linked_requirement_ids: Option<Vec<String>>,
    #[serde(default)]
    linked_task_ids: Option<Vec<String>>,
}

fn build_epic_from_create_args(args: EpicCreateArgs) -> Result<EpicItem> {
    Ok(EpicItem {
        id: String::new(),
        title: args.title,
        description: args.description,
        priority: parse_requirement_priority_opt(args.priority.as_deref())?
            .unwrap_or(RequirementPriority::Should),
        status: parse_epic_status_opt(args.status.as_deref())?.unwrap_or(EpicStatus::Draft),
        source: args
            .source
            .unwrap_or_else(|| protocol::ACTOR_CLI.to_string()),
        tags: args.tag,
        linked_requirement_ids: args.linked_requirement_id,
        linked_task_ids: args.linked_task_id,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
}

fn build_epic_patch_args(args: EpicUpdateArgs) -> Result<EpicPatchInput> {
    Ok(EpicPatchInput {
        id: args.id,
        title: args.title,
        description: args.description,
        priority: parse_requirement_priority_opt(args.priority.as_deref())?,
        status: parse_epic_status_opt(args.status.as_deref())?,
        source: args.source,
        tags: if args.replace_tags || !args.tag.is_empty() {
            Some(args.tag)
        } else {
            None
        },
        linked_requirement_ids: if args.replace_linked_requirement_ids
            || !args.linked_requirement_id.is_empty()
        {
            Some(args.linked_requirement_id)
        } else {
            None
        },
        linked_task_ids: if args.replace_linked_task_ids || !args.linked_task_id.is_empty() {
            Some(args.linked_task_id)
        } else {
            None
        },
    })
}

fn parse_epic_status_opt(value: Option<&str>) -> Result<Option<EpicStatus>> {
    let Some(value) = value else {
        return Ok(None);
    };

    value
        .parse()
        .map(Some)
        .map_err(|error: String| anyhow::anyhow!(error))
}

fn parse_requirement_priority_opt(value: Option<&str>) -> Result<Option<RequirementPriority>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let priority = match value.trim().to_ascii_lowercase().as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => anyhow::bail!("invalid requirement priority: {value}"),
    };

    Ok(Some(priority))
}
