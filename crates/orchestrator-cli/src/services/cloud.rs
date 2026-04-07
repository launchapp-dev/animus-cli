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
    use sha2::{Digest, Sha256};

    let server = args.server.unwrap_or_else(|| "https://animus.launchapp.dev".to_string());
    let server = server.trim_end_matches('/');
    let client_id = "animus-cli";

    // Step 1: Start a local HTTP server to receive the authorization code
    let port: u16 = 19823;
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .context("Failed to bind port 19823. Is another animus login running?")?;
    let redirect_uri = format!("http://localhost:{}/callback", port);

    // Step 2: Generate PKCE code verifier + challenge (S256)
    let code_verifier = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let code_challenge = {
        let hash = Sha256::digest(code_verifier.as_bytes());
        base64_url_encode(&hash)
    };
    let state = uuid::Uuid::new_v4().to_string();

    // Step 3: Open browser to the OAuth 2.1 authorize endpoint
    // Flow: /oauth2/authorize → /login (if not authed) → user logs in → /oauth2/authorize (again, now authed)
    //       → issues auth code → redirects to localhost:19823/callback?code=CODE&state=STATE
    let auth_url = format!(
        "{}/api/auth/oauth2/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        server,
        urlencoding::encode(client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode("openid profile email offline_access"),
        urlencoding::encode(&state),
        urlencoding::encode(&code_challenge),
    );

    if args.no_browser {
        if !json {
            eprintln!("Open the following URL in your browser to authenticate:");
            eprintln!("{}", auth_url);
        }
    } else {
        let _ = open_browser(&auth_url);
        if !json {
            eprintln!("Opening browser for authentication...");
            eprintln!("If browser did not open, visit: {}", auth_url);
        }
    }

    // Step 4: Wait for the authorization code callback
    let auth_code = tokio::time::timeout(Duration::from_secs(120), async {
        let (stream, _) = listener.accept().await.context("Failed to accept connection")?;

        let mut buf = vec![0u8; 4096];
        stream.readable().await?;
        let n = stream.try_read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        let path = request.lines().next().unwrap_or("");
        let url_part = path.split_whitespace().nth(1).unwrap_or("");

        let mut received_code = None;
        let mut received_state = None;
        let mut received_error = None;

        if let Some(query) = url_part.split('?').nth(1) {
            for param in query.split('&') {
                let mut kv = param.splitn(2, '=');
                let key = kv.next().unwrap_or("");
                let value = kv.next().unwrap_or("");
                match key {
                    "code" => received_code = Some(urlencoding::decode(value).unwrap_or_default().to_string()),
                    "state" => received_state = Some(urlencoding::decode(value).unwrap_or_default().to_string()),
                    "error" => received_error = Some(urlencoding::decode(value).unwrap_or_default().to_string()),
                    _ => {}
                }
            }
        }

        // Send browser response
        let (status_line, html) = if received_code.is_some() {
            ("200 OK", "<html><body><h1>Authentication successful!</h1><p>You can close this tab and return to the terminal.</p></body></html>")
        } else {
            ("400 Bad Request", "<html><body><h1>Authentication failed</h1><p>Please try again.</p></body></html>")
        };
        let response = format!(
            "HTTP/1.1 {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status_line, html.len(), html
        );
        stream.writable().await?;
        let _ = stream.try_write(response.as_bytes());

        if let Some(err) = received_error {
            anyhow::bail!("OAuth error: {}", err);
        }

        if received_state.as_deref() != Some(&state) {
            anyhow::bail!("State mismatch. Try again.");
        }

        received_code.ok_or_else(|| anyhow::anyhow!("No authorization code received."))
    })
    .await
    .map_err(|_| anyhow::anyhow!("Authentication timeout — user did not complete login within 2 minutes"))??;

    // Step 5: Exchange authorization code for access token
    if !json {
        eprintln!("Exchanging authorization code...");
    }

    let http_client = reqwest::Client::new();
    let token_resp = http_client
        .post(&format!("{}/api/auth/oauth2/token", server))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", auth_code.as_str()),
            ("code_verifier", code_verifier.as_str()),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
        .context("Failed to exchange authorization code")?;

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({status}): {body}");
    }

    let token_data: TokenResponse = token_resp.json().await.context("Failed to parse token response")?;

    // Step 6: Store access token and refresh token with expiration
    let mut config = SyncConfig::load_global();
    config.server = Some(server.to_string());
    config.token = Some(token_data.access_token.clone());

    // Store refresh token if provided
    if let Some(ref refresh_token) = token_data.refresh_token {
        config.refresh_token = Some(refresh_token.clone());
    }

    // Calculate and store token expiration time
    if let Some(expires_in) = token_data.expires_in {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in as i64);
        config.access_token_expires_at = Some(expires_at.to_rfc3339());
    }

    config.save_global()?;

    let result = LoginResult {
        authenticated: true,
        server: server.to_string(),
        message: "Successfully authenticated with Animus Cloud".to_string(),
    };

    if !json {
        eprintln!("✓ Authentication successful!");
        eprintln!("Server: {}", server);
    }

    print_value(result, json)
}

/// Base64url encode (no padding) per RFC 7636
fn base64_url_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
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
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;

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
        let ensure_url = format!("{}/api/cli/projects/ensure", server.trim_end_matches('/'));

        let request_body =
            EnsureProjectRequest { org_id: owner.clone(), name: repo.clone(), repo_url: origin_url.clone() };

        let resp = client
            .post(&ensure_url)
            .json(&request_body)
            .send()
            .await
            .context("Failed to connect to projects endpoint")?;

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

async fn build_config_bundle(hub: Arc<FileServiceHub>, project_root: &str) -> Result<ConfigBundle> {
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

    // Collect tasks
    if let Ok(tasks) = hub.tasks().list().await {
        bundle.set_tasks(tasks);
    }

    // Collect requirements
    if let Ok(requirements) = hub.planning().list_requirements().await {
        bundle.set_requirements(requirements);
    }

    Ok(bundle)
}

async fn handle_push(hub: Arc<FileServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
    let project_id = config
        .project_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No project linked. Run: animus cloud link --project-id <id>"))?;

    // Build .ao/ config bundle with tasks and requirements
    let config_bundle = build_config_bundle(hub.clone(), project_root).await?;
    let config_files_count = config_bundle.file_count();
    let tasks_count = config_bundle.task_count();
    let requirements_count = config_bundle.requirement_count();

    if config_bundle.is_empty() {
        anyhow::bail!("No .ao/ config found to push. Create .ao/workflows/ first.");
    }

    // Get current git ref
    let git_ref = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    let client = build_client(&token)?;

    // Push config to POST /api/configs/push
    let push_body = serde_json::json!({
        "projectId": project_id,
        "configData": config_bundle,
        "gitRef": git_ref,
    });

    let resp = client
        .post(&format!("{}/api/configs/push", server.trim_end_matches('/')))
        .json(&push_body)
        .send()
        .await
        .context("Failed to push config to cloud")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Config push failed ({status}): {body}");
    }

    let mut config = SyncConfig::load_for_project(project_root);
    config.last_synced_at = Some(chrono::Utc::now().to_rfc3339());
    config.save_for_project(project_root)?;

    let result = PushResult {
        tasks_sent: tasks_count,
        requirements_sent: requirements_count,
        config_files_sent: config_files_count,
        conflicts: 0,
        server_time: chrono::Utc::now().to_rfc3339(),
    };

    print_value(result, json)
}

async fn handle_pull(hub: Arc<FileServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
    let mut config = SyncConfig::load_for_project(project_root);

    // Try to fetch cloud status if configured
    let (projects, daemons, workflows) = if config.is_configured() {
        match fetch_cloud_status(&mut config).await {
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

async fn fetch_cloud_status(
    config: &mut SyncConfig,
) -> Result<(Vec<CloudProject>, Vec<CloudDaemon>, Vec<CloudWorkflow>)> {
    let server = config.server_url()?;
    let token = get_valid_token(config).await?;

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
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
        .delete(&format!("{}/api/cli/projects/{}/daemons/{}", server.trim_end_matches('/'), project_id, daemon_id))
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

    let result =
        DeployDestroyResult { app_name: args.app_name, status: "destroyed".to_string(), machines_destroyed: 1 };

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

/// Refresh the OAuth access token using the refresh token if needed.
/// Returns true if the token was refreshed, false if it's still valid.
async fn refresh_access_token(config: &mut SyncConfig) -> Result<bool> {
    if !config.needs_token_refresh() {
        return Ok(false);
    }

    if !config.can_refresh_token() {
        anyhow::bail!("Token has expired and cannot be refreshed. Please log in again with: animus cloud login");
    }

    let server = config.server.as_ref().ok_or_else(|| anyhow::anyhow!("Server URL not configured"))?.to_string();
    let refresh_token =
        config.refresh_token.as_ref().ok_or_else(|| anyhow::anyhow!("Refresh token not available"))?.clone();
    let client_id = "animus-cli";

    let http_client = reqwest::Client::new();
    let token_resp = http_client
        .post(&format!("{}/api/auth/oauth2/token", server))
        .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh_token.as_str()), ("client_id", client_id)])
        .send()
        .await
        .context("Failed to refresh access token")?;

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        anyhow::bail!("Token refresh failed ({status}): {body}. Please log in again with: animus cloud login");
    }

    let token_data: TokenResponse = token_resp.json().await.context("Failed to parse token response")?;

    // Update the configuration with the new access token
    config.token = Some(token_data.access_token.clone());

    // Update refresh token if a new one was provided
    if let Some(ref new_refresh_token) = token_data.refresh_token {
        config.refresh_token = Some(new_refresh_token.clone());
    }

    // Update expiration time
    if let Some(expires_in) = token_data.expires_in {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in as i64);
        config.access_token_expires_at = Some(expires_at.to_rfc3339());
    }

    // Save the updated configuration
    config.save_global()?;

    Ok(true)
}

/// Get a valid access token, refreshing if necessary.
async fn get_valid_token(config: &mut SyncConfig) -> Result<String> {
    refresh_access_token(config).await?;
    config.bearer_token()
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnsureProjectRequest {
    org_id: String,
    name: String,
    repo_url: String,
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
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
        .post(&format!("{}/api/cli/projects/{}/daemons/{}/start", server.trim_end_matches('/'), project_id, daemon_id))
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
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
        .post(&format!("{}/api/cli/projects/{}/daemons/{}/stop", server.trim_end_matches('/'), project_id, daemon_id))
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
    let mut config = SyncConfig::load_for_project(project_root);
    let server = config.server_url()?;
    let token = get_valid_token(&mut config).await?;
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
        .get(&format!("{}/api/cli/projects/{}/daemons/{}", server.trim_end_matches('/'), project_id, daemon_id))
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
