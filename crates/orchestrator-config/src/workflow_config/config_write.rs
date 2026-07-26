//! Config write-back orchestration: ship the full canonical [`WorkflowConfig`]
//! to the installed `config_source` plugin, plus the kernel-side
//! read-modify-write entity edits (manage one agent / one workflow) that load
//! the current model, mutate the targeted entity, validate, and write the full
//! model back.
//!
//! The kernel is the COMPILER/validator; the plugin only ever sees
//! `config/write` (a coarse full-model write). See
//! `animus-config-protocol`'s `METHOD_CONFIG_WRITE` for the wire contract.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use animus_config_protocol::agent_types::{AgentProfileOverlay, PhaseExecutionDefinition};

use super::config_source_client::{resolve_plugin_base, write_plugin_config};
use super::loading::compile_workflow_config_onto_base;
use super::types::{WorkflowConfig, WorkflowDefinition};

/// Validate `config` and then persist it through the installed (writable)
/// `config_source` plugin. This is the single chokepoint every write-back path
/// funnels through, so validation can never be skipped.
///
/// Validation runs against the POST-MERGE COMPILED config, not the raw model:
/// `config` is treated as the source base, pack overlays (installed +
/// project-override) are merged on top, and the kernel validator runs on the
/// merged result — exactly what the runtime load path does. This way a source
/// edit that references a pack-provided phase is accepted, and an edit that
/// only becomes invalid after the pack merge (e.g. removing an agent a pack
/// phase targets) is rejected. Only the RAW `config` is persisted; the merged
/// view is throwaway and recomputed on every load.
///
/// Returns an actionable error when no config_source plugin is installed or the
/// installed source is not writable (e.g. the read-only YAML source).
pub fn write_full_workflow_config(project_root: &Path, config: &WorkflowConfig) -> Result<()> {
    compile_workflow_config_onto_base(project_root, config.clone())
        .context("the config to write is invalid once pack overlays are merged; refusing to persist")?;
    write_plugin_config(project_root, config)
}

/// Load the RAW canonical model the config_source plugin serves (via
/// `config/load`, BEFORE pack-overlay merge), apply `mutate`, validate, and
/// write the full model back. Shared by every entity-level verb so they all
/// validate-before-write through one path.
///
/// We deliberately read the raw plugin base — not the pack-merged runtime
/// config from `load_workflow_config` — so an entity edit only ever persists
/// what the source itself owns. Writing the merged config back would bake
/// pack-supplied agents/workflows into the source (e.g. removing a
/// pack-provided workflow would "succeed" yet reappear on the next merge).
fn read_modify_write<F>(project_root: &Path, mutate: F) -> Result<WorkflowConfig>
where
    F: FnOnce(&mut WorkflowConfig) -> Result<()>,
{
    // Config authoring (WU-F) is out of scope for the actor wave: this in-tree
    // write path runs without an authenticated actor, so it reads the global
    // (`actor = None`) base. A future wave threads the writer's actor here.
    let (mut config, _cache_token) = resolve_plugin_base(project_root, None)
        .context("loading the raw config from the config_source plugin before mutating it")?
        .ok_or_else(|| {
            anyhow!(
                "no config_source plugin is installed, so there is no config to modify; install a writable source such as `animus-config-postgres`"
            )
        })?;
    mutate(&mut config)?;
    write_full_workflow_config(project_root, &config)?;
    Ok(config)
}

/// Upsert (create or replace) an agent profile keyed by `agent_id`. The overlay
/// is taken verbatim; the kernel validates the resulting full config before it
/// is written.
pub fn upsert_agent_profile(
    project_root: &Path,
    agent_id: &str,
    profile: AgentProfileOverlay,
) -> Result<WorkflowConfig> {
    let agent_id = agent_id.trim();
    if agent_id.is_empty() {
        return Err(anyhow!("agent id must not be empty"));
    }
    read_modify_write(project_root, |config| {
        config.agent_profiles.insert(agent_id.to_string(), profile);
        Ok(())
    })
}

/// Remove an agent profile by id. Errors if no such agent exists so callers get
/// a clear "nothing removed" signal instead of a silent no-op.
pub fn remove_agent_profile(project_root: &Path, agent_id: &str) -> Result<WorkflowConfig> {
    let agent_id = agent_id.trim();
    read_modify_write(project_root, |config| {
        if config.agent_profiles.remove(agent_id).is_none() {
            return Err(anyhow!("no agent profile with id '{agent_id}' exists in the current config"));
        }
        Ok(())
    })
}

/// Upsert (create or replace) a phase definition keyed by `phase_id` on the RAW
/// config_source base's `phase_definitions`. This is the config-source authoring
/// path — distinct from the agent-runtime overlay that `animus workflow phases
/// upsert` writes. Writing here means a subsequently-set workflow that references
/// the phase resolves during the post-pack-merge validation, instead of failing
/// with "references unknown phase". The kernel validates the resulting full
/// config before it is written.
pub fn set_phase_definition(
    project_root: &Path,
    phase_id: &str,
    definition: PhaseExecutionDefinition,
) -> Result<WorkflowConfig> {
    let phase_id = phase_id.trim();
    if phase_id.is_empty() {
        return Err(anyhow!("phase id must not be empty"));
    }
    read_modify_write(project_root, |config| {
        // Phase ids resolve case-insensitively everywhere else (validation's
        // reference check and the runtime `phase_execution` lookup both use
        // `eq_ignore_ascii_case`). Drop any existing key that differs only by
        // case before inserting, so an upsert REPLACES the phase instead of
        // leaving a stale duplicate that could win by map order.
        config.phase_definitions.retain(|existing, _| !existing.eq_ignore_ascii_case(phase_id));
        config.phase_definitions.insert(phase_id.to_string(), definition);
        Ok(())
    })
}

/// Upsert (create or replace) a workflow definition by its `id`. The definition
/// replaces any existing entry with the same id; otherwise it is appended.
pub fn upsert_workflow_definition(project_root: &Path, definition: WorkflowDefinition) -> Result<WorkflowConfig> {
    if definition.id.trim().is_empty() {
        return Err(anyhow!("workflow definition id must not be empty"));
    }
    read_modify_write(project_root, |config| {
        if let Some(existing) = config.workflows.iter_mut().find(|w| w.id == definition.id) {
            *existing = definition;
        } else {
            config.workflows.push(definition);
        }
        Ok(())
    })
}

/// Remove a workflow definition by id. Errors if no such workflow exists.
pub fn remove_workflow_definition(project_root: &Path, workflow_id: &str) -> Result<WorkflowConfig> {
    let workflow_id = workflow_id.trim();
    read_modify_write(project_root, |config| {
        let before = config.workflows.len();
        config.workflows.retain(|w| w.id != workflow_id);
        if config.workflows.len() == before {
            return Err(anyhow!("no workflow definition with id '{workflow_id}' exists in the current config"));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_config_protocol::agent_types::AgentProfileOverlay;
    use animus_config_protocol::builtins::builtin_workflow_config;

    // The entity read-modify-write mutation closures are the unit under test
    // here: they take a `&mut WorkflowConfig` and either succeed with the
    // intended change or return an actionable error. We exercise them against
    // an in-memory config directly (no plugin spawn) by replicating the closure
    // bodies the public verbs use; the full validate-before-write gate is
    // covered by the CLI-layer integration test.

    fn sample_agent() -> AgentProfileOverlay {
        AgentProfileOverlay::default()
    }

    fn sample_workflow(id: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            phases: Vec::new(),
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        }
    }

    #[test]
    fn upsert_agent_inserts_then_replaces() {
        let mut config = builtin_workflow_config();
        let before = config.agent_profiles.len();
        config.agent_profiles.insert("new-agent".to_string(), sample_agent());
        assert_eq!(config.agent_profiles.len(), before + 1);
        // Replace: same key, no growth.
        config.agent_profiles.insert("new-agent".to_string(), sample_agent());
        assert_eq!(config.agent_profiles.len(), before + 1);
    }

    #[test]
    fn remove_agent_reports_missing() {
        let mut config = builtin_workflow_config();
        assert!(config.agent_profiles.remove("does-not-exist").is_none());
    }

    #[test]
    fn upsert_workflow_appends_then_replaces_by_id() {
        let mut config = builtin_workflow_config();
        config.workflows.clear();
        let upsert = |config: &mut WorkflowConfig, def: WorkflowDefinition| {
            if let Some(existing) = config.workflows.iter_mut().find(|w| w.id == def.id) {
                *existing = def;
            } else {
                config.workflows.push(def);
            }
        };
        upsert(&mut config, sample_workflow("a"));
        upsert(&mut config, sample_workflow("b"));
        assert_eq!(config.workflows.len(), 2);
        let mut replaced = sample_workflow("a");
        replaced.name = "renamed".to_string();
        upsert(&mut config, replaced);
        assert_eq!(config.workflows.len(), 2, "replace must not grow the list");
        assert_eq!(config.workflows.iter().find(|w| w.id == "a").unwrap().name, "renamed");
    }

    #[test]
    fn remove_workflow_retains_others() {
        let mut config = builtin_workflow_config();
        config.workflows = vec![sample_workflow("a"), sample_workflow("b")];
        config.workflows.retain(|w| w.id != "a");
        assert_eq!(config.workflows.len(), 1);
        assert_eq!(config.workflows[0].id, "b");
    }

    #[test]
    fn write_rejects_invalid_config_before_touching_any_source() {
        // An invalid config (a workflow referencing a non-existent phase) must
        // fail at the validate gate, before write_plugin_config is ever called —
        // so even with no config_source installed the error is the validation
        // error, not "no plugin installed". This proves validation precedes the
        // write attempt.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = builtin_workflow_config();
        let mut broken = sample_workflow("broken");
        broken.phases =
            vec![animus_config_protocol::workflow_types::WorkflowPhaseEntry::Simple("does-not-exist".to_string())];
        config.workflows = vec![broken];
        let err = write_full_workflow_config(dir.path(), &config).expect_err("invalid config must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("invalid"), "error must flag the invalid config, got: {msg}");
    }

    #[test]
    fn phase_authored_on_base_lets_a_referencing_workflow_validate() {
        // The whole point of `set_phase_definition`: a phase written to the
        // config_source base's `phase_definitions` must resolve when a
        // subsequently-set workflow references it — no "references unknown
        // phase". We drive the validate gate directly (write_full_workflow_config
        // validates BEFORE it attempts the plugin write), so phase-reference
        // resolution is exercised without a live writable plugin.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = builtin_workflow_config();
        // Minimal valid phase definition (agent mode needs no agent_id).
        let phase: PhaseExecutionDefinition = serde_json::from_str(r#"{"mode":"agent"}"#).expect("valid phase json");
        config.phase_definitions.insert("authored-phase".to_string(), phase);
        let mut wf = sample_workflow("uses-authored");
        wf.phases =
            vec![animus_config_protocol::workflow_types::WorkflowPhaseEntry::Simple("authored-phase".to_string())];
        config.workflows = vec![wf];

        // Validation PASSES: the only remaining failure is the write step (no
        // writable config_source plugin under test), never an "unknown phase".
        let err = write_full_workflow_config(dir.path(), &config).expect_err("no writable source under test");
        let msg = format!("{err:#}");
        assert!(!msg.contains("unknown phase"), "phase authored on the base must resolve; got: {msg}");
        assert!(
            msg.contains("no config_source plugin") || msg.contains("does not support writes"),
            "should fail only at the write step, got: {msg}"
        );

        // Control: WITHOUT the phase definition the same workflow reference is
        // rejected as unknown — proving it is the authored phase that unblocks it.
        let mut missing = builtin_workflow_config();
        let mut wf2 = sample_workflow("uses-authored");
        wf2.phases =
            vec![animus_config_protocol::workflow_types::WorkflowPhaseEntry::Simple("authored-phase".to_string())];
        missing.workflows = vec![wf2];
        let err2 = write_full_workflow_config(dir.path(), &missing).expect_err("unknown phase must be rejected");
        let msg2 = format!("{err2:#}");
        assert!(msg2.contains("unknown phase"), "control must fail as unknown phase; got: {msg2}");
    }

    #[test]
    fn write_to_non_writable_or_absent_source_is_actionable() {
        // A VALID config that passes the validate gate then hits the write path.
        // Depending on the test host either no config_source plugin is installed
        // (=> "no config_source plugin is installed") or only the read-only YAML
        // source is (=> the writable-capability rejection naming the plugin).
        // Both are clean, actionable errors — never a panic or partial write.
        let dir = tempfile::tempdir().expect("tempdir");
        let config = builtin_workflow_config();
        let err = write_full_workflow_config(dir.path(), &config).expect_err("non-writable/absent source => error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no config_source plugin") || msg.contains("does not support writes"),
            "expected actionable no-plugin or read-only error, got: {msg}"
        );
    }
}
