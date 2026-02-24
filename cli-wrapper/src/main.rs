//! CLI Wrapper - Test and manage different AI coding CLIs

use clap::{Parser, Subcommand};
use cli_wrapper::{CliRegistry, CliTester, CliType, Config, TestSuite};
use colored::Colorize;
use std::path::PathBuf;
use tracing::error;

#[derive(Parser)]
#[command(name = "cli-wrapper")]
#[command(about = "Test and manage different AI coding CLIs", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Config file path
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover installed CLIs
    Discover,

    /// List all discovered CLIs
    List,

    /// Test a specific CLI or all CLIs
    Test {
        /// CLI to test (claude, codex, gemini) - omit to test all
        cli: Option<String>,

        /// Test suite to run (basic, file-ops, code-gen)
        #[arg(short, long, default_value = "basic")]
        suite: String,
    },

    /// Run health check on CLIs
    Health {
        /// CLI to check - omit to check all
        cli: Option<String>,
    },

    /// Show CLI capabilities
    Info {
        /// CLI to show info for
        cli: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Setup logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt().with_env_filter(log_level).init();

    // Load config
    let config = if let Some(config_path) = cli.config {
        Config::load_from_file(&config_path)?
    } else {
        Config::default()
    };

    // Create registry
    let mut registry = CliRegistry::new();

    match cli.command {
        Commands::Discover => {
            println!("{}", "Discovering installed CLIs...".bold());
            let count = registry.discover_clis().await?;
            println!("{}", format!("✓ Found {} CLI(s)", count).green().bold());
        }

        Commands::List => {
            registry.discover_clis().await?;
            let statuses = registry.check_all_status().await;

            println!("{}", "\nInstalled CLIs:".bold());
            println!("{}", "─".repeat(60));

            for (cli_type, status) in statuses {
                let status_str = match status {
                    cli_wrapper::CliStatus::Available => "✓ Available".green(),
                    cli_wrapper::CliStatus::NotInstalled => "✗ Not Installed".red(),
                    cli_wrapper::CliStatus::NotAuthenticated => "⚠ Not Authenticated".yellow(),
                    cli_wrapper::CliStatus::Error(ref e) => format!("✗ Error: {}", e).red(),
                };

                println!("{:15} {}", cli_type.display_name(), status_str);
            }
        }

        Commands::Test { cli, suite } => {
            registry.discover_clis().await?;

            let test_suite = match suite.as_str() {
                "basic" => TestSuite::basic_verification(),
                "file-ops" => TestSuite::file_operations(),
                "code-gen" => TestSuite::code_generation(),
                _ => {
                    error!("Unknown test suite: {}", suite);
                    return Ok(());
                }
            };

            println!(
                "{}",
                format!("\nRunning test suite: {}", test_suite.name).bold()
            );
            println!("{}", "─".repeat(60));

            // Create test workspace directory if it doesn't exist
            if !config.test_workspace_dir.exists() {
                std::fs::create_dir_all(&config.test_workspace_dir)?;
            }

            let tester = CliTester::new().with_temp_dir(config.test_workspace_dir.clone());

            let results = if let Some(cli_name) = cli {
                let cli_type = parse_cli_type(&cli_name)?;
                if let Some(cli_impl) = registry.get(cli_type) {
                    tester.test_cli(cli_impl, &test_suite).await?
                } else {
                    error!("CLI not found: {}", cli_name);
                    return Ok(());
                }
            } else {
                tester.test_all_clis(&registry, &test_suite).await?
            };

            // Print results
            for result in &results {
                let status = if result.passed {
                    "✓ PASS".green()
                } else {
                    "✗ FAIL".red()
                };

                println!(
                    "{} {} - {} ({}ms)",
                    status,
                    result.test_name,
                    result.cli_type.display_name(),
                    result.duration_ms
                );

                if !result.failures.is_empty() {
                    for failure in &result.failures {
                        println!("    {}", failure.dimmed());
                    }
                }

                if let Some(ref error) = result.error {
                    println!("    {}", error.red().dimmed());
                }
            }

            // Summary
            let passed = results.iter().filter(|r| r.passed).count();
            let total = results.len();
            println!("\n{}", "─".repeat(60));
            println!(
                "Summary: {}/{} tests passed",
                if passed == total {
                    format!("{}", passed).green()
                } else {
                    format!("{}", passed).yellow()
                },
                total
            );
        }

        Commands::Health { cli } => {
            registry.discover_clis().await?;
            let tester = CliTester::new();

            println!("{}", "\nRunning health checks...".bold());
            println!("{}", "─".repeat(60));

            let clis_to_check = if let Some(cli_name) = cli {
                let cli_type = parse_cli_type(&cli_name)?;
                if let Some(cli_impl) = registry.get(cli_type) {
                    vec![cli_impl]
                } else {
                    error!("CLI not found: {}", cli_name);
                    return Ok(());
                }
            } else {
                registry.all()
            };

            for cli_impl in clis_to_check {
                let result = tester.health_check(cli_impl).await?;

                let status = if result.passed {
                    "✓ HEALTHY".green()
                } else {
                    "✗ UNHEALTHY".red()
                };

                println!(
                    "{} {} ({}ms)",
                    status,
                    result.cli_type.display_name(),
                    result.duration_ms
                );

                if !result.output.is_empty() {
                    println!("    {}", result.output.dimmed());
                }

                if let Some(ref error) = result.error {
                    println!("    {}", error.red().dimmed());
                }
            }
        }

        Commands::Info { cli } => {
            registry.discover_clis().await?;
            let cli_type = parse_cli_type(&cli)?;

            if let Some(cli_impl) = registry.get(cli_type) {
                let metadata = cli_impl.metadata();

                println!("\n{}", cli_type.display_name().bold());
                println!("{}", "─".repeat(60));
                println!("Executable: {:?}", metadata.executable_path);
                if let Some(ref version) = metadata.version {
                    println!("Version: {}", version);
                }
                println!("\nCapabilities:");
                println!(
                    "  File editing: {}",
                    if metadata.capabilities.supports_file_editing {
                        "✓".green()
                    } else {
                        "✗".red()
                    }
                );
                println!(
                    "  Streaming: {}",
                    if metadata.capabilities.supports_streaming {
                        "✓".green()
                    } else {
                        "✗".red()
                    }
                );
                println!(
                    "  Tool use: {}",
                    if metadata.capabilities.supports_tool_use {
                        "✓".green()
                    } else {
                        "✗".red()
                    }
                );
                println!(
                    "  Vision: {}",
                    if metadata.capabilities.supports_vision {
                        "✓".green()
                    } else {
                        "✗".red()
                    }
                );
                println!(
                    "  MCP Support: {}",
                    if metadata.capabilities.supports_mcp {
                        "✓".green()
                    } else {
                        "✗".red()
                    }
                );
                if let Some(max_tokens) = metadata.capabilities.max_context_tokens {
                    println!("  Max context: {} tokens", max_tokens);
                }
                if let Some(ref endpoint) = metadata.capabilities.mcp_endpoint {
                    println!("  MCP Endpoint: {}", endpoint);
                }
            } else {
                error!("CLI not found: {}", cli);
            }
        }
    }

    Ok(())
}

fn parse_cli_type(name: &str) -> anyhow::Result<CliType> {
    match name.to_lowercase().as_str() {
        "claude" => Ok(CliType::Claude),
        "codex" => Ok(CliType::Codex),
        "gemini" => Ok(CliType::Gemini),
        "opencode" => Ok(CliType::OpenCode),
        "aider" => Ok(CliType::Aider),
        "cursor" => Ok(CliType::Cursor),
        "cline" => Ok(CliType::Cline),
        _ => Err(anyhow::anyhow!("Unknown CLI type: {}", name)),
    }
}
