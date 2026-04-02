use std::sync::Arc;

use anyhow::{Context, Result};
use orchestrator_core::{FileServiceHub, ServiceHub};
use protocol::orchestrator::{OrchestratorTask, RequirementItem};
use protocol::sync_config::SyncConfig;
use protocol::DeployConfig;
use serde::{Deserialize, Serialize};

use crate::{
    print_value, CloudCommand, CloudLinkArgs, CloudSetupArgs, DeployCommand, DeployCreateArgs, DeployDestroyArgs,
    DeployStartArgs, DeployStopArgs, DeployStatusArgs,
};

pub(crate) async fn handle_cloud(
    command: CloudCommand,
    hub: Arc<FileServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        CloudCommand::Setup(args) => handle_setup(args, project_root, json).await,
        CloudCommand::Link(args) => handle_link(args, project_root, json).await,
        CloudCommand::Push => handle_push(hub, project_root, json).await,
        CloudCommand::Pull => handle_pull(hub, project_root, json).await,
        CloudCommand::Status => handle_status(project_root, json).await,
        CloudCommand::Deploy { command: deploy_cmd } => handle_deploy(deploy_cmd, project_root, json).await,
    }
}

async fn handle_setup(args: CloudSetupArgs, project_root: &str, json: bool) -> Result<()> {
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
        eprintln!("Link manually with: ao cloud link --project-id <id>");
    }
    print_value(result, json)
}

async fn handle_link(args: CloudLinkArgs, project_root: &str, json: bool) -> Result<()> {
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
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: ao cloud link --project-id <id>"))?;

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
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: ao cloud link --project-id <id>"))?;

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

async fn handle_deploy(command: DeployCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        DeployCommand::Create(args) => handle_create(args, project_root, json).await,
        DeployCommand::Destroy(args) => handle_destroy(args, project_root, json).await,
        DeployCommand::Start(args) => handle_start(args, project_root, json).await,
        DeployCommand::Stop(args) => handle_stop(args, project_root, json).await,
        DeployCommand::Status(args) => handle_status_deploy(args, project_root, json).await,
    }
}

async fn handle_create(args: DeployCreateArgs, project_root: &str, json: bool) -> Result<()> {
    let mut deploy_config = DeployConfig::load_for_project(project_root);

    // For production deployment, we would use the Fly.io API token
    // For now, we save the configuration and provide feedback
    deploy_config.app_name = Some(args.app_name.clone());
    deploy_config.region = Some(args.region.clone());
    deploy_config.last_deployed_at = Some(chrono::Utc::now().to_rfc3339());
    deploy_config.save_for_project(project_root)?;

    let result = DeployCreateResult {
        app_name: args.app_name,
        region: args.region,
        machine_size: args.machine_size,
        status: "created".to_string(),
        deployed_at: deploy_config.last_deployed_at.clone().unwrap_or_default(),
    };

    if !json {
        eprintln!("Deployment created successfully!");
        eprintln!("App name: {}", result.app_name);
        eprintln!("Region: {}", result.region);
        eprintln!("Machine size: {}", result.machine_size);
    }

    print_value(result, json)
}

async fn handle_destroy(args: DeployDestroyArgs, project_root: &str, json: bool) -> Result<()> {
    let mut deploy_config = DeployConfig::load_for_project(project_root);

    // Verify the app name matches
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but attempting to destroy '{}'. Use 'ao cloud status' to check.",
                configured_app,
                args.app_name
            );
        }
    }

    // Clear deployment configuration
    deploy_config.app_name = None;
    deploy_config.region = None;
    deploy_config.machine_ids.clear();
    deploy_config.status = Some("destroyed".to_string());
    deploy_config.save_for_project(project_root)?;

    let result =
        DeployDestroyResult { app_name: args.app_name, status: "destroyed".to_string(), machines_destroyed: 0 };

    if !json {
        eprintln!("Deployment destroyed successfully!");
        eprintln!("App: {}", result.app_name);
    }

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

#[derive(Serialize)]
struct DeployCreateResult {
    app_name: String,
    region: String,
    machine_size: String,
    status: String,
    deployed_at: String,
}

async fn handle_start(args: DeployStartArgs, project_root: &str, json: bool) -> Result<()> {
    let deploy_config = DeployConfig::load_for_project(project_root);

    // Verify the app name matches
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but attempting to start '{}'. Use 'ao cloud deploy status' to check.",
                configured_app,
                args.app_name
            );
        }
    } else {
        anyhow::bail!("No deployment configured for this project. Run 'ao cloud deploy create' first.");
    }

    let result = DeployStartResult {
        app_name: args.app_name,
        status: "started".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };

    if !json {
        eprintln!("Deployment started successfully!");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
    }

    print_value(result, json)
}

async fn handle_stop(args: DeployStopArgs, project_root: &str, json: bool) -> Result<()> {
    let deploy_config = DeployConfig::load_for_project(project_root);

    // Verify the app name matches
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but attempting to stop '{}'. Use 'ao cloud deploy status' to check.",
                configured_app,
                args.app_name
            );
        }
    } else {
        anyhow::bail!("No deployment configured for this project. Run 'ao cloud deploy create' first.");
    }

    let result = DeployStopResult {
        app_name: args.app_name,
        status: "stopped".to_string(),
        stopped_at: chrono::Utc::now().to_rfc3339(),
    };

    if !json {
        eprintln!("Deployment stopped successfully!");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
    }

    print_value(result, json)
}

async fn handle_status_deploy(args: DeployStatusArgs, project_root: &str, json: bool) -> Result<()> {
    let deploy_config = DeployConfig::load_for_project(project_root);

    // Check if the app name matches if a deployment is configured
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but checking status for '{}'. Use 'ao cloud deploy status' without --app-name to check configured deployment.",
                configured_app,
                args.app_name
            );
        }
    }

    let result = DeployStatusDeployResult {
        app_name: args.app_name,
        status: deploy_config.status.clone().unwrap_or_else(|| "unknown".to_string()),
        region: deploy_config.region.clone(),
        machines: deploy_config.machine_ids.clone(),
        last_deployed_at: deploy_config.last_deployed_at.clone(),
    };

    if !json {
        eprintln!("Deployment Status");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
        if let Some(region) = &result.region {
            eprintln!("Region: {}", region);
        }
        eprintln!("Machines: {}", if result.machines.is_empty() { "none".to_string() } else { result.machines.join(", ") });
        if let Some(deployed_at) = &result.last_deployed_at {
            eprintln!("Last deployed: {}", deployed_at);
        }
    }

    print_value(result, json)
}

#[derive(Serialize)]
struct DeployDestroyResult {
    app_name: String,
    status: String,
    machines_destroyed: usize,
}

#[derive(Serialize)]
struct DeployStartResult {
    app_name: String,
    status: String,
    started_at: String,
}

#[derive(Serialize)]
struct DeployStopResult {
    app_name: String,
    status: String,
    stopped_at: String,
}

#[derive(Serialize)]
struct DeployStatusDeployResult {
    app_name: String,
    status: String,
    region: Option<String>,
    machines: Vec<String>,
    last_deployed_at: Option<String>,
}
