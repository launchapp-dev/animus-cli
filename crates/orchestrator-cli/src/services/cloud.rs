use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use orchestrator_core::{FileServiceHub, ServiceHub};
use protocol::orchestrator::{OrchestratorTask, RequirementItem};
use protocol::sync_config::SyncConfig;
use protocol::{ConfigBundle, DeployConfig};
use serde::{Deserialize, Serialize};

use crate::{
    print_value, CloudCommand, CloudLinkArgs, CloudLoginArgs, CloudSetupArgs, DeployCommand, DeployCreateArgs,
    DeployDestroyArgs, DeployStartArgs, DeployStatusArgs, DeployStopArgs,
};

pub(crate) async fn handle_cloud(
    command: CloudCommand,
    hub: Arc<FileServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        CloudCommand::Login(args) => handle_login(args, json).await,
        CloudCommand::Setup(args) => handle_setup(args, project_root, json).await,
        CloudCommand::Link(args) => handle_link(args, project_root, json).await,
        CloudCommand::Push => handle_push(hub, project_root, json).await,
        CloudCommand::Pull => handle_pull(hub, project_root, json).await,
        CloudCommand::Status => handle_status(project_root, json).await,
        CloudCommand::Deploy { command: deploy_cmd } => handle_deploy(deploy_cmd, project_root, json).await,
    }
}

async fn handle_login(args: CloudLoginArgs, json: bool) -> Result<()> {
    let server = args.server.unwrap_or_else(|| "https://api.animus.cloud".to_string());
    let server = server.trim_end_matches('/');

    // Step 1: Initiate device auth flow
    let client = reqwest::Client::new();
    let resp = client
        .post(&format!("{}/api/cli/auth/initiate", server))
        .send()
        .await
        .context("Failed to connect to auth server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Auth initiation failed ({status}): {body}");
    }

    let auth_response: AuthInitiateResponse = resp.json().await.context("Failed to parse auth response")?;

    // Step 2: Open browser or print URL
    let auth_url = &auth_response.auth_url;
    if args.no_browser {
        if !json {
            eprintln!("Open the following URL in your browser to authenticate:");
            eprintln!("{}", auth_url);
            eprintln!("Device code: {}", auth_response.device_code);
        }
    } else {
        // Attempt to open browser
        let _ = open_browser(auth_url);
        if !json {
            eprintln!("Opening browser for authentication...");
            eprintln!("If browser did not open, visit: {}", auth_url);
        }
    }

    // Step 3: Poll for completion
    let max_attempts = 120; // 2 minutes with 1 second polling
    let poll_interval = Duration::from_secs(1);

    for attempt in 0..max_attempts {
        tokio::time::sleep(poll_interval).await;

        let resp = client
            .post(&format!("{}/api/cli/auth/complete", server))
            .json(&AuthCompleteRequest { device_code: auth_response.device_code.clone() })
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let complete_response: AuthCompleteResponse =
                    r.json().await.context("Failed to parse completion response")?;

                // Step 4: Store token in SyncConfig
                let mut config = SyncConfig::load_global();
                config.server = Some(server.to_string());
                config.token = Some(complete_response.token.clone());
                config.save_global()?;

                let result = LoginResult {
                    authenticated: true,
                    server: server.to_string(),
                    message: "Successfully authenticated with animus cloud".to_string(),
                };

                if !json {
                    eprintln!("✓ Authentication successful!");
                    eprintln!("Server: {}", server);
                }

                return print_value(result, json);
            }
            Ok(r) if r.status().as_u16() == 400 => {
                // Not yet complete, continue polling
                continue;
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                anyhow::bail!("Auth completion failed ({status}): {body}");
            }
            Err(e) if attempt < max_attempts - 1 => {
                // Network error, retry
                continue;
            }
            Err(e) => {
                anyhow::bail!("Auth completion request failed: {}", e);
            }
        }
    }

    anyhow::bail!("Authentication timeout - user did not complete login within 2 minutes")
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
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;

    let project_id = if let Some(ref id) = args.project_id {
        // Explicit project_id provided
        id.clone()
    } else {
        // Auto-detect from git remote
        let origin_url = get_git_origin(project_root)
            .ok_or_else(|| anyhow::anyhow!("Could not detect git remote. Run: animus cloud link --project-id <id>"))?;

        let (owner, repo) = parse_github_repo(&origin_url).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not parse GitHub repo from remote URL: {}. Run: animus cloud link --project-id <id>",
                origin_url
            )
        })?;

        // Call /api/cli/projects/ensure to check for GitHub App installation
        let client = build_client(&token)?;
        let ensure_url = format!(
            "{}/api/cli/projects/ensure?owner={}&repo={}",
            server.trim_end_matches('/'),
            urlencoding(&owner),
            urlencoding(&repo)
        );

        let resp = client.post(&ensure_url).send().await.context("Failed to connect to projects endpoint")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 {
                anyhow::bail!(
                    "No GitHub App installation found for {}/{}. Run: animus cloud link --project-id <id>",
                    owner,
                    repo
                );
            }
            anyhow::bail!("Project detection failed ({status}): {body}");
        }

        let body = resp.json::<EnsureProjectResponse>().await.context("Failed to parse projects response")?;
        body.project_id
    };

    let mut config = SyncConfig::load_for_project(project_root);
    config.project_id = Some(project_id.clone());
    config.save_for_project(project_root)?;

    let result = serde_json::json!({ "linked": true, "project_id": project_id });
    print_value(result, json)
}

fn build_config_bundle(project_root: &str) -> Result<ConfigBundle> {
    let mut bundle = ConfigBundle::new();
    let ao_dir = PathBuf::from(project_root).join(".ao");

    // Collect workflow YAML files
    if let Ok(entries) = std::fs::read_dir(ao_dir.join("workflows")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "yaml" || ext == "yml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(file_name) = path.file_name() {
                        let key = format!(".ao/workflows/{}", file_name.to_string_lossy());
                        bundle.add_file(key, content);
                    }
                }
            }
        }
    }

    // Collect root workflows.yaml
    let workflows_file = ao_dir.join("workflows.yaml");
    if workflows_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&workflows_file) {
            bundle.add_file(".ao/workflows.yaml".to_string(), content);
        }
    }

    // Collect config.json
    let config_file = ao_dir.join("config.json");
    if config_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_file) {
            bundle.add_file(".ao/config.json".to_string(), content);
        }
    }

    Ok(bundle)
}

async fn handle_push(hub: Arc<FileServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

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

    // Push config bundle to cloud
    let config_bundle = build_config_bundle(project_root)?;
    let config_files_count = config_bundle.file_count();

    if !config_bundle.is_empty() {
        let config_resp = client
            .post(&format!("{}/api/projects/{}/configs", server.trim_end_matches('/'), project_id))
            .json(&config_bundle)
            .send()
            .await
            .context("Failed to connect to configs endpoint")?;

        if !config_resp.status().is_success() {
            let status = config_resp.status();
            let body = config_resp.text().await.unwrap_or_default();
            anyhow::bail!("Config push failed ({status}): {body}");
        }
    }

    let mut config = SyncConfig::load_for_project(project_root);
    config.last_synced_at = Some(sync_resp.server_time.clone());
    config.save_for_project(project_root)?;

    let result = PushResult {
        tasks_sent: tasks_count,
        requirements_sent: reqs_count,
        config_files_sent: config_files_count,
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
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

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

    // Try to fetch cloud status if configured
    let (projects, daemons, workflows) = if config.is_configured() {
        match fetch_cloud_status(&config).await {
            Ok((projects, daemons, workflows)) => (Some(projects), Some(daemons), Some(workflows)),
            Err(_) => {
                // Fall back gracefully if cloud API is unavailable
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    let result = StatusResult {
        configured: config.is_configured(),
        server: config.server.clone(),
        project_id: config.project_id.clone(),
        last_synced_at: config.last_synced_at.clone(),
        cloud_projects: projects,
        cloud_daemons: daemons,
        active_workflows: workflows,
    };
    print_value(result, json)
}

async fn fetch_cloud_status(config: &SyncConfig) -> Result<(Vec<CloudProject>, Vec<CloudDaemon>, Vec<CloudWorkflow>)> {
    let server = config.server_url()?;
    let token = config.bearer_token()?;

    let client = build_client(&token)?;
    let resp = client
        .get(&format!("{}/api/cli/status", server.trim_end_matches('/')))
        .send()
        .await
        .context("Failed to connect to cloud status endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Cloud status check failed ({status}): {body}");
    }

    let cloud_response: CloudStatusResponse = resp.json().await.context("Failed to parse cloud status response")?;

    Ok((cloud_response.projects, cloud_response.daemons, cloud_response.workflows))
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
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

    let client = build_client(&token)?;
    let create_request = CreateDaemonRequest {
        app_name: args.app_name.clone(),
        region: args.region.clone(),
        machine_size: args.machine_size.clone(),
    };

    let resp = client
        .post(&format!("{}/api/cli/projects/{}/daemons", server.trim_end_matches('/'), project_id))
        .json(&create_request)
        .send()
        .await
        .context("Failed to connect to daemon creation endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon creation failed ({status}): {body}");
    }

    let daemon_resp: DaemonResponse = resp.json().await.context("Failed to parse daemon response")?;

    // Save daemon ID locally for future reference
    let mut deploy_config = DeployConfig::load_for_project(project_root);
    deploy_config.app_name = Some(args.app_name.clone());
    deploy_config.region = Some(args.region.clone());
    deploy_config.last_deployed_at = Some(chrono::Utc::now().to_rfc3339());
    deploy_config.machine_ids.push(daemon_resp.daemon_id.clone());
    deploy_config.save_for_project(project_root)?;

    let result = DeployCreateResult {
        app_name: args.app_name,
        region: args.region,
        machine_size: args.machine_size,
        status: daemon_resp.status,
        deployed_at: daemon_resp.created_at,
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
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

    let deploy_config = DeployConfig::load_for_project(project_root);

    // Verify the app name matches
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but attempting to destroy '{}'. Use 'ao cloud deploy status' to check.",
                configured_app,
                args.app_name
            );
        }
    } else {
        anyhow::bail!("No deployment configured for this project. Run 'ao cloud deploy create' first.");
    }

    // Get the daemon ID from local config
    let daemon_id = deploy_config
        .machine_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("No daemon ID found in local configuration"))?;

    let client = build_client(&token)?;
    let resp = client
        .delete(&format!(
            "{}/api/cli/projects/{}/daemons/{}",
            server.trim_end_matches('/'),
            project_id,
            daemon_id
        ))
        .send()
        .await
        .context("Failed to connect to daemon destruction endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon destruction failed ({status}): {body}");
    }

    // Clear deployment configuration locally
    let mut deploy_config = DeployConfig::load_for_project(project_root);
    deploy_config.app_name = None;
    deploy_config.region = None;
    deploy_config.machine_ids.clear();
    deploy_config.status = Some("destroyed".to_string());
    deploy_config.save_for_project(project_root)?;

    let result = DeployDestroyResult { app_name: args.app_name, status: "destroyed".to_string(), machines_destroyed: 1 };

    if !json {
        eprintln!("Deployment destroyed successfully!");
        eprintln!("App: {}", result.app_name);
    }

    print_value(result, json)
}

#[derive(Serialize)]
struct CreateDaemonRequest {
    app_name: String,
    region: String,
    machine_size: String,
}

#[derive(Deserialize)]
struct DaemonResponse {
    daemon_id: String,
    app_name: String,
    region: String,
    status: String,
    created_at: String,
    updated_at: Option<String>,
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

fn parse_github_repo(url: &str) -> Option<(String, String)> {
    // Handle both HTTPS and SSH GitHub URLs
    // HTTPS: https://github.com/owner/repo or https://github.com/owner/repo.git
    // SSH: git@github.com:owner/repo or git@github.com:owner/repo.git

    let url = url.trim();

    // SSH URL format: git@github.com:owner/repo[.git]
    if let Some(stripped) = url.strip_prefix("git@github.com:") {
        let repo_part = stripped.trim_end_matches(".git").trim_end_matches('/');
        let parts: Vec<&str> = repo_part.split('/').collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // HTTPS URL format: https://github.com/owner/repo[.git]
    if let Some(stripped) = url.strip_prefix("https://github.com/") {
        let repo_part = stripped.trim_end_matches(".git").trim_end_matches('/');
        let parts: Vec<&str> = repo_part.split('/').collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // Also try with http (less common but possible)
    if let Some(stripped) = url.strip_prefix("http://github.com/") {
        let repo_part = stripped.trim_end_matches(".git").trim_end_matches('/');
        let parts: Vec<&str> = repo_part.split('/').collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_repo_https() {
        let result = parse_github_repo("https://github.com/anthropics/claude-code");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_https_with_git() {
        let result = parse_github_repo("https://github.com/anthropics/claude-code.git");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_ssh() {
        let result = parse_github_repo("git@github.com:anthropics/claude-code");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_ssh_with_git() {
        let result = parse_github_repo("git@github.com:anthropics/claude-code.git");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_with_trailing_slash() {
        let result = parse_github_repo("https://github.com/anthropics/claude-code/");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_http() {
        let result = parse_github_repo("http://github.com/anthropics/claude-code");
        assert_eq!(result, Some(("anthropics".to_string(), "claude-code".to_string())));
    }

    #[test]
    fn test_parse_github_repo_invalid() {
        let result = parse_github_repo("https://gitlab.com/anthropics/claude-code");
        assert_eq!(result, None);
    }
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
    config_files_sent: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_projects: Option<Vec<CloudProject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_daemons: Option<Vec<CloudDaemon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_workflows: Option<Vec<CloudWorkflow>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CloudProject {
    id: String,
    name: String,
    created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CloudDaemon {
    id: String,
    project_id: String,
    app_name: String,
    status: String,
    region: String,
    machine_size: String,
    created_at: String,
    updated_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CloudWorkflow {
    id: String,
    name: String,
    project_id: String,
    status: String,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Deserialize)]
struct CloudStatusResponse {
    projects: Vec<CloudProject>,
    daemons: Vec<CloudDaemon>,
    workflows: Vec<CloudWorkflow>,
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

#[derive(Deserialize)]
struct EnsureProjectResponse {
    project_id: String,
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
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

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

    // Get the daemon ID from local config
    let daemon_id = deploy_config
        .machine_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("No daemon ID found in local configuration"))?;

    let client = build_client(&token)?;
    let resp = client
        .post(&format!(
            "{}/api/cli/projects/{}/daemons/{}/start",
            server.trim_end_matches('/'),
            project_id,
            daemon_id
        ))
        .send()
        .await
        .context("Failed to connect to daemon start endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon start failed ({status}): {body}");
    }

    let daemon_resp: DaemonResponse = resp.json().await.context("Failed to parse daemon response")?;

    let result = DeployStartResult {
        app_name: args.app_name,
        status: daemon_resp.status,
        started_at: daemon_resp.updated_at.unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    };

    if !json {
        eprintln!("Deployment started successfully!");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
    }

    print_value(result, json)
}

async fn handle_stop(args: DeployStopArgs, project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

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

    // Get the daemon ID from local config
    let daemon_id = deploy_config
        .machine_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("No daemon ID found in local configuration"))?;

    let client = build_client(&token)?;
    let resp = client
        .post(&format!(
            "{}/api/cli/projects/{}/daemons/{}/stop",
            server.trim_end_matches('/'),
            project_id,
            daemon_id
        ))
        .send()
        .await
        .context("Failed to connect to daemon stop endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon stop failed ({status}): {body}");
    }

    let daemon_resp: DaemonResponse = resp.json().await.context("Failed to parse daemon response")?;

    let result = DeployStopResult {
        app_name: args.app_name,
        status: daemon_resp.status,
        stopped_at: daemon_resp.updated_at.unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    };

    if !json {
        eprintln!("Deployment stopped successfully!");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
    }

    print_value(result, json)
}

async fn handle_status_deploy(args: DeployStatusArgs, project_root: &str, json: bool) -> Result<()> {
    let config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = config.bearer_token()?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

    let deploy_config = DeployConfig::load_for_project(project_root);

    // Check if the app name matches if a deployment is configured
    if let Some(ref configured_app) = deploy_config.app_name {
        if configured_app != &args.app_name {
            anyhow::bail!(
                "App name mismatch: configured '{}' but checking status for '{}'. Use 'ao cloud deploy status --app-name {}' to check configured deployment.",
                configured_app,
                args.app_name,
                configured_app
            );
        }
    } else {
        anyhow::bail!("No deployment configured for this project. Run 'ao cloud deploy create' first.");
    }

    // Get the daemon ID from local config
    let daemon_id = deploy_config
        .machine_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("No daemon ID found in local configuration"))?;

    let client = build_client(&token)?;
    let resp = client
        .get(&format!(
            "{}/api/cli/projects/{}/daemons/{}",
            server.trim_end_matches('/'),
            project_id,
            daemon_id
        ))
        .send()
        .await
        .context("Failed to connect to daemon status endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon status check failed ({status}): {body}");
    }

    let daemon_resp: DaemonResponse = resp.json().await.context("Failed to parse daemon response")?;

    let result = DeployStatusDeployResult {
        app_name: args.app_name,
        status: daemon_resp.status,
        region: deploy_config.region.clone(),
        machines: vec![daemon_resp.daemon_id],
        last_deployed_at: deploy_config.last_deployed_at.clone(),
    };

    if !json {
        eprintln!("Deployment Status");
        eprintln!("App: {}", result.app_name);
        eprintln!("Status: {}", result.status);
        if let Some(region) = &result.region {
            eprintln!("Region: {}", region);
        }
        eprintln!(
            "Machines: {}",
            if result.machines.is_empty() { "none".to_string() } else { result.machines.join(", ") }
        );
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

#[derive(Deserialize)]
struct AuthInitiateResponse {
    device_code: String,
    auth_url: String,
}

#[derive(Serialize)]
struct AuthCompleteRequest {
    device_code: String,
}

#[derive(Deserialize)]
struct AuthCompleteResponse {
    token: String,
}

#[derive(Serialize)]
struct LoginResult {
    authenticated: bool,
    server: String,
    message: String,
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }

    #[cfg(target_os = "linux")]
    {
        // Try xdg-open first, then firefox, then chromium
        let _ = std::process::Command::new("xdg-open").arg(url).spawn().or_else(|_| {
            std::process::Command::new("firefox")
                .arg(url)
                .spawn()
                .or_else(|_| std::process::Command::new("chromium").arg(url).spawn())
        });
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd").args(&["/C", "start", url]).spawn()?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        // On other platforms, just return Ok (user will need to visit URL manually)
    }

    Ok(())
}
