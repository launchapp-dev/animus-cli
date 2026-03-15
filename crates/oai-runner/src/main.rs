mod api;
mod config;
mod runner;
mod tools;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

        #[arg(long, default_value = "50")]
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
            prompt,
        } => {
            let working_dir =
                working_dir.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));

            let json_mode = format.as_deref() == Some("json");

            let resolved_config = config::resolve_config(&model, api_base, api_key)?;

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

            if let Err(e) = runner::agent_loop::run_agent_loop(
                &client,
                &resolved_config.model_id,
                &system,
                &prompt,
                &all_tools,
                &working_dir,
                max_turns,
                &mut output,
                parsed_schema.as_ref(),
                &mcp_clients,
                session_id.as_deref(),
            )
            .await
            {
                if json_mode {
                    let err_json = serde_json::json!({
                        "type": "error",
                        "error": e.to_string()
                    });
                    eprintln!("{}", err_json);
                }
                bail!(e);
            }

            Ok(())
        }
    }
}
