use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use orchestrator_config::{
    list_bundled_project_templates, load_bundled_project_template, load_pack_inventory, load_pack_selection_state,
    load_project_template_from_dir, save_pack_selection_state, LoadedProjectTemplate, PackRegistrySource,
    PackSelectionEntry, PackSelectionSource, ProjectTemplateSourceKind, ProjectTemplateSummary,
};
use orchestrator_core::{
    daemon_project_config_path, load_daemon_project_config, update_daemon_project_config, write_daemon_project_config,
    DaemonProjectConfig, DaemonProjectConfigPatch, DoctorCheckStatus, DoctorReport, FileServiceHub,
};
use serde::Serialize;

use crate::{conflict_error, invalid_input_error, print_value, InitArgs};

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
struct InitFieldPlan {
    field: String,
    before: bool,
    after: bool,
    changed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InitBlockedItem {
    check_id: String,
    details: String,
    remediation: String,
}

#[derive(Debug, Clone)]
struct DesiredDaemonConfig {
    auto_merge_enabled: bool,
    auto_pr_enabled: bool,
    auto_commit_before_merge: bool,
}

pub(crate) async fn handle_init(args: InitArgs, project_root: &str, json: bool) -> Result<()> {
    let mode = if args.non_interactive { InitMode::NonInteractive } else { InitMode::Guided };
    let project_root_path = Path::new(project_root);
    let loaded_template = resolve_template(&args, mode)?;
    ensure_supported_template_source_mode(&loaded_template)?;

    let current_config = load_daemon_project_config(project_root_path)?;
    let desired_config = resolve_desired_config(&args, &loaded_template, &current_config);
    let daemon_plan = daemon_field_plan(&current_config, &desired_config);

    let template_output = InitTemplateOutput {
        id: loaded_template.manifest.id.clone(),
        version: loaded_template.manifest.version.clone(),
        title: loaded_template.manifest.title.clone(),
        description: loaded_template.manifest.description.clone(),
        pattern: loaded_template.manifest.pattern.clone(),
        source_kind: loaded_template.source_kind,
    };

    let existing_before = existing_template_paths(project_root_path, &loaded_template);
    let template_file_plan = build_template_file_plan(project_root_path, &loaded_template, &existing_before, args.force);
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
                "apply": {
                    "applied": false,
                    "changed_domains": [],
                    "unchanged_domains": ["template_files", "daemon_config", "pack_selection"],
                },
                "next_steps": loaded_template.manifest.next_steps,
            }),
            json,
        );
    }

    fail_on_conflicting_paths(&existing_before, args.force)?;

    let bootstrap_needed_before = remediation_needed(&doctor_before, "bootstrap_project_state");
    let daemon_config_exists_before = daemon_project_config_path(project_root_path).exists();

    FileServiceHub::new(project_root_path)?;
    let written_files = write_template_files(project_root_path, &loaded_template)?;
    let pack_selection_updated = apply_template_packs(project_root_path, &loaded_template)?;
    let daemon_config_updated =
        persist_desired_daemon_config(project_root_path, &desired_config, daemon_config_exists_before)?;
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
    if pack_selection_updated {
        changed_domains.push("pack_selection");
    } else {
        unchanged_domains.push("pack_selection");
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
                "pack_selection_updated": pack_selection_updated,
            },
            "doctor_before": doctor_before,
            "doctor_after": doctor_after,
            "next_steps": loaded_template.manifest.next_steps,
        }),
        json,
    )
}

fn resolve_template(args: &InitArgs, mode: InitMode) -> Result<LoadedProjectTemplate> {
    match (args.template.as_deref(), args.path.as_deref()) {
        (Some(template_id), None) => load_bundled_project_template(template_id),
        (None, Some(path)) => {
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
                let template = prompt_template_selection(&list_bundled_project_templates()?)?;
                load_bundled_project_template(&template.id)
            }
            InitMode::NonInteractive => {
                Err(invalid_input_error("non-interactive init requires --template or --path"))
            }
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

fn resolve_desired_config(
    args: &InitArgs,
    template: &LoadedProjectTemplate,
    current: &DaemonProjectConfig,
) -> DesiredDaemonConfig {
    DesiredDaemonConfig {
        auto_merge_enabled: args
            .auto_merge
            .unwrap_or(template.manifest.daemon.auto_merge.unwrap_or(current.auto_merge_enabled)),
        auto_pr_enabled: args.auto_pr.unwrap_or(template.manifest.daemon.auto_pr.unwrap_or(current.auto_pr_enabled)),
        auto_commit_before_merge: args.auto_commit_before_merge.unwrap_or(
            template
                .manifest
                .daemon
                .auto_commit_before_merge
                .unwrap_or(current.auto_commit_before_merge),
        ),
    }
}

fn existing_template_paths(project_root: &Path, template: &LoadedProjectTemplate) -> BTreeSet<PathBuf> {
    template
        .files
        .iter()
        .map(|file| project_root.join(&file.relative_path))
        .filter(|path| path.exists())
        .collect()
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
                if force { "overwrite" } else { "conflict" }
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
            let entry = inventory
                .entries
                .iter()
                .find(|entry| entry.pack_id.eq_ignore_ascii_case(&pack.id) && pack_source_matches(pack.source, entry.source));
            let action = if pack.activate {
                if entry.is_some() { "activate" } else { "missing" }
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

fn apply_template_packs(project_root: &Path, template: &LoadedProjectTemplate) -> Result<bool> {
    if template.manifest.packs.is_empty() {
        return Ok(false);
    }

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
            .find(|entry| entry.pack_id.eq_ignore_ascii_case(&pack.id) && pack_source_matches(pack.source, entry.source))
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
    Ok(updated)
}

fn selection_source_for(source: PackRegistrySource) -> PackSelectionSource {
    match source {
        PackRegistrySource::Bundled => PackSelectionSource::Bundled,
        PackRegistrySource::Installed => PackSelectionSource::Installed,
        PackRegistrySource::ProjectOverride => PackSelectionSource::ProjectOverride,
    }
}

fn prompt_template_selection(templates: &[ProjectTemplateSummary]) -> Result<ProjectTemplateSummary> {
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
        io::stdin().read_line(&mut input)?;
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

fn persist_desired_daemon_config(
    project_root: &Path,
    desired: &DesiredDaemonConfig,
    daemon_config_exists_before: bool,
) -> Result<bool> {
    if !daemon_config_exists_before {
        let mut config = load_daemon_project_config(project_root)?;
        config.auto_merge_enabled = desired.auto_merge_enabled;
        config.auto_pr_enabled = desired.auto_pr_enabled;
        config.auto_commit_before_merge = desired.auto_commit_before_merge;
        write_daemon_project_config(project_root, &config)?;
        return Ok(true);
    }

    let patch = DaemonProjectConfigPatch {
        auto_merge_enabled: Some(desired.auto_merge_enabled),
        auto_pr_enabled: Some(desired.auto_pr_enabled),
        auto_commit_before_merge: Some(desired.auto_commit_before_merge),
    };
    let (_, updated) = update_daemon_project_config(project_root, &patch)?;
    Ok(updated)
}

fn daemon_field_plan(current: &DaemonProjectConfig, desired: &DesiredDaemonConfig) -> Vec<InitFieldPlan> {
    vec![
        field_plan("auto_merge_enabled", current.auto_merge_enabled, desired.auto_merge_enabled),
        field_plan("auto_pr_enabled", current.auto_pr_enabled, desired.auto_pr_enabled),
        field_plan("auto_commit_before_merge", current.auto_commit_before_merge, desired.auto_commit_before_merge),
    ]
}

fn field_plan(field: &str, before: bool, after: bool) -> InitFieldPlan {
    InitFieldPlan { field: field.to_string(), before, after, changed: before != after }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_template_file_plan_marks_conflicts_without_force() {
        let template = load_bundled_project_template("task-queue").expect("template should load");
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let conflict_path = temp.path().join(".ao/workflows/custom.yaml");
        std::fs::create_dir_all(conflict_path.parent().expect("parent")).expect("parent should exist");
        std::fs::write(&conflict_path, "existing").expect("existing file should be written");
        let existing = existing_template_paths(temp.path(), &template);

        let plan = build_template_file_plan(temp.path(), &template, &existing, false);
        assert!(plan.iter().any(|file| file.action == "conflict"));
    }

    #[test]
    fn resolve_desired_config_prefers_explicit_overrides() {
        let template = load_bundled_project_template("direct-workflow").expect("template should load");
        let current =
            DaemonProjectConfig { auto_merge_enabled: true, auto_pr_enabled: true, auto_commit_before_merge: true };
        let args = InitArgs {
            template: Some("direct-workflow".to_string()),
            path: None,
            non_interactive: true,
            plan: false,
            force: false,
            auto_merge: Some(true),
            auto_pr: None,
            auto_commit_before_merge: Some(false),
        };

        let desired = resolve_desired_config(&args, &template, &current);
        assert!(desired.auto_merge_enabled);
        assert!(!desired.auto_pr_enabled);
        assert!(!desired.auto_commit_before_merge);
    }
}
