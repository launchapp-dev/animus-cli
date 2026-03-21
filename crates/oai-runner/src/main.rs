mod api;
mod config;
mod pricing;
mod runner;
mod tools;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")");

#[derive(Parser)]
#[command(name = "ao-oai-runner", version = VERSION, about = "OpenAI-compatible agent runner for AO")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        #[arg(short, long)]
        model: String,

        #[arg(long)]
        api_base: Option<String>,

        #[arg(long)]
        api_key: Option<String>,

        #[arg(long)]
        format: Option<String>,

        #[arg(long)]
        system_prompt: Option<PathBuf>,

        #[arg(long)]
        working_dir: Option<PathBuf>,

        #[arg(long, default_value = "200")]
        max_turns: usize,

        #[arg(long, default_value = "600")]
        idle_timeout: u64,

        #[arg(long)]
        response_schema: Option<String>,

        #[arg(long)]
        read_only: bool,

        #[arg(long)]
        mcp_config: Option<String>,

        #[arg(long)]
        session_id: Option<String>,

        #[arg(long, help = "Disable sending response_format with json_schema on API requests")]
        no_response_format: bool,

        #[arg(long, default_value = "128000", help = "Context window token limit for the model")]
        context_limit: usize,

        #[arg(long, default_value = "16384", help = "Maximum output tokens per response")]
        max_tokens: usize,

        #[arg(long, help = "Cancel run with exit code 2 when accumulated cost exceeds this USD amount")]
        max_cost_usd: Option<f64>,

        prompt: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            model,
            api_base,
            api_key,
            format,
            system_prompt,
            working_dir,
            max_turns,
            idle_timeout,
            response_schema,
            read_only,
            mcp_config,
            session_id,
            no_response_format,
            context_limit,
            max_tokens,
            max_cost_usd,
            prompt,
        } => {
            let working_dir =
                working_dir.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));

            let json_mode = format.as_deref() == Some("json");

            let resolved_config = config::resolve_config(&model, api_base, api_key)?;
            let api_base_url = resolved_config.api_base.clone();

            let system = match system_prompt {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("Failed to read system prompt {}: {}", path.display(), e))?,
                None => String::new(),
            };

            let parsed_schema = match &response_schema {
                Some(schema_str) => {
                    let schema: serde_json::Value = serde_json::from_str(schema_str)
                        .map_err(|e| anyhow::anyhow!("Invalid --response-schema JSON: {}", e))?;
                    Some(schema)
                }
                None => None,
            };

            let client = api::client::ApiClient::new(resolved_config.api_base, resolved_config.api_key, idle_timeout);

            let native_tools = if read_only {
                tools::definitions::read_only_tool_definitions()
            } else {
                tools::definitions::all_tool_definitions()
            };

            let mcp_configs: Vec<tools::mcp_client::McpServerConfig> = match &mcp_config {
                Some(json_str) => {
                    serde_json::from_str(json_str).map_err(|e| anyhow::anyhow!("Invalid --mcp-config JSON: {}", e))?
                }
                None => vec![],
            };
            let mut mcp_clients = tools::mcp_client::connect_all(&mcp_configs).await?;
            let mcp_tool_defs = tools::mcp_client::fetch_all_tool_definitions(&mut mcp_clients).await?;
            let all_tools = tools::definitions::merge_tools(native_tools, mcp_tool_defs);

            let mut output = runner::output::OutputFormatter::new(json_mode);

            let cancel_token = CancellationToken::new();
            let cancel_for_signal = cancel_token.clone();

            tokio::spawn(async move {
                let ctrl_c = tokio::signal::ctrl_c();
                #[cfg(unix)]
                {
                    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("failed to register SIGTERM handler");
                    tokio::select! {
                        _ = ctrl_c => {},
                        _ = sigterm.recv() => {},
                    }
                }
                #[cfg(not(unix))]
                {
                    ctrl_c.await.ok();
                }
                eprintln!("[oai-runner] Shutdown signal received, cancelling...");
                cancel_for_signal.cancel();
            });

            let structured_output = if no_response_format { None } else { Some(resolved_config.structured_output) };

            let result = runner::agent_loop::run_agent_loop(
                &client,
                &resolved_config.model_id,
                &api_base_url,
                &system,
                &prompt,
                &all_tools,
                &working_dir,
                max_turns,
                &mut output,
                parsed_schema.as_ref(),
                &mcp_clients,
                session_id.as_deref(),
                structured_output,
                cancel_token,
                context_limit,
                max_tokens,
                max_cost_usd,
            )
            .await;

            if let Err(e) = result {
                if let Some(budget_err) = e.downcast_ref::<runner::agent_loop::BudgetExceededError>() {
                    if json_mode {
                        let err_json = serde_json::json!({
                            "type": "error",
                            "error": "budget_exceeded",
                            "cost_usd": budget_err.spent_usd,
                            "limit_usd": budget_err.limit_usd
                        });
                        println!("{}", err_json);
                    } else {
                        eprintln!(
                            "[oai-runner] Budget exceeded: ${:.6} spent, limit ${:.6}",
                            budget_err.spent_usd, budget_err.limit_usd
                        );
                    }
                    std::process::exit(2);
                }
                if json_mode {
                    let err_json = serde_json::json!({
                        "type": "error",
                        "error": e.to_string()
                    });
                    println!("{}", err_json);
                }
                bail!(e);
            }

            Ok(())
        }
    }
}
