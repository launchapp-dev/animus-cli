use anyhow::Result;
use protocol::AutoUpdateChannel;
use serde::Serialize;

use crate::cli_types::{UpdateArgs, UpdateChannelArg};
use crate::print_value;
use crate::services::self_update::{
    resolve_effective_config_block, run_manual_update, ManualUpdateOptions, UpdateOutcome,
};

const UPDATE_SCHEMA: &str = "animus.update.cli.v1";

#[derive(Debug, Serialize)]
struct UpdateOutput {
    schema: &'static str,
    action: &'static str,
    current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed: Option<String>,
    channel: &'static str,
}

pub(crate) async fn handle_update(args: UpdateArgs, project_root: &str, json: bool) -> Result<()> {
    let block = resolve_effective_config_block(project_root);
    let channel = channel_from_arg(args.channel);
    // The top-level `animus update --channel <c>` is an explicit user
    // selection — it must beat both `auto_update.channel` in config and
    // any `prerelease_override` boolean. Use `channel_override` so a
    // `--channel stable` request never silently picks up a prerelease
    // from a prerelease-configured project.
    let options = ManualUpdateOptions {
        check_only: args.check,
        force: false,
        prerelease_override: false,
        assume_yes: args.yes,
        channel_override: Some(channel),
    };

    let outcome = run_manual_update(block.as_ref(), options).await?;
    let (action, current, latest, installed) = match outcome {
        UpdateOutcome::UpToDate { current } => ("up_to_date", current, None, None),
        UpdateOutcome::Available { current, latest } => ("available", current, Some(latest), None),
        UpdateOutcome::Installed { previous, installed } => ("installed", previous, None, Some(installed)),
    };
    let channel_label = channel_label(channel);

    if !json {
        match action {
            "up_to_date" => eprintln!("animus is up to date (v{current}, channel={channel_label})."),
            "available" => {
                if let Some(latest) = latest.as_deref() {
                    eprintln!(
                        "Update available on channel {channel_label}: v{current} -> v{latest}. Re-run with --yes to install."
                    );
                }
            }
            "installed" => {
                if let Some(installed) = installed.as_deref() {
                    eprintln!(
                        "Updated animus v{current} -> v{installed}. Restart any running daemons (`animus daemon restart --autonomous`)."
                    );
                }
            }
            _ => {}
        }
    }

    let output = UpdateOutput { schema: UPDATE_SCHEMA, action, current, latest, installed, channel: channel_label };
    print_value(output, json)?;

    if args.check && action == "up_to_date" {
        std::process::exit(1);
    }
    Ok(())
}

fn channel_from_arg(arg: UpdateChannelArg) -> AutoUpdateChannel {
    match arg {
        UpdateChannelArg::Stable => AutoUpdateChannel::Stable,
        UpdateChannelArg::Nightly => AutoUpdateChannel::Prerelease,
    }
}

fn channel_label(channel: AutoUpdateChannel) -> &'static str {
    match channel {
        AutoUpdateChannel::Stable => "stable",
        AutoUpdateChannel::Prerelease => "nightly",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_arg_maps_to_auto_update_channel() {
        assert!(matches!(channel_from_arg(UpdateChannelArg::Stable), AutoUpdateChannel::Stable));
        assert!(matches!(channel_from_arg(UpdateChannelArg::Nightly), AutoUpdateChannel::Prerelease));
    }

    #[test]
    fn channel_label_is_stable_or_nightly() {
        assert_eq!(channel_label(AutoUpdateChannel::Stable), "stable");
        assert_eq!(channel_label(AutoUpdateChannel::Prerelease), "nightly");
    }
}
