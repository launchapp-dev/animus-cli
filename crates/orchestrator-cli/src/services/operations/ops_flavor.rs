//! `animus flavor` CLI subcommand — v0.5 Wave 3.
//!
//! Wraps the [`orchestrator_core::flavor`] loader and lets operators
//! inspect / install the curated flavor manifest. v0.5 ships exactly one
//! flavor (`default`); future versions may ship others without changing
//! this CLI surface.

use std::path::PathBuf;

use anyhow::{Context, Result};
use orchestrator_core::flavor::{
    list_available_flavor_names, load_flavor_in, locate_flavor_manifest_in, FlavorManifest,
};
use serde::Serialize;

use crate::cli_types::{FlavorCommand, FlavorCurrentArgs, FlavorDescribeArgs, FlavorInstallArgs};
use crate::print_value;

/// Schema constant emitted by every `animus flavor --json` envelope.
const FLAVOR_SCHEMA: &str = "animus.flavor.cli.v1";

#[derive(Debug, Serialize)]
struct FlavorListOutput {
    schema: &'static str,
    flavors: Vec<FlavorSummary>,
}

#[derive(Debug, Serialize)]
struct FlavorSummary {
    name: String,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct FlavorCurrentOutput {
    schema: &'static str,
    name: String,
    installed: bool,
    drift: Vec<FlavorDriftEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<FlavorManifest>,
}

#[derive(Debug, Serialize)]
struct FlavorDriftEntry {
    plugin: String,
    role: &'static str,
    installed: bool,
}

#[derive(Debug, Serialize)]
struct FlavorDescribeOutput {
    schema: &'static str,
    name: String,
    manifest: FlavorManifest,
}

pub(crate) async fn handle_flavor(command: FlavorCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        FlavorCommand::List => handle_flavor_list(project_root, json),
        FlavorCommand::Current(args) => handle_flavor_current(args, project_root, json),
        FlavorCommand::Info(args) => handle_flavor_describe(args, project_root, json),
        FlavorCommand::Install(args) => handle_flavor_install(args, project_root, json).await,
    }
}

fn handle_flavor_list(project_root: &str, json: bool) -> Result<()> {
    let root = std::path::Path::new(project_root);
    let names = list_available_flavor_names();
    let mut flavors = Vec::with_capacity(names.len());
    for name in names {
        let manifest_path = locate_flavor_manifest_in(root, &name);
        match load_flavor_in(root, &name)? {
            Some(manifest) => flavors.push(FlavorSummary {
                name: manifest.id.clone(),
                available: true,
                title: Some(manifest.title.clone()),
                version: Some(manifest.version.clone()),
                description: Some(manifest.description.clone()),
                manifest_path,
            }),
            None => flavors.push(FlavorSummary {
                name,
                available: false,
                title: None,
                version: None,
                description: None,
                manifest_path: None,
            }),
        }
    }
    if json {
        return print_value(FlavorListOutput { schema: FLAVOR_SCHEMA, flavors }, true);
    }
    for f in &flavors {
        match f.available {
            true => {
                println!("{} ({}) — {}", f.name, f.version.as_deref().unwrap_or("?"), f.title.as_deref().unwrap_or(""))
            }
            false => println!("{} (manifest not found)", f.name),
        }
    }
    Ok(())
}

fn handle_flavor_current(args: FlavorCurrentArgs, project_root: &str, json: bool) -> Result<()> {
    let root = std::path::Path::new(project_root);
    let manifest = load_flavor_in(root, &args.name)?;
    let drift = match &manifest {
        Some(m) => compute_drift(project_root, m)?,
        None => Vec::new(),
    };
    let installed = drift.iter().filter(|d| d.installed).count();
    let total = drift.len();
    let output = FlavorCurrentOutput {
        schema: FLAVOR_SCHEMA,
        name: args.name.clone(),
        installed: manifest.is_some() && installed == total,
        drift,
        manifest,
    };
    if json {
        return print_value(output, true);
    }
    if output.manifest.is_none() {
        println!("flavor '{}' not found on disk", output.name);
        return Ok(());
    }
    println!("flavor: {} ({}/{}) installed", output.name, installed, total);
    for entry in &output.drift {
        let mark = if entry.installed { "ok" } else { "missing" };
        println!("  [{mark}] {} ({})", entry.plugin, entry.role);
    }
    Ok(())
}

fn handle_flavor_describe(args: FlavorDescribeArgs, project_root: &str, json: bool) -> Result<()> {
    let root = std::path::Path::new(project_root);
    let manifest = load_flavor_in(root, &args.name)?
        .with_context(|| format!("flavor manifest '{}' not found on disk", args.name))?;
    if json {
        return print_value(FlavorDescribeOutput { schema: FLAVOR_SCHEMA, name: args.name, manifest }, true);
    }
    let text = toml::to_string_pretty(&manifest).context("failed to serialize flavor manifest to TOML")?;
    println!("{text}");
    Ok(())
}

async fn handle_flavor_install(args: FlavorInstallArgs, project_root: &str, json: bool) -> Result<()> {
    // `animus flavor install <name>` delegates to the manifest-driven
    // `animus plugin install-defaults --flavor <name>` path: everything
    // the manifest marks `required` installs; `--include-recommended`
    // adds the recommended set. One code path, one required-set
    // definition (`FlavorManifest::required_plugins`).
    use crate::cli_types::PluginCommand;
    use crate::cli_types::PluginInstallDefaultsArgs;
    use crate::services::operations::handle_plugin;
    // Pre-load the manifest just to confirm it exists; the manifest
    // loader inside `install-defaults` will do the same work for the
    // resolved targets, but failing-fast here gives a clearer error
    // message when an unknown flavor name is passed.
    if load_flavor_in(std::path::Path::new(project_root), &args.name)?.is_none() {
        anyhow::bail!(
            "flavor manifest '{}' not found on disk; expected at <repo>/flavors/{}.toml",
            args.name,
            args.name
        );
    }
    let install_args = PluginInstallDefaultsArgs {
        json,
        force: args.force,
        yes: args.yes,
        flavor: args.name.clone(),
        include_recommended: args.include_recommended,
        include_oai_agent: false,
        include_subjects: false,
        include_transports: false,
        plugin_dir: None,
        force_rewrite_lockfile: false,
    };
    // Delegate to `install-defaults`, which already emits a JSON envelope
    // when `install_args.json` is set. Do NOT emit a second envelope from
    // this fn — codex P2: stdout for `animus --json flavor install` would
    // otherwise contain two concatenated JSON objects, breaking strict
    // line-delimited / single-JSON consumers.
    handle_plugin(PluginCommand::InstallDefaults(install_args), project_root, json).await
}

/// Compare the set of installed plugins on disk against the manifest's
/// declared plugins.  v0.5: a slug is considered "installed" iff the
/// plugin install directory contains a directory whose name matches the
/// repo basename. The comparison set is
/// [`FlavorManifest::required_plugins`] — the exact same required-set
/// function that drives `animus plugin install-defaults --flavor` /
/// `animus flavor install`, so the drift report and the install plan
/// cannot disagree.
fn compute_drift(_project_root: &str, manifest: &FlavorManifest) -> Result<Vec<FlavorDriftEntry>> {
    let install_dir = orchestrator_plugin_host::plugin_install_dir();
    let installed_basenames: std::collections::HashSet<String> = std::fs::read_dir(&install_dir)
        .map(|iter| iter.flatten().filter_map(|entry| entry.file_name().to_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let mut entries = Vec::new();
    for (role, slug) in manifest.required_plugins() {
        let basename = slug.rsplit('/').next().unwrap_or(&slug).to_string();
        entries.push(FlavorDriftEntry {
            plugin: slug.clone(),
            role,
            installed: installed_basenames.contains(&basename),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `animus flavor current` (drift) and `animus flavor install` /
    /// `animus plugin install-defaults --flavor` must agree on what the
    /// flavor requires. Both sides call
    /// [`FlavorManifest::required_plugins`]; this guard asserts the drift
    /// report's `(plugin, role)` set is exactly that shared required set.
    #[test]
    fn drift_report_compares_against_the_shared_required_set() {
        let manifest: FlavorManifest = toml::from_str(
            r#"
schema = "animus.flavor.v1"
id = "default"
version = "0.5.0"
title = "Test"
description = "Drift fixture."

[providers]
required = ["launchapp-dev/animus-provider-claude"]
recommended = ["launchapp-dev/animus-provider-codex"]

[subjects]
required = ["launchapp-dev/animus-subject-default", "launchapp-dev/animus-subject-requirements"]

[workflow_runner]
required = ["launchapp-dev/animus-workflow-runner-default"]

[queue]
required = ["launchapp-dev/animus-queue-default"]
"#,
        )
        .unwrap();

        let drift = compute_drift("/nonexistent-project-root", &manifest).unwrap();
        let drift_pairs: Vec<(&str, String)> = drift.iter().map(|e| (e.role, e.plugin.clone())).collect();
        assert_eq!(
            drift_pairs,
            manifest.required_plugins(),
            "flavor-current drift set must equal FlavorManifest::required_plugins (one shared function)"
        );
        assert!(
            !drift.iter().any(|e| e.plugin.contains("codex")),
            "recommended plugins are not part of the drift comparison set"
        );
    }
}
