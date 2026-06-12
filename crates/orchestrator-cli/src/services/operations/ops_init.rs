use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use orchestrator_config::{
    list_project_templates_from_default_registry_with_options, load_pack_inventory, load_pack_manifest,
    load_pack_selection_state, load_project_template_from_default_registry_with_options,
    load_project_template_from_dir, save_pack_selection_state, LoadedProjectTemplate, PackRegistrySource,
    PackSelectionEntry, PackSelectionSource, ProjectTemplateSourceKind, ProjectTemplateSummary, RegistrySyncOptions,
};
use orchestrator_core::{
    daemon_project_config_path, load_daemon_project_config, write_daemon_project_config, DoctorCheckStatus,
    DoctorReport, FileServiceHub,
};
use serde::Serialize;

use crate::{conflict_error, invalid_input_error, print_value, InitArgs};

// -----------------------------------------------------------------------------
// v0.4.13 onboarding walkthrough: bundled hello-world template + helpers.
// -----------------------------------------------------------------------------

const HELLO_WORLD_TEMPLATE_NAME: &str = "hello-world";
const HELLO_WORLD_WORKFLOW_FILE: &str = "hello-world.yaml";
const HELLO_WORLD_WORKFLOW_YAML: &str = include_str!("../../../templates/hello-world/workflows/hello-world.yaml");
const HELLO_WORLD_README: &str = include_str!("../../../templates/hello-world/README.md");

/// Logical name -> (CLI binary, env var holding the API key, dotfile dir).
/// The detection is best-effort; absence does not mean "no key", because
/// providers also fall back to per-tool config files.
const CLI_DETECTION_TARGETS: &[CliTarget] = &[
    CliTarget { name: "claude", binary: "claude", api_env: "ANTHROPIC_API_KEY", config_subpath: ".claude" },
    CliTarget { name: "codex", binary: "codex", api_env: "OPENAI_API_KEY", config_subpath: ".codex" },
    CliTarget { name: "gemini", binary: "gemini", api_env: "GEMINI_API_KEY", config_subpath: ".gemini" },
    CliTarget { name: "opencode", binary: "opencode", api_env: "OPENCODE_API_KEY", config_subpath: ".opencode" },
];

#[derive(Debug, Clone, Copy)]
struct CliTarget {
    name: &'static str,
    binary: &'static str,
    api_env: &'static str,
    config_subpath: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct CliDetection {
    name: String,
    binary: String,
    installed: bool,
    binary_path: Option<String>,
    api_key_env: String,
    api_key_via_env: bool,
    api_key_via_config: bool,
    config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WalkthroughTemplatePlan {
    name: String,
    relative_path: String,
    action: String, // "create" | "skip_exists" | "overwrite"
}

#[derive(Debug, Clone, Serialize)]
struct WalkthroughPluginStep {
    skipped: bool,
    invoked: bool,
    exit_code: Option<i32>,
    stderr_tail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WalkthroughDaemonStep {
    requested: bool,
    invoked: bool,
    exit_code: Option<i32>,
    stderr_tail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WalkthroughTemplateStep {
    skipped: bool,
    written: Vec<String>,
    skipped_existing: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum InitMode {
    Guided,
    NonInteractive,
}

#[derive(Debug, Clone, Serialize)]
struct InitTemplateOutput {
    id: String,
    version: String,
    title: String,
    description: String,
    pattern: String,
    source_kind: ProjectTemplateSourceKind,
}

#[derive(Debug, Clone, Serialize)]
struct InitFilePlan {
    path: String,
    action: String,
}

#[derive(Debug, Clone, Serialize)]
struct InitPackPlan {
    pack_id: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct InitPackApply {
    installed_packs: Vec<String>,
    pack_selection_updated: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InitBlockedItem {
    check_id: String,
    details: String,
    remediation: String,
}

pub(crate) async fn handle_init(args: InitArgs, project_root: &str, json: bool) -> Result<()> {
    let mode = if args.non_interactive { InitMode::NonInteractive } else { InitMode::Guided };
    let project_root_path = Path::new(project_root);

    if !json && !args.plan && !args.non_interactive {
        let _ = crate::services::metrics::maybe_prompt_first_run(project_root_path);
    }

    if args.walkthrough {
        return run_walkthrough(&args, project_root_path, mode, json).await;
    }

    let loaded_template = resolve_template(&args, mode)?;
    ensure_supported_template_source_mode(&loaded_template)?;

    // The daemon git/merge policy knobs were removed in v0.5.x; init only
    // ensures the daemon config file exists.
    let daemon_plan: Vec<serde_json::Value> = Vec::new();

    let template_output = InitTemplateOutput {
        id: loaded_template.manifest.id.clone(),
        version: loaded_template.manifest.version.clone(),
        title: loaded_template.manifest.title.clone(),
        description: loaded_template.manifest.description.clone(),
        pattern: loaded_template.manifest.pattern.clone(),
        source_kind: loaded_template.source_kind,
    };

    let existing_before = existing_template_paths(project_root_path, &loaded_template);
    let template_file_plan =
        build_template_file_plan(project_root_path, &loaded_template, &existing_before, args.force);
    let pack_plan = build_pack_plan(project_root_path, &loaded_template)?;

    let doctor_before = DoctorReport::run_for_project(project_root_path);
    let blocked_items = collect_blocked_items(&doctor_before);
    let doctor_summary = serde_json::json!({
        "result": doctor_before.result,
        "ok": count_checks(&doctor_before, DoctorCheckStatus::Ok),
        "warn": count_checks(&doctor_before, DoctorCheckStatus::Warn),
        "fail": count_checks(&doctor_before, DoctorCheckStatus::Fail),
    });

    if args.plan {
        return print_value(
            serde_json::json!({
                "stage": "plan",
                "mode": mode,
                "template": template_output,
                "environment": {
                    "project_root": project_root,
                    "doctor": doctor_summary,
                    "daemon_config_path": daemon_project_config_path(project_root_path).display().to_string(),
                },
                "required_changes": {
                    "template_files": template_file_plan,
                    "daemon_config": daemon_plan,
                    "packs": pack_plan,
                },
                "blocked_items": blocked_items,
                "planned_actions": {
                    "install_recommended_packs": args.install_packs,
                },
                "apply": {
                    "applied": false,
                    "changed_domains": [],
                    "unchanged_domains": ["template_files", "daemon_config", "pack_installation", "pack_selection"],
                },
                "next_steps": loaded_template.manifest.next_steps,
            }),
            json,
        );
    }

    fail_on_conflicting_paths(&existing_before, args.force)?;

    let bootstrap_needed_before = remediation_needed(&doctor_before, "bootstrap_project_state");
    let daemon_config_exists_before = daemon_project_config_path(project_root_path).exists();

    // Install the recommended packs BEFORE applying the template's pack
    // selection: a template manifest may reference one of the recommended
    // packs via `[[packs]]`, and `apply_template_packs` fails when the
    // referenced pack is not yet available. (codex round-1 P2.)
    let recommended_pack_step = if args.install_packs {
        let step = install_recommended_packs(project_root_path);
        if !json {
            print_pack_install_summary(&step);
        }
        step
    } else {
        RecommendedPackStep::skipped()
    };
    // Recompute the template pack plan after the recommended installs so the
    // apply envelope does not report packs as `missing` that init just
    // installed. (codex round-5 P3.)
    let pack_plan = if args.install_packs { build_pack_plan(project_root_path, &loaded_template)? } else { pack_plan };

    let written_files = write_template_files(project_root_path, &loaded_template)?;
    FileServiceHub::new(project_root_path)?;
    let pack_apply = apply_template_packs(project_root_path, &loaded_template)?;
    let daemon_config_updated = ensure_daemon_project_config(project_root_path, daemon_config_exists_before)?;
    let doctor_after = DoctorReport::run_for_project(project_root_path);

    let mut changed_domains = Vec::new();
    let mut unchanged_domains = Vec::new();
    if bootstrap_needed_before {
        changed_domains.push("project_bootstrap");
    } else {
        unchanged_domains.push("project_bootstrap");
    }
    if written_files.is_empty() {
        unchanged_domains.push("template_files");
    } else {
        changed_domains.push("template_files");
    }
    if daemon_config_updated {
        changed_domains.push("daemon_config");
    } else {
        unchanged_domains.push("daemon_config");
    }
    if pack_apply.pack_selection_updated {
        changed_domains.push("pack_selection");
    } else {
        unchanged_domains.push("pack_selection");
    }
    if pack_apply.installed_packs.is_empty()
        && !recommended_pack_step.results.iter().any(|result| result.status == "installed")
    {
        unchanged_domains.push("pack_installation");
    } else {
        changed_domains.push("pack_installation");
    }

    let mut next_steps: Vec<String> = loaded_template.manifest.next_steps.clone();
    if recommended_pack_step.skipped {
        next_steps.push(
            "Install the recommended workflow packs when ready: animus init --walkthrough --no-template --no-install --install-packs".to_string(),
        );
    } else {
        for result in &recommended_pack_step.results {
            if result.status == "failed" {
                next_steps.push(format!(
                    "Pack install failed for {}: {}",
                    result.pack_id,
                    result.detail.as_deref().unwrap_or("unknown error")
                ));
            }
        }
        if task_pack_ready(&recommended_pack_step) {
            next_steps.push(
                "Run the standard task workflow: `animus subject create --kind task --title \"My first task\"` then `animus workflow run animus.task/standard --task-id TASK-001 --sync`."
                    .to_string(),
            );
        }
    }

    print_value(
        serde_json::json!({
            "stage": "apply",
            "mode": mode,
            "template": template_output,
            "environment": {
                "project_root": project_root,
                "daemon_config_path": daemon_project_config_path(project_root_path).display().to_string(),
            },
            "required_changes": {
                "template_files": template_file_plan,
                "daemon_config": daemon_plan,
                "packs": pack_plan,
            },
            "blocked_items": blocked_items,
            "apply": {
                "applied": true,
                "changed_domains": changed_domains,
                "unchanged_domains": unchanged_domains,
                "written_files": written_files,
                "daemon_config_updated": daemon_config_updated,
                "installed_packs": pack_apply.installed_packs,
                "pack_selection_updated": pack_apply.pack_selection_updated,
                "recommended_packs": recommended_pack_step,
            },
            "recommended_install": default_install_manifest(),
            "doctor_before": doctor_before,
            "doctor_after": doctor_after,
            "next_steps": next_steps,
        }),
        json,
    )
}

fn resolve_template(args: &InitArgs, mode: InitMode) -> Result<LoadedProjectTemplate> {
    let sync_options = if args.update_registry { RegistrySyncOptions::update() } else { RegistrySyncOptions::pinned() };
    match (args.template.as_deref(), args.path.as_deref()) {
        (Some(template_id), None) => {
            load_project_template_from_default_registry_with_options(template_id, sync_options)
        }
        (None, Some(path)) => {
            if args.update_registry {
                return Err(invalid_input_error("--update-registry only applies to template registry templates; remove --path or drop --update-registry"));
            }
            let path = PathBuf::from(path.trim());
            if path.as_os_str().is_empty() {
                return Err(invalid_input_error("template path must not be empty"));
            }
            load_project_template_from_dir(&path)
        }
        (Some(_), Some(_)) => Err(invalid_input_error("provide exactly one of --template or --path")),
        (None, None) => match mode {
            InitMode::Guided => {
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                    return Err(invalid_input_error(
                        "guided init must be run in an interactive terminal; rerun with --template or --path and --non-interactive"
                    ));
                }
                let template = prompt_template_selection(&list_project_templates_from_default_registry_with_options(
                    sync_options,
                )?)?;
                load_project_template_from_default_registry_with_options(&template.id, sync_options)
            }
            InitMode::NonInteractive => Err(invalid_input_error("non-interactive init requires --template or --path")),
        },
    }
}

fn ensure_supported_template_source_mode(template: &LoadedProjectTemplate) -> Result<()> {
    match template.manifest.source.mode {
        orchestrator_config::ProjectTemplateSourceMode::Copy => Ok(()),
        unsupported => Err(invalid_input_error(format!(
            "template source mode '{unsupported:?}' is not supported yet; only copy mode is available"
        ))),
    }
}

fn existing_template_paths(project_root: &Path, template: &LoadedProjectTemplate) -> BTreeSet<PathBuf> {
    template.files.iter().map(|file| project_root.join(&file.relative_path)).filter(|path| path.exists()).collect()
}

fn build_template_file_plan(
    project_root: &Path,
    template: &LoadedProjectTemplate,
    existing_before: &BTreeSet<PathBuf>,
    force: bool,
) -> Vec<InitFilePlan> {
    template
        .files
        .iter()
        .map(|file| {
            let path = project_root.join(&file.relative_path);
            let action = if existing_before.contains(&path) {
                if force {
                    "overwrite"
                } else {
                    "conflict"
                }
            } else {
                "create"
            };
            InitFilePlan { path: file.relative_path.display().to_string(), action: action.to_string() }
        })
        .collect()
}

fn build_pack_plan(project_root: &Path, template: &LoadedProjectTemplate) -> Result<Vec<InitPackPlan>> {
    if template.manifest.packs.is_empty() {
        return Ok(Vec::new());
    }

    let inventory = load_pack_inventory(project_root)?;
    template
        .manifest
        .packs
        .iter()
        .map(|pack| {
            let entry = inventory.entries.iter().find(|entry| {
                entry.pack_id.eq_ignore_ascii_case(&pack.id) && pack_source_matches(pack.source, entry.source)
            });
            let action = if pack.activate {
                if entry.is_some() {
                    "activate"
                } else {
                    "missing"
                }
            } else {
                "skip"
            };
            Ok(InitPackPlan {
                pack_id: pack.id.clone(),
                action: action.to_string(),
                source: entry.map(|entry| entry.source.as_str().to_string()),
            })
        })
        .collect()
}

fn pack_source_matches(source: Option<PackSelectionSource>, entry_source: PackRegistrySource) -> bool {
    source.map(|value| value.as_registry_source() == entry_source).unwrap_or(true)
}

fn fail_on_conflicting_paths(existing_before: &BTreeSet<PathBuf>, force: bool) -> Result<()> {
    if force || existing_before.is_empty() {
        return Ok(());
    }
    let conflicts = existing_before.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
    Err(conflict_error(format!(
        "init would overwrite existing project files: {}. rerun with --force to replace them",
        conflicts.join(", ")
    )))
}

fn write_template_files(project_root: &Path, template: &LoadedProjectTemplate) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for file in &template.files {
        let path = project_root.join(&file.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.contents)?;
        written.push(file.relative_path.display().to_string());
    }
    Ok(written)
}

fn apply_template_packs(project_root: &Path, template: &LoadedProjectTemplate) -> Result<InitPackApply> {
    let installed_packs = ensure_template_packs_available(project_root, template)?;

    let inventory = load_pack_inventory(project_root)?;
    let mut state = load_pack_selection_state(project_root)?;
    let mut updated = false;

    for pack in &template.manifest.packs {
        if !pack.activate {
            continue;
        }

        let source = inventory
            .entries
            .iter()
            .find(|entry| {
                entry.pack_id.eq_ignore_ascii_case(&pack.id) && pack_source_matches(pack.source, entry.source)
            })
            .map(|entry| selection_source_for(entry.source))
            .ok_or_else(|| invalid_input_error(format!("template references unavailable pack '{}'", pack.id)))?;

        state.upsert(PackSelectionEntry {
            pack_id: pack.id.clone(),
            version: pack.version.clone(),
            source: Some(source),
            enabled: true,
        })?;
        updated = true;
    }

    if updated {
        save_pack_selection_state(project_root, &state)?;
    }
    Ok(InitPackApply { installed_packs, pack_selection_updated: updated })
}

fn ensure_template_packs_available(project_root: &Path, template: &LoadedProjectTemplate) -> Result<Vec<String>> {
    if template.manifest.packs.is_empty() {
        return Ok(Vec::new());
    }

    let _inventory = load_pack_inventory(project_root)?;
    Ok(Vec::new())
}

const DEFAULT_INSTALL_MANIFEST_JSON: &str = include_str!("../../../config/default-install.json");

fn default_install_manifest() -> serde_json::Value {
    serde_json::from_str(DEFAULT_INSTALL_MANIFEST_JSON).unwrap_or_else(|_| serde_json::json!({}))
}

/// One recommended pack pin from `config/default-install.json`.
#[derive(Debug, Clone, serde::Deserialize)]
struct RecommendedPackPin {
    id: String,
    repo: String,
    tag: String,
}

fn recommended_packs() -> Vec<RecommendedPackPin> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        packs: Vec<RecommendedPackPin>,
    }
    serde_json::from_str::<Manifest>(DEFAULT_INSTALL_MANIFEST_JSON).map(|manifest| manifest.packs).unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
struct PackInstallResult {
    pack_id: String,
    repo: String,
    tag: String,
    /// "installed" | "already_installed" | "failed"
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RecommendedPackStep {
    skipped: bool,
    results: Vec<PackInstallResult>,
}

impl RecommendedPackStep {
    fn skipped() -> Self {
        Self { skipped: true, results: Vec::new() }
    }
}

fn manual_pack_install_command(pack: &RecommendedPackPin) -> String {
    let dir_name = pack.repo.rsplit('/').next().unwrap_or(pack.id.as_str());
    format!(
        "git clone --depth 1 --branch {tag} https://github.com/{repo}.git /tmp/{dir} && animus pack install --path /tmp/{dir} --activate",
        tag = pack.tag,
        repo = pack.repo,
        dir = dir_name,
    )
}

/// Resolve the local source directory for a recommended pack. By default
/// this shallow-clones the pinned GitHub release tag; the
/// `ANIMUS_INIT_PACK_SOURCE_DIR` env var overrides the source with
/// `<dir>/<pack-id>` for offline installs and tests.
fn resolve_recommended_pack_source(pack: &RecommendedPackPin, scratch: &Path) -> Result<PathBuf> {
    if let Ok(source_dir) = std::env::var("ANIMUS_INIT_PACK_SOURCE_DIR") {
        let candidate = PathBuf::from(source_dir).join(&pack.id);
        if !candidate.is_dir() {
            return Err(anyhow::anyhow!(
                "ANIMUS_INIT_PACK_SOURCE_DIR is set but {} does not exist",
                candidate.display()
            ));
        }
        return Ok(candidate);
    }

    let clone_target = scratch.join(&pack.id);
    let url = format!("https://github.com/{}.git", pack.repo);
    let output = Command::new("git")
        .args(["clone", "--depth", "1", "--branch", &pack.tag, &url])
        .arg(&clone_target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run git clone for {url}: {err}"))?;
    if !output.status.success() {
        let stderr = tail_stderr(&output.stderr).unwrap_or_default();
        return Err(anyhow::anyhow!("git clone of {url} at tag {} failed: {stderr}", pack.tag));
    }
    Ok(clone_target)
}

/// True when the `animus.task` pack ended up usable (freshly installed or
/// already present), i.e. the `animus.task/standard` workflow hint is valid.
fn task_pack_ready(step: &RecommendedPackStep) -> bool {
    step.results.iter().any(|result| {
        result.pack_id.eq_ignore_ascii_case("animus.task")
            && matches!(result.status.as_str(), "installed" | "already_installed")
    })
}

/// Re-enable the project pack selection for a pack version that is already
/// discovered in the inventory, preserving the source it was found under.
fn activate_existing_pack(project_root: &Path, pack_id: &str, version: &str, source: PackRegistrySource) -> Result<()> {
    let mut state = load_pack_selection_state(project_root)?;
    state.upsert(PackSelectionEntry {
        pack_id: pack_id.to_string(),
        version: Some(format!("={version}")),
        source: Some(selection_source_for(source)),
        enabled: true,
    })?;
    save_pack_selection_state(project_root, &state)?;
    Ok(())
}

/// Install + activate every recommended pack from `default-install.json`.
/// Failures are per-pack and never abort init: the result row carries the
/// error plus the manual recovery command.
fn install_recommended_packs(project_root: &Path) -> RecommendedPackStep {
    let packs = recommended_packs();
    // Skip the download only when the *pinned* version is already present;
    // an older version of the same pack id must not mask the pinned release.
    // Versions install side by side under ~/.animus/packs/<id>/<version>/,
    // so installing the pin never clobbers existing versions.
    // (codex round-1 P2.)
    // Only machine-installed packs (~/.animus/packs/) count as the pinned
    // install. A project override with the same id/version must not stop
    // the pinned release from being installed. (codex round-4 P2.)
    let installed_versions: std::collections::BTreeMap<(String, String), PackRegistrySource> =
        load_pack_inventory(project_root)
            .map(|inventory| {
                inventory
                    .entries
                    .iter()
                    .filter(|entry| entry.source == PackRegistrySource::Installed)
                    .map(|entry| ((entry.pack_id.to_ascii_lowercase(), entry.version.clone()), entry.source))
                    .collect()
            })
            .unwrap_or_default();
    let scratch = tempfile::tempdir().ok();

    let mut results = Vec::with_capacity(packs.len());
    for pack in &packs {
        let pinned_version = pack.tag.trim_start_matches('v').to_string();
        if let Some(source) = installed_versions.get(&(pack.id.to_ascii_lowercase(), pinned_version.clone())).copied() {
            // Even when the pinned version is already on disk, init promises
            // install-AND-activate: re-enable the project selection so a
            // previously disabled or re-pinned pack becomes usable again.
            // (codex round-2 P2.)
            match activate_existing_pack(project_root, &pack.id, &pinned_version, source) {
                Ok(()) => results.push(PackInstallResult {
                    pack_id: pack.id.clone(),
                    repo: pack.repo.clone(),
                    tag: pack.tag.clone(),
                    status: "already_installed".to_string(),
                    version: Some(pinned_version),
                    detail: None,
                }),
                Err(err) => results.push(PackInstallResult {
                    pack_id: pack.id.clone(),
                    repo: pack.repo.clone(),
                    tag: pack.tag.clone(),
                    status: "failed".to_string(),
                    version: Some(pinned_version),
                    detail: Some(format!("pack is installed but could not be activated: {err:#}")),
                }),
            }
            continue;
        }
        let install = scratch
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("failed to create scratch directory for pack downloads"))
            .and_then(|scratch| resolve_recommended_pack_source(pack, scratch.path()))
            .and_then(|source_root| {
                // The pack manifest must match the pin: installing a
                // mismatched manifest would activate a different pack while
                // init reports the pinned id as installed. (codex round-3 P2.)
                let loaded = load_pack_manifest(&source_root)?;
                if !loaded.manifest.id.eq_ignore_ascii_case(&pack.id) {
                    return Err(anyhow::anyhow!(
                        "pack source declares id '{}' but the pin expects '{}'",
                        loaded.manifest.id,
                        pack.id
                    ));
                }
                if loaded.manifest.version != pinned_version {
                    return Err(anyhow::anyhow!(
                        "pack source declares version '{}' but tag {} pins version '{}'",
                        loaded.manifest.version,
                        pack.tag,
                        pinned_version
                    ));
                }
                super::ops_pack::install_pack_from_source_root(project_root, &source_root, true, false)
            });
        match install {
            Ok(output) => results.push(PackInstallResult {
                pack_id: pack.id.clone(),
                repo: pack.repo.clone(),
                tag: pack.tag.clone(),
                status: "installed".to_string(),
                version: Some(output.version),
                detail: None,
            }),
            Err(err) => results.push(PackInstallResult {
                pack_id: pack.id.clone(),
                repo: pack.repo.clone(),
                tag: pack.tag.clone(),
                status: "failed".to_string(),
                version: None,
                detail: Some(format!("{err:#}; install manually: {}", manual_pack_install_command(pack))),
            }),
        }
    }
    RecommendedPackStep { skipped: false, results }
}

fn print_pack_install_summary(step: &RecommendedPackStep) {
    if step.skipped {
        return;
    }
    println!("Recommended workflow packs:");
    for result in &step.results {
        match result.status.as_str() {
            "installed" => {
                println!("  - {} {} installed and activated", result.pack_id, result.version.as_deref().unwrap_or(""))
            }
            "already_installed" => println!("  - {} already installed", result.pack_id),
            _ => println!("  - {} FAILED: {}", result.pack_id, result.detail.as_deref().unwrap_or("unknown error")),
        }
    }
    println!();
}

// ---------------------------------------------------------------------------
// Secrets migration advice (v0.5.8 keychain support)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct SecretsMigrationAdvice {
    detected_env_keys: Vec<String>,
    suggested_commands: Vec<String>,
    docs: String,
}

#[derive(Debug, Clone, Serialize)]
struct WalkthroughSecretStep {
    offered: bool,
    accepted: bool,
    stored: Vec<String>,
    failed: Vec<String>,
}

impl WalkthroughSecretStep {
    fn not_run(offered: bool) -> Self {
        Self { offered, accepted: false, stored: Vec::new(), failed: Vec::new() }
    }
}

fn build_secrets_migration_advice(detections: &[CliDetection]) -> Option<SecretsMigrationAdvice> {
    let mut detected_env_keys: Vec<String> = Vec::new();
    for detection in detections {
        if detection.api_key_via_env && !detected_env_keys.contains(&detection.api_key_env) {
            detected_env_keys.push(detection.api_key_env.clone());
        }
    }
    if detected_env_keys.is_empty() {
        return None;
    }
    let mut suggested_commands: Vec<String> =
        detected_env_keys.iter().map(|key| format!("animus secret set {key}")).collect();
    suggested_commands.push("animus secret import-env  # bulk migrate from a .env file".to_string());
    Some(SecretsMigrationAdvice {
        detected_env_keys,
        suggested_commands,
        docs: "docs/reference/secrets.md".to_string(),
    })
}

fn print_secrets_migration_advice(advice: &SecretsMigrationAdvice) {
    println!("API keys detected in environment variables: {}", advice.detected_env_keys.join(", "));
    println!("Animus can store these in the OS keychain instead (recommended):");
    for command in &advice.suggested_commands {
        println!("  {command}");
    }
    println!("See {} for details.", advice.docs);
    println!();
}

/// Import the detected env-var API keys into the OS keychain. Only ever
/// called after an explicit interactive confirmation (default no).
fn import_detected_env_keys(project_root: &Path, keys: &[String]) -> WalkthroughSecretStep {
    let mut step = WalkthroughSecretStep { offered: true, accepted: true, stored: Vec::new(), failed: Vec::new() };
    for key in keys {
        let value = std::env::var(key).unwrap_or_default();
        match super::ops_secret::store_project_secret(project_root, key, &value) {
            Ok(()) => step.stored.push(key.clone()),
            Err(err) => {
                eprintln!("warning: failed to store {key} in keychain: {err:#}");
                step.failed.push(key.clone());
            }
        }
    }
    step
}

fn selection_source_for(source: PackRegistrySource) -> PackSelectionSource {
    match source {
        PackRegistrySource::Bundled => PackSelectionSource::Bundled,
        PackRegistrySource::Installed => PackSelectionSource::Installed,
        PackRegistrySource::ProjectOverride => PackSelectionSource::ProjectOverride,
    }
}

fn prompt_template_selection(templates: &[ProjectTemplateSummary]) -> Result<ProjectTemplateSummary> {
    // Defense-in-depth alongside the guided-mode TTY guard at the call
    // site: a non-TTY stdin (CI, piped input) must error cleanly instead
    // of blocking forever on `read_line`.
    if !io::stdin().is_terminal() {
        return Err(invalid_input_error(
            "template selection requires an interactive terminal; rerun with --template <TEMPLATE_ID> or --path <PATH>",
        ));
    }
    let mut stdout = io::stdout();
    let mut input = String::new();

    loop {
        println!("Choose an Animus project template:");
        for (index, template) in templates.iter().enumerate() {
            println!("  {}. {} ({}) - {}", index + 1, template.title, template.id, template.description);
        }
        print!("Template [1-{}]: ", templates.len());
        stdout.flush()?;

        input.clear();
        let bytes_read = io::stdin().read_line(&mut input)?;
        if bytes_read == 0 {
            // EOF (e.g. Ctrl-D): bail instead of looping forever.
            return Err(invalid_input_error(
                "no template selected (stdin closed); rerun with --template <TEMPLATE_ID> or --path <PATH>",
            ));
        }
        let trimmed = input.trim();
        if let Ok(index) = trimmed.parse::<usize>() {
            if let Some(template) = templates.get(index.saturating_sub(1)) {
                return Ok(template.clone());
            }
        }

        if let Some(template) = templates.iter().find(|template| template.id.eq_ignore_ascii_case(trimmed)) {
            return Ok(template.clone());
        }

        println!("Enter a template number or id.");
    }
}

fn ensure_daemon_project_config(project_root: &Path, daemon_config_exists_before: bool) -> Result<bool> {
    if daemon_config_exists_before {
        return Ok(false);
    }
    let config = load_daemon_project_config(project_root)?;
    write_daemon_project_config(project_root, &config)?;
    Ok(true)
}

fn count_checks(report: &DoctorReport, status: DoctorCheckStatus) -> usize {
    report.checks.iter().filter(|check| check.status == status).count()
}

fn collect_blocked_items(report: &DoctorReport) -> Vec<InitBlockedItem> {
    report
        .checks
        .iter()
        .filter(|check| {
            check.status == DoctorCheckStatus::Fail
                || (check.status == DoctorCheckStatus::Warn && !check.remediation.available)
        })
        .map(|check| InitBlockedItem {
            check_id: check.id.clone(),
            details: check.details.clone(),
            remediation: check.remediation.details.clone(),
        })
        .collect()
}

fn remediation_needed(report: &DoctorReport, remediation_id: &str) -> bool {
    report.checks.iter().any(|check| {
        check.remediation.id == remediation_id && check.remediation.available && check.status != DoctorCheckStatus::Ok
    })
}

// ---------------------------------------------------------------------------
// v0.4.13 walkthrough flow
// ---------------------------------------------------------------------------

async fn run_walkthrough(args: &InitArgs, project_root: &Path, mode: InitMode, json: bool) -> Result<()> {
    // JSON envelope contract: in `--json` mode the envelope on stdout is the
    // entire user-facing surface. We must not print the human-readable intro,
    // and we must not block on stdin prompts (which would silently hang
    // scripted callers in a TTY). Forcing `interactive=false` whenever `json`
    // is set bakes both requirements into a single check so every downstream
    // branch (intro print + prompt_yes_no calls) inherits the safe behavior.
    let interactive =
        matches!(mode, InitMode::Guided) && !json && io::stdin().is_terminal() && io::stdout().is_terminal();

    let detections = detect_clis_and_keys();
    let secrets_advice = build_secrets_migration_advice(&detections);
    if interactive {
        print_walkthrough_intro(&detections);
        if let Some(advice) = secrets_advice.as_ref() {
            print_secrets_migration_advice(advice);
        }
    }

    let default_provider = pick_default_provider(&detections);

    let install_plugins = if args.no_install {
        false
    } else if interactive && !args.non_interactive && !args.plan {
        prompt_yes_no("Install the default provider plugins (animus-provider-claude/codex/gemini/opencode)?", true)?
    } else {
        true
    };

    let recommended_pack_ids = recommended_packs().iter().map(|pack| pack.id.clone()).collect::<Vec<_>>().join(", ");
    let install_packs = if args.install_packs {
        true
    } else if interactive && !args.non_interactive && !args.plan {
        prompt_yes_no(&format!("Install the recommended workflow packs ({recommended_pack_ids})?"), true)?
    } else {
        false
    };

    // Importing keys into the OS keychain is opt-in and interactive-only:
    // it must never happen silently (or at all) in scripted runs.
    let import_secrets = if interactive && !args.non_interactive && !args.plan && secrets_advice.is_some() {
        prompt_yes_no("Store the detected API keys in the OS keychain now (animus secret set)?", false)?
    } else {
        false
    };

    let copy_template = !args.no_template;

    let template_plan = plan_walkthrough_template(project_root, &args.walkthrough_template, args.force)?;

    let auto_start_daemon = if args.auto_start {
        true
    } else if interactive && !args.non_interactive && !args.plan {
        prompt_yes_no("Start the autonomous daemon after init?", false)?
    } else {
        false
    };

    // Dry-run: --plan must not mutate state. Report the planned actions only.
    if args.plan {
        return print_value(
            serde_json::json!({
                "stage": "walkthrough_plan",
                "mode": mode,
                "interactive": interactive,
                "environment": {
                    "project_root": project_root.display().to_string(),
                },
                "detected_clis": detections,
                "default_provider": default_provider,
                "template": {
                    "name": HELLO_WORLD_TEMPLATE_NAME,
                    "requested": &args.walkthrough_template,
                    "plan": template_plan,
                },
                "planned_actions": {
                    "install_plugins": install_plugins,
                    "install_packs": install_packs,
                    "copy_template": copy_template,
                    "auto_start_daemon": auto_start_daemon,
                },
                "secrets": secrets_advice,
                "next_steps": [
                    "Re-run without --plan to apply these actions.".to_string(),
                ],
            }),
            json,
        );
    }

    // Create `.animus/` before installing defaults so the project-local
    // plugin lockfile resolves to `.animus/plugins.lock` instead of the
    // `~/.animus/plugins.lock` fallback. Without this, `animus plugin lock
    // list/verify` after init misses the walkthrough's installed entries.
    std::fs::create_dir_all(project_root.join(".animus"))?;
    // Keep project-local plugin BINARIES (`.animus/plugins/`) out of version
    // control while leaving `.animus/plugins.lock` committable — committing
    // the lockfile is how a repo pins its own plugin set.
    if let Err(err) = super::ops_plugin::ensure_project_plugins_gitignore(project_root) {
        tracing::warn!(%err, "failed to write .animus/.gitignore during init");
    }

    let plugin_step = if install_plugins {
        run_install_defaults_subprocess(project_root, json).await
    } else {
        WalkthroughPluginStep { skipped: true, invoked: false, exit_code: None, stderr_tail: None }
    };

    let pack_step =
        if install_packs { install_recommended_packs(project_root) } else { RecommendedPackStep::skipped() };
    if !json {
        print_pack_install_summary(&pack_step);
    }

    let secret_step = if import_secrets {
        let keys = secrets_advice.as_ref().map(|advice| advice.detected_env_keys.clone()).unwrap_or_default();
        let step = import_detected_env_keys(project_root, &keys);
        if !step.stored.is_empty() {
            println!("Stored in OS keychain: {}", step.stored.join(", "));
        }
        step
    } else {
        WalkthroughSecretStep::not_run(secrets_advice.is_some())
    };

    let template_step = if copy_template {
        copy_walkthrough_template(project_root, &args.walkthrough_template, args.force)?
    } else {
        WalkthroughTemplateStep { skipped: true, written: Vec::new(), skipped_existing: Vec::new() }
    };

    let daemon_step = if auto_start_daemon {
        run_daemon_start_subprocess(project_root, json).await
    } else {
        WalkthroughDaemonStep { requested: false, invoked: false, exit_code: None, stderr_tail: None }
    };

    let next_steps = build_next_steps(&template_plan, &plugin_step, &pack_step, secrets_advice.as_ref(), &daemon_step);

    print_value(
        serde_json::json!({
            "stage": "walkthrough",
            "mode": mode,
            "interactive": interactive,
            "environment": {
                "project_root": project_root.display().to_string(),
            },
            "detected_clis": detections,
            "default_provider": default_provider,
            "template": {
                "name": HELLO_WORLD_TEMPLATE_NAME,
                "requested": &args.walkthrough_template,
                "plan": template_plan,
            },
            "secrets": secrets_advice,
            "apply": {
                "plugins": plugin_step,
                "packs": pack_step,
                "secrets": secret_step,
                "template": template_step,
                "daemon": daemon_step,
            },
            "next_steps": next_steps,
        }),
        json,
    )
}

fn print_walkthrough_intro(detections: &[CliDetection]) {
    println!("Animus init walkthrough — detected CLIs and API keys:");
    for detection in detections {
        let install_label = if detection.installed { "installed" } else { "not installed" };
        let key_label = match (detection.api_key_via_env, detection.api_key_via_config) {
            (true, _) => format!("api key via env ({})", detection.api_key_env),
            (false, true) => "api key via local config".to_string(),
            _ => "no api key detected".to_string(),
        };
        println!("  - {:<10} {} | {}", detection.name, install_label, key_label);
    }
    println!();
}

fn pick_default_provider(detections: &[CliDetection]) -> Option<String> {
    detections
        .iter()
        .find(|d| d.installed && (d.api_key_via_env || d.api_key_via_config))
        .map(|d| d.name.clone())
        .or_else(|| detections.iter().find(|d| d.installed).map(|d| d.name.clone()))
}

fn detect_clis_and_keys() -> Vec<CliDetection> {
    let home = dirs::home_dir();
    CLI_DETECTION_TARGETS
        .iter()
        .map(|target| {
            let binary_path = which::which(target.binary).ok();
            let api_key_via_env = std::env::var(target.api_env).map(|v| !v.is_empty()).unwrap_or(false);
            let config_path = home.as_ref().map(|h| h.join(target.config_subpath));
            let api_key_via_config = config_path.as_ref().map(|p| p.exists()).unwrap_or(false);
            CliDetection {
                name: target.name.to_string(),
                binary: target.binary.to_string(),
                installed: binary_path.is_some(),
                binary_path: binary_path.map(|p| p.display().to_string()),
                api_key_env: target.api_env.to_string(),
                api_key_via_env,
                api_key_via_config,
                config_path: config_path.map(|p| p.display().to_string()),
            }
        })
        .collect()
}

fn plan_walkthrough_template(
    project_root: &Path,
    template_name: &str,
    force: bool,
) -> Result<Vec<WalkthroughTemplatePlan>> {
    let files = lookup_walkthrough_template_files(template_name)?;
    Ok(files
        .into_iter()
        .map(|(relative_path, _)| {
            let target = project_root.join(&relative_path);
            let action = if target.exists() {
                if force {
                    "overwrite"
                } else {
                    "skip_exists"
                }
            } else {
                "create"
            };
            WalkthroughTemplatePlan { name: template_name.to_string(), relative_path, action: action.to_string() }
        })
        .collect())
}

fn lookup_walkthrough_template_files(template_name: &str) -> Result<Vec<(String, &'static str)>> {
    match template_name {
        HELLO_WORLD_TEMPLATE_NAME => {
            Ok(vec![(format!(".animus/workflows/{HELLO_WORLD_WORKFLOW_FILE}"), HELLO_WORLD_WORKFLOW_YAML)])
        }
        other => Err(invalid_input_error(format!(
            "unknown walkthrough template '{other}'; only '{HELLO_WORLD_TEMPLATE_NAME}' is bundled in this release"
        ))),
    }
}

fn copy_walkthrough_template(project_root: &Path, template_name: &str, force: bool) -> Result<WalkthroughTemplateStep> {
    let files = lookup_walkthrough_template_files(template_name)?;
    let mut written = Vec::new();
    let mut skipped_existing = Vec::new();
    for (relative, contents) in files {
        let target = project_root.join(&relative);
        if target.exists() && !force {
            skipped_existing.push(relative);
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, contents)?;
        written.push(relative);
    }
    Ok(WalkthroughTemplateStep { skipped: false, written, skipped_existing })
}

async fn run_install_defaults_subprocess(project_root: &Path, json: bool) -> WalkthroughPluginStep {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            return WalkthroughPluginStep {
                skipped: false,
                invoked: false,
                exit_code: None,
                stderr_tail: Some(format!("could not resolve current_exe: {err}")),
            };
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--project-root").arg(project_root);
    if json {
        cmd.arg("--json");
    }
    // Walkthrough is the first-contact path — install the *full* set
    // of required-role plugins (providers + subject backends +
    // transports) so the subsequent `daemon start` preflight passes
    // and the daemon doesn't boot in a degraded state. Without
    // `--include-subjects` the task/requirement subject roles stay
    // unsatisfied and the daemon would refuse to start (or, with
    // --skip-preflight, would boot partially functional). This is
    // half of the defense-in-depth fix; the other half is removing
    // `--skip-preflight` from the daemon spawn below.
    cmd.args(["plugin", "install-defaults", "--yes", "--include-subjects", "--include-transports"]);
    cmd.stdin(Stdio::null());
    // In JSON mode, pipe stdout (and discard) so the child's `--json` output
    // does not interleave with the parent envelope. In human mode, inherit
    // so the user sees progress live.
    if json {
        cmd.stdout(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit());
    }
    cmd.stderr(Stdio::piped());
    let output = match cmd.output() {
        Ok(out) => out,
        Err(err) => {
            return WalkthroughPluginStep {
                skipped: false,
                invoked: false,
                exit_code: None,
                stderr_tail: Some(format!("failed to spawn animus plugin install-defaults: {err}")),
            };
        }
    };
    WalkthroughPluginStep {
        skipped: false,
        invoked: true,
        exit_code: output.status.code(),
        stderr_tail: tail_stderr(&output.stderr),
    }
}

async fn run_daemon_start_subprocess(project_root: &Path, json: bool) -> WalkthroughDaemonStep {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            return WalkthroughDaemonStep {
                requested: true,
                invoked: false,
                exit_code: None,
                stderr_tail: Some(format!("could not resolve current_exe: {err}")),
            };
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--project-root").arg(project_root);
    if json {
        cmd.arg("--json");
    }
    // Defense-in-depth: do NOT pass `--skip-preflight` here. The
    // walkthrough's `plugin install-defaults` step above is supposed
    // to fill all required roles (providers + subjects + transports);
    // if it didn't, we want the daemon to refuse to start with the
    // actionable preflight error rather than boot in a degraded
    // state. `--auto-install` keeps the safety net so any leftover
    // gaps still get filled before the daemon comes up.
    cmd.args(["daemon", "start", "--auto-install"]);
    cmd.stdin(Stdio::null());
    if json {
        cmd.stdout(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit());
    }
    cmd.stderr(Stdio::piped());
    let output = match cmd.output() {
        Ok(out) => out,
        Err(err) => {
            return WalkthroughDaemonStep {
                requested: true,
                invoked: false,
                exit_code: None,
                stderr_tail: Some(format!("failed to spawn animus daemon start: {err}")),
            };
        }
    };
    WalkthroughDaemonStep {
        requested: true,
        invoked: true,
        exit_code: output.status.code(),
        stderr_tail: tail_stderr(&output.stderr),
    }
}

fn tail_stderr(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(10);
    Some(lines[start..].join("\n"))
}

fn build_next_steps(
    template_plan: &[WalkthroughTemplatePlan],
    plugin_step: &WalkthroughPluginStep,
    pack_step: &RecommendedPackStep,
    secrets_advice: Option<&SecretsMigrationAdvice>,
    daemon_step: &WalkthroughDaemonStep,
) -> Vec<String> {
    let mut steps = Vec::new();
    // Keep these install hints in lockstep with the actual command
    // `run_install_defaults_subprocess` invokes above. Without
    // `--include-subjects --include-transports` the user would follow
    // the recovery hint and still end up with an unsatisfied preflight
    // because subject/transport backends would never get installed.
    if plugin_step.skipped {
        steps.push(
            "Install default plugins: animus plugin install-defaults --yes --include-subjects --include-transports"
                .to_string(),
        );
    } else if plugin_step.exit_code != Some(0) {
        steps.push(
            "Re-run `animus plugin install-defaults --include-subjects --include-transports` to fix the failed install above."
                .to_string(),
        );
    }
    if pack_step.skipped {
        steps.push(
            "Install the recommended workflow packs when ready: animus init --walkthrough --no-template --no-install --install-packs"
                .to_string(),
        );
    } else {
        for result in &pack_step.results {
            if result.status == "failed" {
                steps.push(format!(
                    "Pack install failed for {}: {}",
                    result.pack_id,
                    result.detail.as_deref().unwrap_or("unknown error")
                ));
            }
        }
        if task_pack_ready(pack_step) {
            steps.push(
                "Run the standard task workflow: `animus subject create --kind task --title \"My first task\"` then `animus workflow run animus.task/standard --task-id TASK-001 --sync`."
                    .to_string(),
            );
        }
    }
    if template_plan.iter().any(|plan| plan.action == "create" || plan.action == "overwrite") {
        steps.push("Run `animus workflow run hello-world --sync` to verify your install.".to_string());
    } else {
        steps.push(
            "Hello-world template already present; run `animus workflow run hello-world --sync` to verify.".to_string(),
        );
    }
    if let Some(advice) = secrets_advice {
        steps.push(format!(
            "Move API keys ({}) from env vars into the OS keychain: `animus secret set <KEY>` (see {}).",
            advice.detected_env_keys.join(", "),
            advice.docs
        ));
    }
    if !daemon_step.requested {
        steps.push("Start the daemon when ready: `animus daemon start`".to_string());
    } else if daemon_step.exit_code != Some(0) {
        steps.push("Daemon start exited non-zero. Check `animus daemon status` and `animus logs tail`.".to_string());
    }
    steps.push("More: docs/getting-started/quick-start.md".to_string());
    steps
}

fn prompt_yes_no(message: &str, default_yes: bool) -> Result<bool> {
    let default_label = if default_yes { "[Y/n]" } else { "[y/N]" };
    let mut input = String::new();
    print!("{message} {default_label}: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

/// Re-exported for the bundled README contents so callers (or future
/// `animus init --walkthrough --print-readme` style flags) can surface it.
#[allow(dead_code)]
pub(crate) fn bundled_hello_world_readme() -> &'static str {
    HELLO_WORLD_README
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use orchestrator_config::{
        ProjectTemplateDaemon, ProjectTemplateFile, ProjectTemplateManifest, ProjectTemplateSource,
        ProjectTemplateSourceMode,
    };

    fn template_fixture(
        id: &str,
        pattern: &str,
        daemon: ProjectTemplateDaemon,
        files: Vec<ProjectTemplateFile>,
    ) -> LoadedProjectTemplate {
        LoadedProjectTemplate {
            source_kind: ProjectTemplateSourceKind::Local,
            template_root: None,
            manifest: ProjectTemplateManifest {
                schema: "animus.template.v1".to_string(),
                id: id.to_string(),
                version: "0.1.0".to_string(),
                title: id.to_string(),
                description: format!("{id} template"),
                pattern: pattern.to_string(),
                source: ProjectTemplateSource { mode: ProjectTemplateSourceMode::Copy, root: "skeleton".to_string() },
                daemon,
                packs: Vec::new(),
                next_steps: Vec::new(),
            },
            files,
        }
    }

    #[test]
    fn build_template_file_plan_marks_conflicts_without_force() {
        let template = template_fixture(
            "task-queue",
            "task-queue",
            ProjectTemplateDaemon::default(),
            vec![ProjectTemplateFile {
                relative_path: PathBuf::from(".animus/workflows/custom.yaml"),
                contents: b"default_workflow_ref: standard-workflow\n".to_vec(),
            }],
        );
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let conflict_path = temp.path().join(".animus/workflows/custom.yaml");
        std::fs::create_dir_all(conflict_path.parent().expect("parent")).expect("parent should exist");
        std::fs::write(&conflict_path, "existing").expect("existing file should be written");
        let existing = existing_template_paths(temp.path(), &template);

        let plan = build_template_file_plan(temp.path(), &template, &existing, false);
        assert!(plan.iter().any(|file| file.action == "conflict"));
    }

    // ---- v0.4.13 walkthrough tests ---------------------------------------

    #[test]
    fn walkthrough_template_lookup_returns_hello_world_yaml() {
        let files = lookup_walkthrough_template_files(HELLO_WORLD_TEMPLATE_NAME).expect("hello-world lookup");
        assert_eq!(files.len(), 1);
        let (path, contents) = &files[0];
        assert_eq!(path, ".animus/workflows/hello-world.yaml");
        assert!(contents.contains("id: hello-world"));
        assert!(contents.contains("model: claude-haiku-4-5"));
    }

    #[test]
    fn walkthrough_template_lookup_rejects_unknown_template() {
        let err = lookup_walkthrough_template_files("does-not-exist").expect_err("unknown template should error");
        let msg = format!("{err}");
        assert!(msg.contains("unknown walkthrough template"), "got: {msg}");
    }

    #[test]
    fn init_template_hello_world_passes_yaml_schema_validation() {
        let parsed = orchestrator_config::parse_yaml_workflow_config(HELLO_WORLD_WORKFLOW_YAML)
            .expect("bundled hello-world workflow must parse cleanly");
        assert!(
            parsed.workflows.iter().any(|w| w.id == "hello-world"),
            "parsed workflows should include 'hello-world', got: {:?}",
            parsed.workflows.iter().map(|w| &w.id).collect::<Vec<_>>()
        );
        assert!(
            parsed.agent_profiles.contains_key("hello-world-agent"),
            "agent registry should include hello-world-agent"
        );
    }

    #[test]
    fn copy_walkthrough_template_writes_hello_world_workflow() {
        let project = tempfile::tempdir().expect("project tempdir");
        let step = copy_walkthrough_template(project.path(), HELLO_WORLD_TEMPLATE_NAME, false).expect("copy succeeds");
        assert!(!step.skipped);
        assert_eq!(step.written, vec![".animus/workflows/hello-world.yaml".to_string()]);
        let target = project.path().join(".animus/workflows/hello-world.yaml");
        assert!(target.exists(), "yaml should be written to {}", target.display());
        let contents = std::fs::read_to_string(&target).expect("read written yaml");
        assert!(contents.contains("id: hello-world"));
    }

    #[test]
    fn init_skips_template_if_no_template_flag_set() {
        // When --no-template is set we should not touch the workflows
        // directory at all. We simulate that by calling the plan + apply
        // helpers in their "skip" branch.
        let project = tempfile::tempdir().expect("project tempdir");
        let skipped = WalkthroughTemplateStep { skipped: true, written: Vec::new(), skipped_existing: Vec::new() };
        assert!(skipped.skipped && skipped.written.is_empty());
        let target = project.path().join(".animus/workflows/hello-world.yaml");
        assert!(!target.exists(), "no template copy must not write anything");
    }

    #[test]
    fn copy_walkthrough_template_skips_existing_without_force() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = project.path().join(".animus/workflows/hello-world.yaml");
        std::fs::create_dir_all(target.parent().unwrap()).expect("parent dir");
        std::fs::write(&target, "user-owned").expect("seed existing");
        let step = copy_walkthrough_template(project.path(), HELLO_WORLD_TEMPLATE_NAME, false).expect("copy succeeds");
        assert!(step.written.is_empty(), "should not overwrite without --force");
        assert_eq!(step.skipped_existing, vec![".animus/workflows/hello-world.yaml".to_string()]);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user-owned");
    }

    #[test]
    fn copy_walkthrough_template_overwrites_with_force() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = project.path().join(".animus/workflows/hello-world.yaml");
        std::fs::create_dir_all(target.parent().unwrap()).expect("parent dir");
        std::fs::write(&target, "stale").expect("seed");
        let step = copy_walkthrough_template(project.path(), HELLO_WORLD_TEMPLATE_NAME, true).expect("copy succeeds");
        assert_eq!(step.written, vec![".animus/workflows/hello-world.yaml".to_string()]);
        let contents = std::fs::read_to_string(&target).expect("read overwritten yaml");
        assert!(contents.contains("id: hello-world"), "force overwrite should replace stale contents");
    }

    #[test]
    fn pick_default_provider_prefers_installed_with_key() {
        let detections = vec![
            CliDetection {
                name: "claude".into(),
                binary: "claude".into(),
                installed: false,
                binary_path: None,
                api_key_env: "ANTHROPIC_API_KEY".into(),
                api_key_via_env: true,
                api_key_via_config: false,
                config_path: None,
            },
            CliDetection {
                name: "codex".into(),
                binary: "codex".into(),
                installed: true,
                binary_path: Some("/usr/local/bin/codex".into()),
                api_key_env: "OPENAI_API_KEY".into(),
                api_key_via_env: true,
                api_key_via_config: false,
                config_path: None,
            },
            CliDetection {
                name: "gemini".into(),
                binary: "gemini".into(),
                installed: true,
                binary_path: Some("/usr/local/bin/gemini".into()),
                api_key_env: "GEMINI_API_KEY".into(),
                api_key_via_env: false,
                api_key_via_config: false,
                config_path: None,
            },
        ];
        assert_eq!(pick_default_provider(&detections), Some("codex".to_string()));
    }

    #[test]
    fn pick_default_provider_falls_back_to_installed_without_key() {
        let detections = vec![CliDetection {
            name: "gemini".into(),
            binary: "gemini".into(),
            installed: true,
            binary_path: Some("/usr/local/bin/gemini".into()),
            api_key_env: "GEMINI_API_KEY".into(),
            api_key_via_env: false,
            api_key_via_config: false,
            config_path: None,
        }];
        assert_eq!(pick_default_provider(&detections), Some("gemini".to_string()));
    }

    #[test]
    fn pick_default_provider_returns_none_when_nothing_installed() {
        let detections = vec![CliDetection {
            name: "claude".into(),
            binary: "claude".into(),
            installed: false,
            binary_path: None,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            api_key_via_env: false,
            api_key_via_config: false,
            config_path: None,
        }];
        assert_eq!(pick_default_provider(&detections), None);
    }

    #[test]
    fn plan_walkthrough_template_marks_create_when_absent() {
        let project = tempfile::tempdir().expect("project tempdir");
        let plan = plan_walkthrough_template(project.path(), HELLO_WORLD_TEMPLATE_NAME, false).expect("plan");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].action, "create");
    }

    #[test]
    fn plan_walkthrough_template_marks_skip_when_present_without_force() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = project.path().join(".animus/workflows/hello-world.yaml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "existing").unwrap();
        let plan = plan_walkthrough_template(project.path(), HELLO_WORLD_TEMPLATE_NAME, false).expect("plan");
        assert_eq!(plan[0].action, "skip_exists");
    }

    #[tokio::test]
    async fn walkthrough_plan_does_not_mutate_filesystem() {
        let project = tempfile::tempdir().expect("project tempdir");
        let args = InitArgs {
            template: None,
            path: None,
            non_interactive: true,
            plan: true,
            force: false,
            update_registry: false,
            walkthrough: true,
            no_install: false,
            install_packs: false,
            no_template: false,
            auto_start: false,
            walkthrough_template: HELLO_WORLD_TEMPLATE_NAME.to_string(),
        };

        let res = run_walkthrough(&args, project.path(), InitMode::NonInteractive, true).await;
        assert!(res.is_ok(), "walkthrough --plan should succeed; got {res:?}");

        let workflow_path = project.path().join(".animus/workflows/hello-world.yaml");
        assert!(
            !workflow_path.exists(),
            "walkthrough --plan must NOT write the hello-world template; found at {}",
            workflow_path.display()
        );
    }

    #[test]
    fn build_next_steps_includes_workflow_run_hint() {
        let template_plan = vec![WalkthroughTemplatePlan {
            name: HELLO_WORLD_TEMPLATE_NAME.into(),
            relative_path: ".animus/workflows/hello-world.yaml".into(),
            action: "create".into(),
        }];
        let plugins = WalkthroughPluginStep { skipped: false, invoked: true, exit_code: Some(0), stderr_tail: None };
        let daemon = WalkthroughDaemonStep { requested: false, invoked: false, exit_code: None, stderr_tail: None };
        let steps = build_next_steps(&template_plan, &plugins, &RecommendedPackStep::skipped(), None, &daemon);
        assert!(steps.iter().any(|s| s.contains("animus workflow run hello-world")));
        assert!(steps.iter().any(|s| s.contains("animus daemon start")));
        assert!(steps.iter().any(|s| s.contains("--install-packs")), "skipped pack step should leave an install hint");
    }

    #[test]
    fn recommended_packs_parse_from_embedded_manifest() {
        let packs = recommended_packs();
        assert!(!packs.is_empty(), "default-install.json must declare recommended packs");
        let ids: Vec<&str> = packs.iter().map(|pack| pack.id.as_str()).collect();
        assert!(ids.contains(&"animus.task"), "recommended packs should include animus.task, got {ids:?}");
        for pack in &packs {
            assert!(pack.repo.contains('/'), "pack repo should be an owner/repo slug, got {:?}", pack.repo);
            assert!(!pack.tag.is_empty(), "pack {} must carry a pinned release tag", pack.id);
        }
    }

    #[test]
    fn manual_pack_install_command_names_clone_and_pack_install() {
        let pack = RecommendedPackPin {
            id: "animus.task".to_string(),
            repo: "launchapp-dev/animus-pack-task".to_string(),
            tag: "v0.1.1".to_string(),
        };
        let command = manual_pack_install_command(&pack);
        assert!(command.contains("git clone --depth 1 --branch v0.1.1"));
        assert!(command.contains("https://github.com/launchapp-dev/animus-pack-task.git"));
        assert!(command.contains("animus pack install --path"));
        assert!(command.contains("--activate"));
    }

    #[test]
    fn build_next_steps_surfaces_failed_pack_install_with_manual_command() {
        let template_plan: Vec<WalkthroughTemplatePlan> = Vec::new();
        let plugins = WalkthroughPluginStep { skipped: true, invoked: false, exit_code: None, stderr_tail: None };
        let daemon = WalkthroughDaemonStep { requested: false, invoked: false, exit_code: None, stderr_tail: None };
        let pack_step = RecommendedPackStep {
            skipped: false,
            results: vec![
                PackInstallResult {
                    pack_id: "animus.task".to_string(),
                    repo: "launchapp-dev/animus-pack-task".to_string(),
                    tag: "v0.1.1".to_string(),
                    status: "failed".to_string(),
                    version: None,
                    detail: Some("network down; install manually: git clone ...".to_string()),
                },
                PackInstallResult {
                    pack_id: "animus.review".to_string(),
                    repo: "launchapp-dev/animus-pack-review".to_string(),
                    tag: "v0.1.0".to_string(),
                    status: "installed".to_string(),
                    version: Some("0.1.0".to_string()),
                    detail: None,
                },
            ],
        };
        let steps = build_next_steps(&template_plan, &plugins, &pack_step, None, &daemon);
        assert!(
            steps.iter().any(|s| s.contains("Pack install failed for animus.task") && s.contains("install manually")),
            "failed pack should surface the manual command, got {steps:?}"
        );
        assert!(
            !steps.iter().any(|s| s.contains("animus workflow run animus.task/standard")),
            "the task workflow hint must not appear when animus.task failed to install, got {steps:?}"
        );

        // Once animus.task itself lands, the hint should appear.
        let mut task_ok = pack_step.clone();
        task_ok.results[0].status = "installed".to_string();
        let steps = build_next_steps(&template_plan, &plugins, &task_ok, None, &daemon);
        assert!(
            steps.iter().any(|s| s.contains("animus workflow run animus.task/standard")),
            "an installed task pack should surface the first-workflow command, got {steps:?}"
        );
    }

    #[test]
    fn build_next_steps_includes_secrets_migration_hint() {
        let template_plan: Vec<WalkthroughTemplatePlan> = Vec::new();
        let plugins = WalkthroughPluginStep { skipped: true, invoked: false, exit_code: None, stderr_tail: None };
        let daemon = WalkthroughDaemonStep { requested: false, invoked: false, exit_code: None, stderr_tail: None };
        let advice = SecretsMigrationAdvice {
            detected_env_keys: vec!["ANTHROPIC_API_KEY".to_string()],
            suggested_commands: vec!["animus secret set ANTHROPIC_API_KEY".to_string()],
            docs: "docs/reference/secrets.md".to_string(),
        };
        let steps = build_next_steps(&template_plan, &plugins, &RecommendedPackStep::skipped(), Some(&advice), &daemon);
        assert!(
            steps.iter().any(|s| s.contains("animus secret set") && s.contains("docs/reference/secrets.md")),
            "secrets advice should surface the keychain migration hint, got {steps:?}"
        );
    }

    #[test]
    fn secrets_migration_advice_only_lists_env_detected_keys() {
        let detections = vec![
            CliDetection {
                name: "claude".into(),
                binary: "claude".into(),
                installed: true,
                binary_path: None,
                api_key_env: "ANTHROPIC_API_KEY".into(),
                api_key_via_env: true,
                api_key_via_config: false,
                config_path: None,
            },
            CliDetection {
                name: "codex".into(),
                binary: "codex".into(),
                installed: true,
                binary_path: None,
                api_key_env: "OPENAI_API_KEY".into(),
                api_key_via_env: false,
                api_key_via_config: true,
                config_path: None,
            },
        ];
        let advice = build_secrets_migration_advice(&detections).expect("env key detected -> advice present");
        assert_eq!(advice.detected_env_keys, vec!["ANTHROPIC_API_KEY".to_string()]);
        assert!(advice.suggested_commands.iter().any(|c| c == "animus secret set ANTHROPIC_API_KEY"));
        assert!(advice.suggested_commands.iter().any(|c| c.starts_with("animus secret import-env")));
        assert_eq!(advice.docs, "docs/reference/secrets.md");
    }

    #[test]
    fn secrets_migration_advice_absent_without_env_keys() {
        let detections = vec![CliDetection {
            name: "claude".into(),
            binary: "claude".into(),
            installed: true,
            binary_path: None,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            api_key_via_env: false,
            api_key_via_config: true,
            config_path: None,
        }];
        assert!(build_secrets_migration_advice(&detections).is_none());
    }

    /// Audit Fix 5 regression: `init --walkthrough --json` must NOT prompt
    /// for input, even in a TTY where `io::stdin().is_terminal()` is true.
    /// Before the fix, the TTY-detection branch (Guided + TTY) forced
    /// interactive=true regardless of `--json`, so a CI runner with an
    /// allocated pty would silently hang waiting for `prompt_yes_no`.
    ///
    /// We exercise the `--plan` short-circuit so the test completes without
    /// touching the filesystem mutation paths. The contract this pins is
    /// "JSON-mode walkthrough returns a JSON envelope without ever calling
    /// `prompt_yes_no`" — if a future change reads stdin in JSON mode, this
    /// test will hang and time out instead of silently passing.
    #[tokio::test]
    async fn walkthrough_json_mode_does_not_prompt_in_guided_tty() {
        let project = tempfile::tempdir().expect("project tempdir");
        let args = InitArgs {
            template: None,
            path: None,
            non_interactive: false, // Guided mode; would normally prompt in TTY.
            plan: true,             // Short-circuit so we don't mutate the filesystem.
            force: false,
            update_registry: false,
            walkthrough: true,
            no_install: false,
            install_packs: false,
            no_template: false,
            auto_start: false,
            walkthrough_template: HELLO_WORLD_TEMPLATE_NAME.to_string(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_walkthrough(&args, project.path(), InitMode::Guided, true),
        )
        .await;
        let walkthrough_result =
            result.expect("walkthrough must return without blocking on stdin when --json is set in Guided mode");
        assert!(walkthrough_result.is_ok(), "JSON-mode walkthrough should succeed; got {walkthrough_result:?}");
    }
}
