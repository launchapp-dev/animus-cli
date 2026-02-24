use crate::cli_types::MockupCommand;
use crate::{parse_input_json_or, print_value};
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use super::state::{project_state_dir, read_json_or_default, write_json_pretty};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MockupState {
    #[serde(default)]
    mockups: Vec<MockupRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockupRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    mockup_type: String,
    #[serde(default)]
    requirement_ids: Vec<String>,
    #[serde(default)]
    flow_ids: Vec<String>,
    #[serde(default)]
    files: Vec<MockupFileRecord>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockupFileRecord {
    relative_path: String,
    encoding: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MockupCreateInputCli {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mockup_type: Option<String>,
    #[serde(default)]
    requirement_ids: Vec<String>,
    #[serde(default)]
    flow_ids: Vec<String>,
    #[serde(default)]
    files: Vec<MockupFileRecord>,
}

fn mockups_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("mockups.json")
}

fn load_mockups(project_root: &str) -> Result<MockupState> {
    read_json_or_default(&mockups_path(project_root))
}

fn save_mockups(project_root: &str, state: &MockupState) -> Result<()> {
    write_json_pretty(&mockups_path(project_root), state)
}

pub(super) async fn handle_requirement_mockups(
    command: MockupCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        MockupCommand::List => print_value(load_mockups(project_root)?.mockups, json),
        MockupCommand::Create(args) => {
            let mut state = load_mockups(project_root)?;
            let input = parse_input_json_or(args.input_json, || {
                Ok(MockupCreateInputCli {
                    id: None,
                    name: args.name,
                    description: args.description,
                    mockup_type: args.mockup_type,
                    requirement_ids: args.requirement_id,
                    flow_ids: args.flow_id,
                    files: Vec::new(),
                })
            })?;
            if input.name.trim().is_empty() {
                anyhow::bail!("mockup name is required");
            }

            let now = Utc::now().to_rfc3339();
            let record = MockupRecord {
                id: input
                    .id
                    .unwrap_or_else(|| format!("MOCK-{}", Uuid::new_v4().simple())),
                name: input.name,
                description: input.description,
                mockup_type: input.mockup_type.unwrap_or_else(|| "wireframe".to_string()),
                requirement_ids: input.requirement_ids,
                flow_ids: input.flow_ids,
                files: input.files,
                created_at: now.clone(),
                updated_at: now,
            };
            state.mockups.push(record.clone());
            save_mockups(project_root, &state)?;
            print_value(record, json)
        }
        MockupCommand::Link(args) => {
            let mut state = load_mockups(project_root)?;
            let mockup = state
                .mockups
                .iter_mut()
                .find(|mockup| mockup.id == args.id)
                .ok_or_else(|| anyhow!("mockup not found: {}", args.id))?;
            for requirement_id in args.requirement_id {
                if !mockup
                    .requirement_ids
                    .iter()
                    .any(|existing| existing == &requirement_id)
                {
                    mockup.requirement_ids.push(requirement_id);
                }
            }
            for flow_id in args.flow_id {
                if !mockup.flow_ids.iter().any(|existing| existing == &flow_id) {
                    mockup.flow_ids.push(flow_id);
                }
            }
            mockup.updated_at = Utc::now().to_rfc3339();
            let updated = mockup.clone();
            save_mockups(project_root, &state)?;
            print_value(updated, json)
        }
        MockupCommand::GetFile(args) => {
            let state = load_mockups(project_root)?;
            let mockup = state
                .mockups
                .iter()
                .find(|mockup| mockup.id == args.id)
                .ok_or_else(|| anyhow!("mockup not found: {}", args.id))?;
            let file = mockup
                .files
                .iter()
                .find(|file| file.relative_path == args.relative_path)
                .ok_or_else(|| anyhow!("mockup file not found: {}", args.relative_path))?;
            print_value(file, json)
        }
    }
}
