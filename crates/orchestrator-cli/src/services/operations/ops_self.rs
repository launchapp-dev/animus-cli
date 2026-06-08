use anyhow::Result;
use serde::Serialize;

use crate::cli_types::{SelfCommand, SelfUpdateArgs};
use crate::print_value;
use crate::services::self_update::{
    resolve_effective_config_block, run_manual_update, ManualUpdateOptions, UpdateOutcome,
};

const SELF_SCHEMA: &str = "animus.self.cli.v1";

#[derive(Debug, Serialize)]
struct SelfUpdateOutput {
    schema: &'static str,
    status: &'static str,
    current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed: Option<String>,
}

pub(crate) async fn handle_self(command: SelfCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        SelfCommand::Update(args) => handle_self_update(args, project_root, json).await,
    }
}

async fn handle_self_update(args: SelfUpdateArgs, project_root: &str, json: bool) -> Result<()> {
    let block = resolve_effective_config_block(project_root);
    let options = ManualUpdateOptions {
        check_only: args.check_only,
        force: args.force,
        prerelease_override: args.prerelease,
        assume_yes: args.yes,
        channel_override: None,
    };
    let outcome = run_manual_update(block.as_ref(), options).await?;
    let (status, current, latest, installed) = match outcome {
        UpdateOutcome::UpToDate { current } => ("up_to_date", current, None, None),
        UpdateOutcome::Available { current, latest } => ("available", current, Some(latest), None),
        UpdateOutcome::Installed { previous, installed } => ("installed", previous, None, Some(installed)),
    };

    if !json {
        match status {
            "up_to_date" => eprintln!("animus is up to date (v{current})."),
            "available" => {
                if let Some(latest) = latest.as_deref() {
                    eprintln!("Update available: v{current} -> v{latest}.");
                }
            }
            "installed" => {
                if let Some(installed) = installed.as_deref() {
                    eprintln!("Installed v{installed} (was v{current}).");
                }
            }
            _ => {}
        }
    }

    let output = SelfUpdateOutput { schema: SELF_SCHEMA, status, current, latest, installed };
    print_value(output, json)?;

    // `--check-only` is scriptable: exit 0 when an update is available,
    // exit 1 when already up-to-date. Done after the JSON envelope so
    // `animus self update --check-only --json | jq` still works.
    if args.check_only && status == "up_to_date" {
        std::process::exit(1);
    }
    Ok(())
}
