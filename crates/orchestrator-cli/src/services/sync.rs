use std::sync::Arc;

use anyhow::{Context, Result};
use orchestrator_core::{FileServiceHub, ServiceHub};
use protocol::orchestrator::{OrchestratorTask, RequirementItem};
use protocol::sync_config::SyncConfig;
use serde::{Deserialize, Serialize};

use crate::{print_value, SyncCommand, SyncLinkArgs, SyncSetupArgs};

pub(crate) async fn handle_sync(
    command: SyncCommand,
    hub: Arc<FileServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        SyncCommand::Setup(args) => handle_setup(args, project_root, json).await,
        SyncCommand::Link(args) => handle_link(args, project_root, json).await,
        SyncCommand::Push => handle_push(hub, project_root, json).await,
        SyncCommand::Pull => handle_pull(hub, project_root, json).await,
        SyncCommand::Status => handle_status(project_root, json).await,
    }
}

async fn handle_setup(args: SyncSetupArgs, project_root: &str, json: bool) -> Result<()> {
    let mut global_config = SyncConfig::load_global();
    global_config.server = Some(args.server.clone());
    global_config.token = Some(args.token.clone());
    global_config.save_global()?;

    let origin_url = get_git_origin(project_root);

    if let Some(ref url) = origin_url {
        let client = build_client(&args.token)?;
        let server = args.server.trim_end_matches('/');
        let resp = client.get(&format!("{server}/api/projects/by-repo?url={}", urlencoding(url))).send().await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<ProjectResponse>().await {
                    let mut project_config = SyncConfig::load_for_project(project_root);
                    project_config.project_id = Some(body.project.id.clone());
                    project_config.save_for_project(project_root)?;

                    let result = SetupResult {
                        server: args.server,
                        project_id: Some(body.project.id),
                        project_name: Some(body.project.name),
                        auto_linked: true,
                    };
                    return print_value(result, json);
                }
            }
        }
    }

    let result = SetupResult { server: args.server, project_id: None, project_name: None, auto_linked: false };
    if !json {
        eprintln!("Sync server configured. No matching remote project found for this repo.");
        eprintln!("Link manually with: ao sync link --project-id <id>");
    }
    print_value(result, json)
}

async fn handle_link(args: SyncLinkArgs, project_root: &str, json: bool) -> Result<()> {
    let mut config = SyncConfig::load_for_project(project_root);
    config.project_id = Some(args.project_id.clone());
    config.save_for_project(project_root)?;

    let result = serde_json::json!({ "linked": true, "project_id": args.project_id });
    print_value(result, json)
}

async fn handle_push(hub: Arc<FileServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: ao sync link --project-id <id>"))?;

    let tasks: Vec<OrchestratorTask> = hub.tasks().list().await?;
    let requirements: Vec<RequirementItem> = hub.planning().list_requirements().await?;
    let tasks_count = tasks.len();
    let reqs_count = requirements.len();

    let client = build_client(&token)?;
    let resp = client
        .post(&format!("{}/api/projects/{}/sync", server.trim_end_matches('/'), project_id))
        .json(&SyncRequest { tasks, requirements, since: config.last_synced_at.clone() })
        .send()
        .await
        .context("Failed to connect to sync server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Sync push failed ({status}): {body}");
    }

    let sync_resp: SyncResponse = resp.json().await.context("Failed to parse sync response")?;

    let mut config = SyncConfig::load_for_project(project_root);
    config.last_synced_at = Some(sync_resp.server_time.clone());
    config.save_for_project(project_root)?;

    let result = PushResult {
        tasks_sent: tasks_count,
        requirements_sent: reqs_count,
        conflicts: sync_resp.conflicts.len(),
        server_time: sync_resp.server_time,
    };

    if !json && !sync_resp.conflicts.is_empty() {
        eprintln!("Conflicts ({}):", sync_resp.conflicts.len());
        for c in &sync_resp.conflicts {
            eprintln!("  {} {}: {}", c.r#type, c.id, c.reason);
        }
    }

    print_value(result, json)
}

async fn handle_pull(hub: Arc<FileServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: ao sync link --project-id <id>"))?;

    let client = build_client(&token)?;
    let resp = client
        .post(&format!("{}/api/projects/{}/sync", server.trim_end_matches('/'), project_id))
        .json(&SyncRequest { tasks: vec![], requirements: vec![], since: config.last_synced_at.clone() })
        .send()
        .await
        .context("Failed to connect to sync server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Sync pull failed ({status}): {body}");
    }

    let sync_resp: SyncResponse = resp.json().await.context("Failed to parse sync response")?;

    let task_count = sync_resp.tasks.len();
    let req_count = sync_resp.requirements.len();

    for task in sync_resp.tasks {
        hub.tasks().replace(task).await?;
    }
    for req in sync_resp.requirements {
        hub.planning().upsert_requirement(req).await?;
    }

    let mut config = SyncConfig::load_for_project(project_root);
    config.last_synced_at = Some(sync_resp.server_time.clone());
    config.save_for_project(project_root)?;

    let result =
        PullResult { tasks_received: task_count, requirements_received: req_count, server_time: sync_resp.server_time };
    print_value(result, json)
}

async fn handle_status(project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let result = StatusResult {
        configured: config.is_configured(),
        server: config.server.clone(),
        project_id: config.project_id.clone(),
        last_synced_at: config.last_synced_at.clone(),
    };
    print_value(result, json)
}

fn build_client(token: &str) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?);
    reqwest::Client::builder().default_headers(headers).build().context("Failed to build HTTP client")
}

fn get_git_origin(project_root: &str) -> Option<String> {
    std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(
            |o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            },
        )
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

#[derive(Serialize)]
struct SetupResult {
    server: String,
    project_id: Option<String>,
    project_name: Option<String>,
    auto_linked: bool,
}

#[derive(Serialize)]
struct PushResult {
    tasks_sent: usize,
    requirements_sent: usize,
    conflicts: usize,
    server_time: String,
}

#[derive(Serialize)]
struct PullResult {
    tasks_received: usize,
    requirements_received: usize,
    server_time: String,
}

#[derive(Serialize)]
struct StatusResult {
    configured: bool,
    server: Option<String>,
    project_id: Option<String>,
    last_synced_at: Option<String>,
}

#[derive(Deserialize)]
struct ProjectResponse {
    project: ProjectInfo,
}

#[derive(Deserialize)]
struct ProjectInfo {
    id: String,
    name: String,
}

#[derive(Serialize)]
struct SyncRequest {
    tasks: Vec<OrchestratorTask>,
    requirements: Vec<RequirementItem>,
    since: Option<String>,
}

#[derive(Deserialize)]
struct SyncResponse {
    tasks: Vec<OrchestratorTask>,
    requirements: Vec<RequirementItem>,
    conflicts: Vec<SyncConflict>,
    server_time: String,
}

#[derive(Deserialize)]
struct SyncConflict {
    r#type: String,
    id: String,
    reason: String,
}
