use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use orchestrator_config::McpServerDefinition;
use protocol::orchestrator::{McpServerHealth, McpServerHealthStatus};
use tokio::process::Child;

const MAX_RESTARTS: u32 = 3;
const STARTUP_PROBE_MILLIS: u64 = 1_000;
const STARTUP_PROBE_POLL_MILLIS: u64 = 100;
const RESTART_BACKOFF_BASE_SECS: u64 = 5;

struct ManagedMcpServer {
    name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    child: Option<Child>,
    restart_count: u32,
    status: McpServerHealthStatus,
    last_started: Option<Instant>,
}

impl ManagedMcpServer {
    fn new(name: String, def: McpServerDefinition) -> Self {
        Self {
            name,
            command: def.command,
            args: def.args,
            env: def.env,
            child: None,
            restart_count: 0,
            status: McpServerHealthStatus::Starting,
            last_started: None,
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }

    fn spawn(&mut self) -> bool {
        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&self.args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.last_started = Some(Instant::now());
                true
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "warn",
                        "event": "mcp_server_spawn_failed",
                        "server": self.name,
                        "error": e.to_string(),
                    })
                );
                false
            }
        }
    }

    fn is_still_alive(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            _ => false,
        }
    }
}

pub struct McpServerManager {
    servers: Vec<ManagedMcpServer>,
}

impl McpServerManager {
    pub fn new(servers: BTreeMap<String, McpServerDefinition>) -> Self {
        let managed = servers
            .into_iter()
            .filter(|(_, def)| !def.command.is_empty())
            .map(|(name, def)| ManagedMcpServer::new(name, def))
            .collect();
        Self { servers: managed }
    }

    pub async fn start_all(&mut self) {
        for server in &mut self.servers {
            let spawned = server.spawn();
            if !spawned {
                server.status = McpServerHealthStatus::Dead;
                continue;
            }

            let probe_deadline = Duration::from_millis(STARTUP_PROBE_MILLIS);
            let poll_interval = Duration::from_millis(STARTUP_PROBE_POLL_MILLIS);
            let start = Instant::now();
            loop {
                if !server.is_still_alive() {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "level": "warn",
                            "event": "mcp_server_exited_during_startup",
                            "server": server.name,
                        })
                    );
                    server.status = McpServerHealthStatus::Dead;
                    break;
                }
                if start.elapsed() >= probe_deadline {
                    server.status = McpServerHealthStatus::Running;
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "level": "info",
                            "event": "mcp_server_started",
                            "server": server.name,
                            "pid": server.pid(),
                        })
                    );
                    break;
                }
                tokio::time::sleep(poll_interval).await;
            }
        }
    }

    pub async fn tick(&mut self) {
        for server in &mut self.servers {
            if matches!(
                server.status,
                McpServerHealthStatus::Dead | McpServerHealthStatus::Unhealthy
            ) {
                continue;
            }

            if server.child.is_none() {
                continue;
            }

            if server.is_still_alive() {
                server.status = McpServerHealthStatus::Running;
                continue;
            }

            if server.restart_count >= MAX_RESTARTS {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "warn",
                        "event": "mcp_server_max_restarts_exceeded",
                        "server": server.name,
                        "restart_count": server.restart_count,
                    })
                );
                server.status = McpServerHealthStatus::Unhealthy;
                continue;
            }

            let backoff = Duration::from_secs(
                RESTART_BACKOFF_BASE_SECS * (server.restart_count as u64 + 1),
            );
            if let Some(last_started) = server.last_started {
                if last_started.elapsed() < backoff {
                    continue;
                }
            }

            server.restart_count += 1;
            server.status = McpServerHealthStatus::Restarting;
            eprintln!(
                "{}",
                serde_json::json!({
                    "level": "warn",
                    "event": "mcp_server_restarting",
                    "server": server.name,
                    "restart_count": server.restart_count,
                })
            );

            let spawned = server.spawn();
            if spawned {
                server.status = McpServerHealthStatus::Running;
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "info",
                        "event": "mcp_server_restarted",
                        "server": server.name,
                        "pid": server.pid(),
                        "restart_count": server.restart_count,
                    })
                );
            } else {
                server.status = McpServerHealthStatus::Dead;
            }
        }
    }

    pub async fn stop_all(&mut self) {
        for server in &mut self.servers {
            if let Some(mut child) = server.child.take() {
                if let Some(pid) = child.id() {
                    protocol::graceful_kill_process(pid as i32);
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    child.wait(),
                )
                .await;
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "info",
                        "event": "mcp_server_stopped",
                        "server": server.name,
                    })
                );
            }
        }
    }

    pub fn health(&self) -> Vec<McpServerHealth> {
        self.servers
            .iter()
            .map(|s| McpServerHealth {
                name: s.name.clone(),
                healthy: matches!(s.status, McpServerHealthStatus::Running),
                status: s.status,
                restart_count: s.restart_count,
                pid: s.pid(),
            })
            .collect()
    }

    pub fn first_mcp_runtime_config(&self) -> Option<protocol::McpRuntimeConfig> {
        self.servers
            .iter()
            .find(|s| matches!(s.status, McpServerHealthStatus::Running))
            .map(|s| {
                let args_json = if s.args.is_empty() {
                    None
                } else {
                    serde_json::to_string(&s.args).ok()
                };
                protocol::McpRuntimeConfig {
                    transport: Some("stdio".to_string()),
                    stdio_command: Some(s.command.clone()),
                    stdio_args_json: args_json,
                    ..Default::default()
                }
            })
    }

}
