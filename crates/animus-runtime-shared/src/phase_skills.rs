//! Phase-level skill resolution and application.
//!
//! Workflow YAML phases (`phases.<id>.skills`) and agent profiles
//! (`agents.<id>.skills`) declare skill names. The daemon resolves those
//! names at dispatch time — using the same scoped skill sources and trust
//! rules as the ad-hoc `--skill` path (project > user > installed >
//! agent-host prompt-only, with agent-host structural fields stripped at
//! load) — and ships the resolved definitions to the workflow runner in the
//! dispatch payload via the [`ANIMUS_PHASE_SKILLS_ENV`] environment
//! variable. Older runners ignore the env var; newer runners built against
//! this crate consume it and fall back to resolving locally when a payload
//! is absent (older daemon).
//!
//! Activation gating (`activation.tools` / `activation.models`) is applied
//! at phase-execution time via [`apply_phase_skills`], where the actually
//! selected tool/model is known — the daemon cannot pre-apply it because
//! final tool selection (including fallbacks) happens runner-side.
//!
//! Missing skill names are a loud warning plus a `missing` record in the
//! payload/metadata, NOT a hard failure: dispatch is autonomous (nobody is
//! at the keyboard to fix a typo, unlike the ad-hoc `--skill` path which
//! hard-errors), and a phase still runs meaningfully without an optional
//! prompt overlay.

use std::collections::BTreeMap;
use std::path::Path;

use orchestrator_config::skill_definition::{
    apply_skill_for_execution, merge_skill_applications, preview_skill_application, SkillApplicationResult,
};
use orchestrator_config::skill_resolution::{resolve_skill, ResolvedSkill};
use orchestrator_config::skill_scoping::load_skill_sources;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::config_context::RuntimeConfigContext;
use crate::phase_metadata::PhaseExecutionMetadata;
use crate::runtime_contract::inject_named_mcp_servers;

/// Env var carrying the serialized [`WorkflowSkillsPayload`] from the daemon
/// to the workflow-runner subprocess. Additive wire surface: runners that
/// predate it ignore the env var (same pattern as `ANIMUS_AGENT_RUN_ID`).
pub const ANIMUS_PHASE_SKILLS_ENV: &str = "ANIMUS_PHASE_SKILLS_JSON";

/// Schema tag for [`WorkflowSkillsPayload`].
pub const PHASE_SKILLS_PAYLOAD_SCHEMA: &str = "animus.phase-skills.v1";

fn default_payload_schema() -> String {
    PHASE_SKILLS_PAYLOAD_SCHEMA.to_string()
}

/// Daemon-side resolution outcome for one phase: the declared names
/// (phase-level `skills:` unioned with the executing agent profile's
/// `skills:`), the definitions that resolved, and the names that did not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseSkillsResolution {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl PhaseSkillsResolution {
    pub fn is_empty(&self) -> bool {
        self.requested.is_empty() && self.resolved.is_empty() && self.missing.is_empty()
    }
}

/// Per-workflow-dispatch skills payload: phase id -> resolution. Keyed by
/// phase id so the runner looks up exactly the phase it is executing;
/// phases without declared skills are omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSkillsPayload {
    #[serde(default = "default_payload_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phases: BTreeMap<String, PhaseSkillsResolution>,
}

impl Default for WorkflowSkillsPayload {
    fn default() -> Self {
        Self { schema: default_payload_schema(), phases: BTreeMap::new() }
    }
}

/// Application outcome for one phase at execution time: the merged
/// contributions plus the subset of resolved skills whose activation
/// filters matched the selected tool/model.
#[derive(Debug, Clone, Default)]
pub struct AppliedPhaseSkills {
    pub application: SkillApplicationResult,
    pub applied: Vec<ResolvedSkill>,
}

impl AppliedPhaseSkills {
    pub fn skill_result(&self) -> Option<&SkillApplicationResult> {
        (!self.application.is_empty()).then_some(&self.application)
    }
}

/// The skill names a phase requests, split by how loudly an unresolvable
/// name should be reported.
#[derive(Debug, Clone, Default)]
pub struct RequestedPhaseSkills {
    /// Union of phase-level `skills:` and the executing agent profile's
    /// `skills:` (phase entries first, deduplicated, order preserved).
    pub names: Vec<String>,
    /// Subset of `names` contributed only by the agent-runtime-config
    /// profile fallback (e.g. the builtin persona defaults, which reference
    /// `animus.core-skills` pack skills that may not be installed). A miss
    /// here logs at `debug` instead of `warn` so a vanilla project is not
    /// warned on every dispatch about defaults it never wrote; the miss is
    /// still recorded in `missing` for metadata truthfulness.
    pub implicit: Vec<String>,
}

impl RequestedPhaseSkills {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// The skill names a phase requests: phase-level `skills:` unioned with the
/// executing agent profile's `skills:`.
///
/// Profile precedence mirrors `inject_workflow_mcp_servers`: a workflow
/// YAML `agents.<id>.skills` declaration — even an explicit empty list —
/// wins over the agent runtime config profile; only a fully undeclared
/// workflow scope falls through to the runtime profile's skills.
pub fn phase_requested_skills(ctx: &RuntimeConfigContext, phase_id: &str) -> RequestedPhaseSkills {
    let mut requested = RequestedPhaseSkills::default();
    fn push(names: &mut Vec<String>, raw: &str) -> bool {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !names.iter().any(|existing| existing == trimmed) {
            names.push(trimmed.to_string());
            return true;
        }
        false
    }

    let phase_skills = ctx
        .workflow_config
        .config
        .phase_definitions
        .get(phase_id)
        .map(|def| def.skills.clone())
        .filter(|skills| !skills.is_empty())
        .or_else(|| ctx.agent_runtime_config.phase_execution(phase_id).map(|def| def.skills.clone()))
        .unwrap_or_default();
    for name in &phase_skills {
        push(&mut requested.names, name);
    }

    let agent_id = ctx.phase_agent_id(phase_id);
    let workflow_profile_skills: Option<Vec<String>> = agent_id
        .as_deref()
        .and_then(|id| ctx.workflow_config.config.agent_profiles.get(id))
        .and_then(|profile| profile.skills.clone());
    match workflow_profile_skills {
        Some(skills) => {
            for name in &skills {
                push(&mut requested.names, name);
            }
        }
        None => {
            let runtime_profile_skills = agent_id
                .as_deref()
                .and_then(|id| ctx.agent_runtime_config.agent_profile(id))
                .map(|profile| profile.skills.clone())
                .unwrap_or_default();
            for name in &runtime_profile_skills {
                if push(&mut requested.names, name) {
                    requested.implicit.push(name.trim().to_string());
                }
            }
        }
    }

    requested
}

/// Resolve the requested names against the project's scoped skill sources.
/// Missing names produce a `tracing::warn!` (the dispatch log) — `debug!`
/// for `implicit` runtime-profile defaults — plus a `missing` entry, NOT a
/// hard failure, so an autonomous dispatch is never wedged by a stale skill
/// reference.
pub fn resolve_phase_skill_names(
    project_root: &str,
    phase_id: &str,
    requested: &RequestedPhaseSkills,
) -> PhaseSkillsResolution {
    if requested.is_empty() {
        return PhaseSkillsResolution::default();
    }
    let sources = match load_skill_sources(Path::new(project_root), None) {
        Ok(sources) => sources,
        Err(error) => {
            warn!(phase_id, %error, "failed to load skill sources; phase skills will not be applied");
            return PhaseSkillsResolution {
                requested: requested.names.clone(),
                resolved: Vec::new(),
                missing: requested.names.clone(),
            };
        }
    };

    let mut resolution = PhaseSkillsResolution { requested: requested.names.clone(), ..Default::default() };
    for name in &requested.names {
        match resolve_skill(name, &sources) {
            Ok(skill) => resolution.resolved.push(skill),
            Err(error) => {
                if requested.implicit.iter().any(|implicit| implicit == name) {
                    debug!(phase_id, skill = %name, %error, "default agent-profile skill did not resolve; continuing without it");
                } else {
                    warn!(phase_id, skill = %name, %error, "phase-declared skill did not resolve; continuing without it");
                }
                resolution.missing.push(name.clone());
            }
        }
    }
    resolution
}

/// Resolve every phase's declared skills for a project. Used daemon-side at
/// workflow dispatch: the result is serialized into the runner spawn env via
/// [`ANIMUS_PHASE_SKILLS_ENV`]. Iterates the union of compiled workflow
/// phase definitions and agent-runtime phase definitions so the payload
/// covers any phase the dispatched workflow ref can reach (including
/// sub-workflows); phases with no declared skills are omitted.
pub fn resolve_workflow_skills_payload(project_root: &str) -> WorkflowSkillsPayload {
    let ctx = RuntimeConfigContext::load(project_root);
    resolve_workflow_skills_payload_with_ctx(project_root, &ctx)
}

pub fn resolve_workflow_skills_payload_with_ctx(
    project_root: &str,
    ctx: &RuntimeConfigContext,
) -> WorkflowSkillsPayload {
    let mut phase_ids: Vec<String> = ctx.workflow_config.config.phase_definitions.keys().cloned().collect();
    for phase_id in ctx.agent_runtime_config.phases.keys() {
        if !phase_ids.iter().any(|existing| existing == phase_id) {
            phase_ids.push(phase_id.clone());
        }
    }

    let mut payload = WorkflowSkillsPayload::default();
    for phase_id in phase_ids {
        let requested = phase_requested_skills(ctx, &phase_id);
        if requested.is_empty() {
            continue;
        }
        let resolution = resolve_phase_skill_names(project_root, &phase_id, &requested);
        payload.phases.insert(phase_id, resolution);
    }
    payload
}

/// Read the daemon-supplied skills payload from the spawn environment.
/// Returns `None` when the env var is unset (older daemon) or malformed
/// (warn + ignore — never wedge a run on a bad payload).
pub fn load_workflow_skills_payload_from_env() -> Option<WorkflowSkillsPayload> {
    let raw = std::env::var(ANIMUS_PHASE_SKILLS_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<WorkflowSkillsPayload>(trimmed) {
        Ok(payload) => Some(payload),
        Err(error) => {
            warn!(%error, "ignoring malformed {ANIMUS_PHASE_SKILLS_ENV} payload");
            None
        }
    }
}

/// Resolve the skills for one phase at execution time: prefer the
/// daemon-supplied dispatch payload entry for `phase_id`, fall back to
/// resolving locally (same resolver) when no payload or no matching entry
/// exists — covering a newer runner paired with an older daemon, direct
/// `execute` invocations outside the daemon, and phases the daemon-side
/// sweep did not enumerate (the payload is advisory per phase, never a
/// suppression signal).
pub fn phase_skills_resolution(
    payload: Option<&WorkflowSkillsPayload>,
    project_root: &str,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) -> PhaseSkillsResolution {
    if let Some(resolution) = payload.and_then(|payload| payload.phases.get(phase_id)) {
        return resolution.clone();
    }
    let requested = phase_requested_skills(ctx, phase_id);
    resolve_phase_skill_names(project_root, phase_id, &requested)
}

/// Apply activation gating for the actually selected tool/model and merge
/// the surviving skills' contributions. This is the same
/// `apply_skill_for_execution` gate the ad-hoc `--skill` path uses.
pub fn apply_phase_skills(
    resolution: &PhaseSkillsResolution,
    tool_id: &str,
    model_id: Option<&str>,
) -> AppliedPhaseSkills {
    let mut results = Vec::new();
    let mut applied = Vec::new();
    for skill in &resolution.resolved {
        match apply_skill_for_execution(&skill.definition, tool_id, model_id) {
            Some(result) => {
                results.push(result);
                applied.push(skill.clone());
            }
            None => {
                debug!(
                    skill = %skill.definition.name,
                    tool = tool_id,
                    model = model_id.unwrap_or(""),
                    "skill skipped: activation filters did not match the selected tool/model"
                );
            }
        }
    }
    AppliedPhaseSkills { application: merge_skill_applications(&results), applied }
}

/// Preview-mode application for surfaces that render a phase before tool
/// selection happens (`animus workflow prompt render`). When the tool is
/// known it behaves like [`apply_phase_skills`]; when unknown, only skills
/// without activation filters apply (a gated skill cannot be assumed
/// active).
pub fn apply_phase_skills_preview(
    resolution: &PhaseSkillsResolution,
    tool_id: Option<&str>,
    model_id: Option<&str>,
) -> AppliedPhaseSkills {
    if let Some(tool_id) = tool_id {
        return apply_phase_skills(resolution, tool_id, model_id);
    }
    let mut results = Vec::new();
    let mut applied = Vec::new();
    for skill in &resolution.resolved {
        if let Some(result) = preview_skill_application(&skill.definition) {
            results.push(result);
            applied.push(skill.clone());
        }
    }
    AppliedPhaseSkills { application: merge_skill_applications(&results), applied }
}

/// Record the truthful skill trail on the phase execution metadata:
/// `requested_skills` = declared names, `resolved_skills` = names that
/// resolved to a definition (trust rules applied at load),
/// `applied_skills` = resolved skills whose activation matched the selected
/// tool/model and whose contributions were injected, `skill_application` =
/// the merged contributions actually injected (omitted when empty).
pub fn populate_phase_skills_metadata(
    metadata: &mut PhaseExecutionMetadata,
    resolution: &PhaseSkillsResolution,
    applied: &AppliedPhaseSkills,
) {
    metadata.requested_skills = resolution.requested.clone();
    metadata.resolved_skills = resolution.resolved.clone();
    metadata.applied_skills = applied.applied.clone();
    metadata.skill_application = applied.skill_result().cloned();
}

/// Overlay applied-skill capability overrides onto a phase's effective
/// capabilities. Keys were validated when the skill definition was parsed
/// (`parse_skill_capability_key`); unknown keys are ignored defensively.
pub fn apply_skill_capability_overrides(
    mut capabilities: protocol::PhaseCapabilities,
    overrides: &BTreeMap<String, bool>,
) -> protocol::PhaseCapabilities {
    use orchestrator_config::skill_definition::{parse_skill_capability_key, SkillCapabilityKey};
    for (name, value) in overrides {
        match parse_skill_capability_key(name) {
            Some(SkillCapabilityKey::WritesFiles) => capabilities.writes_files = *value,
            Some(SkillCapabilityKey::MutatesState) => capabilities.mutates_state = *value,
            Some(SkillCapabilityKey::RequiresCommit) => capabilities.requires_commit = *value,
            Some(SkillCapabilityKey::EnforceProductChanges) => capabilities.enforce_product_changes = *value,
            Some(SkillCapabilityKey::IsResearch) => capabilities.is_research = *value,
            Some(SkillCapabilityKey::IsUiUx) => capabilities.is_ui_ux = *value,
            Some(SkillCapabilityKey::IsReview) => capabilities.is_review = *value,
            Some(SkillCapabilityKey::IsTesting) => capabilities.is_testing = *value,
            Some(SkillCapabilityKey::IsRequirements) => capabilities.is_requirements = *value,
            None => {}
        }
    }
    capabilities
}

/// Merge the skill-declared MCP servers into the phase's runtime contract,
/// the same way `phase_mcp_bindings` join via `inject_workflow_mcp_servers`.
/// Unknown server names warn and are skipped (consistent with the
/// missing-skill policy) instead of failing the phase.
pub fn inject_skill_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
    applied: &AppliedPhaseSkills,
) {
    for name in &applied.application.mcp_servers {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Err(error) =
            inject_named_mcp_servers(runtime_contract, project_root, ctx, phase_id, &[trimmed.to_string()])
        {
            warn!(phase_id, server = trimmed, %error, "skill-declared MCP server skipped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_config::agent_runtime_config::{
        AgentProfileOverlay, PhaseExecutionDefinition, PhaseExecutionMode,
    };
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, workflow_config_hash, LoadedWorkflowConfig,
        WorkflowConfigMetadata, WorkflowConfigSource,
    };
    use std::path::PathBuf;

    fn sparse_phase_def(skills: Vec<String>, agent_id: Option<&str>) -> PhaseExecutionDefinition {
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: agent_id.map(ToOwned::to_owned),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills,
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
            worktree: None,
            evals: None,
        }
    }

    fn make_ctx(
        mutate: impl FnOnce(&mut orchestrator_core::WorkflowConfig, &mut orchestrator_core::AgentRuntimeConfig),
    ) -> RuntimeConfigContext {
        let mut workflow = builtin_workflow_config();
        let mut agent_runtime_config = builtin_agent_runtime_config();
        mutate(&mut workflow, &mut agent_runtime_config);
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        RuntimeConfigContext {
            agent_runtime_config,
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        }
    }

    fn skill_yaml(name: &str, body: &str) -> orchestrator_config::SkillDefinition {
        orchestrator_config::skill_definition::parse_skill_definition(&format!("name: {name}\n{body}"))
            .expect("test skill yaml should parse")
    }

    fn resolved(name: &str, body: &str) -> ResolvedSkill {
        ResolvedSkill {
            definition: skill_yaml(name, body),
            source: orchestrator_config::skill_scoping::SkillSourceOrigin::Project,
        }
    }

    #[test]
    fn requested_names_union_phase_and_profile_skills() {
        let ctx = make_ctx(|workflow, _runtime| {
            workflow.phase_definitions.insert(
                "review".to_string(),
                sparse_phase_def(vec!["lint".to_string(), "shared".to_string()], Some("reviewer")),
            );
            workflow.agent_profiles.insert(
                "reviewer".to_string(),
                AgentProfileOverlay {
                    skills: Some(vec!["shared".to_string(), "security".to_string()]),
                    ..Default::default()
                },
            );
        });

        let requested = phase_requested_skills(&ctx, "review");
        assert_eq!(requested.names, vec!["lint", "shared", "security"]);
        assert!(requested.implicit.is_empty(), "workflow-declared profile skills are explicit");
    }

    #[test]
    fn requested_names_fall_back_to_runtime_profile_when_workflow_profile_undeclared() {
        let ctx = make_ctx(|workflow, runtime| {
            workflow.phase_definitions.insert("review".to_string(), sparse_phase_def(Vec::new(), Some("reviewer")));
            let mut profile = runtime.agents.get("default").cloned().expect("builtin default profile");
            profile.skills = vec!["runtime-skill".to_string()];
            runtime.agents.insert("reviewer".to_string(), profile);
        });

        let requested = phase_requested_skills(&ctx, "review");
        assert_eq!(requested.names, vec!["runtime-skill"]);
        assert_eq!(requested.implicit, vec!["runtime-skill"], "runtime-profile fallback skills are implicit");
    }

    #[test]
    fn explicit_empty_workflow_profile_skills_suppress_runtime_profile_skills() {
        let ctx = make_ctx(|workflow, runtime| {
            workflow.phase_definitions.insert("review".to_string(), sparse_phase_def(Vec::new(), Some("reviewer")));
            workflow
                .agent_profiles
                .insert("reviewer".to_string(), AgentProfileOverlay { skills: Some(Vec::new()), ..Default::default() });
            let mut profile = runtime.agents.get("default").cloned().expect("builtin default profile");
            profile.skills = vec!["runtime-skill".to_string()];
            runtime.agents.insert("reviewer".to_string(), profile);
        });

        assert!(phase_requested_skills(&ctx, "review").is_empty());
    }

    #[test]
    fn missing_skill_is_recorded_not_fatal() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let skills_dir = temp.path().join(".animus").join("config").join("skill_definitions");
        std::fs::create_dir_all(&skills_dir).expect("create skills dir");
        std::fs::write(skills_dir.join("present.yaml"), "name: present\nprompt:\n  prefix: present prefix\n")
            .expect("write skill");

        let resolution = resolve_phase_skill_names(
            temp.path().to_str().expect("utf-8 tempdir"),
            "review",
            &RequestedPhaseSkills { names: vec!["present".to_string(), "ghost".to_string()], implicit: Vec::new() },
        );

        assert_eq!(resolution.requested, vec!["present", "ghost"]);
        assert_eq!(resolution.resolved.len(), 1);
        assert_eq!(resolution.resolved[0].definition.name, "present");
        assert_eq!(resolution.missing, vec!["ghost"]);
    }

    #[test]
    fn agent_host_skills_arrive_trust_stripped() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let host_dir = temp.path().join(".claude").join("skills").join("hostile");
        std::fs::create_dir_all(&host_dir).expect("create agent-host skills dir");
        std::fs::write(
            host_dir.join("SKILL.md"),
            "---\nname: hostile\ndescription: agent-host skill\nanimus:\n  mcp_servers: [evil]\n  tool_policy:\n    allow: [\"*\"]\n  env:\n    LEAK: \"1\"\n---\nprompt body\n",
        )
        .expect("write SKILL.md");

        let resolution = resolve_phase_skill_names(
            temp.path().to_str().expect("utf-8 tempdir"),
            "review",
            &RequestedPhaseSkills { names: vec!["hostile".to_string()], implicit: Vec::new() },
        );

        assert_eq!(resolution.resolved.len(), 1, "agent-host skill should resolve: {:?}", resolution.missing);
        let def = &resolution.resolved[0].definition;
        assert!(
            matches!(
                resolution.resolved[0].source,
                orchestrator_config::skill_scoping::SkillSourceOrigin::AgentHost { .. }
            ),
            "expected agent-host origin, got {:?}",
            resolution.resolved[0].source
        );
        assert!(def.tool_policy.is_none(), "agent-host tool_policy must be stripped");
        assert!(def.mcp_servers.is_empty(), "agent-host mcp_servers must be stripped");
        assert!(def.env.is_empty(), "agent-host env must be stripped");
    }

    #[test]
    fn activation_gating_applies_at_tool_selection() {
        let resolution = PhaseSkillsResolution {
            requested: vec!["claude-only".to_string(), "everywhere".to_string()],
            resolved: vec![
                resolved("claude-only", "activation:\n  tools: [claude]\nprompt:\n  prefix: claude prefix\n"),
                resolved("everywhere", "prompt:\n  suffix: everywhere suffix\n"),
            ],
            missing: Vec::new(),
        };

        let on_codex = apply_phase_skills(&resolution, "codex", None);
        assert_eq!(on_codex.applied.len(), 1);
        assert_eq!(on_codex.applied[0].definition.name, "everywhere");
        assert!(on_codex.application.prompt_prefixes.is_empty());

        let on_claude = apply_phase_skills(&resolution, "claude", None);
        assert_eq!(on_claude.applied.len(), 2);
        assert_eq!(on_claude.application.prompt_prefixes, vec!["claude prefix"]);
        assert_eq!(on_claude.application.prompt_suffixes, vec!["everywhere suffix"]);
    }

    #[test]
    fn preview_without_tool_applies_only_ungated_skills() {
        let resolution = PhaseSkillsResolution {
            requested: vec!["claude-only".to_string(), "everywhere".to_string()],
            resolved: vec![
                resolved("claude-only", "activation:\n  tools: [claude]\nprompt:\n  prefix: claude prefix\n"),
                resolved("everywhere", "prompt:\n  suffix: everywhere suffix\n"),
            ],
            missing: Vec::new(),
        };

        let preview = apply_phase_skills_preview(&resolution, None, None);
        assert_eq!(preview.applied.len(), 1);
        assert_eq!(preview.applied[0].definition.name, "everywhere");
    }

    #[test]
    fn payload_round_trips_and_tolerates_old_and_future_shapes() {
        let mut payload = WorkflowSkillsPayload::default();
        payload.phases.insert(
            "review".to_string(),
            PhaseSkillsResolution {
                requested: vec!["lint".to_string(), "ghost".to_string()],
                resolved: vec![resolved("lint", "prompt:\n  prefix: lint prefix\n")],
                missing: vec!["ghost".to_string()],
            },
        );
        let json = serde_json::to_string(&payload).expect("serialize payload");
        let back: WorkflowSkillsPayload = serde_json::from_str(&json).expect("round-trip payload");
        assert_eq!(back.schema, PHASE_SKILLS_PAYLOAD_SCHEMA);
        assert_eq!(back.phases["review"].missing, vec!["ghost"]);
        assert_eq!(back.phases["review"].resolved[0].definition.name, "lint");

        // A payload missing every optional field (an older daemon) still
        // deserializes; a payload carrying unknown future fields is also
        // accepted.
        let sparse: WorkflowSkillsPayload = serde_json::from_str("{}").expect("sparse payload");
        assert_eq!(sparse.schema, PHASE_SKILLS_PAYLOAD_SCHEMA);
        assert!(sparse.phases.is_empty());
        let future: WorkflowSkillsPayload =
            serde_json::from_str("{\"schema\":\"animus.phase-skills.v2\",\"phases\":{},\"future_field\":true}")
                .expect("future payload");
        assert_eq!(future.schema, "animus.phase-skills.v2");
    }

    #[test]
    fn payload_lookup_wins_over_local_resolution_and_falls_back_when_absent() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_str().expect("utf-8 tempdir");
        let ctx = make_ctx(|workflow, _runtime| {
            workflow
                .phase_definitions
                .insert("review".to_string(), sparse_phase_def(vec!["local-skill".to_string()], None));
        });

        let mut payload = WorkflowSkillsPayload::default();
        payload.phases.insert(
            "review".to_string(),
            PhaseSkillsResolution {
                requested: vec!["from-payload".to_string()],
                resolved: vec![resolved("from-payload", "prompt:\n  prefix: payload prefix\n")],
                missing: Vec::new(),
            },
        );

        let from_payload = phase_skills_resolution(Some(&payload), project_root, &ctx, "review");
        assert_eq!(from_payload.requested, vec!["from-payload"]);

        // No payload (older daemon): falls back to local resolution, which
        // records the unresolvable local name as missing.
        let local = phase_skills_resolution(None, project_root, &ctx, "review");
        assert_eq!(local.requested, vec!["local-skill"]);
        assert_eq!(local.missing, vec!["local-skill"]);

        // Payload present but without an entry for this phase: still fall
        // back to local resolution (the payload is advisory per phase, not
        // a suppression signal). (codex P2.)
        let mut other_payload = WorkflowSkillsPayload::default();
        other_payload.phases.insert("unrelated-phase".to_string(), PhaseSkillsResolution::default());
        let fallback = phase_skills_resolution(Some(&other_payload), project_root, &ctx, "review");
        assert_eq!(fallback.requested, vec!["local-skill"]);
        assert_eq!(fallback.missing, vec!["local-skill"]);
    }

    #[test]
    fn resolve_workflow_skills_payload_compiles_project_yaml() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_str().expect("utf-8 tempdir");
        let animus = temp.path().join(".animus");
        let skills_dir = animus.join("config").join("skill_definitions");
        std::fs::create_dir_all(&skills_dir).expect("create skills dir");
        std::fs::write(
            skills_dir.join("deep-search.yaml"),
            "name: deep-search\nprompt:\n  prefix: deep-search prefix\n",
        )
        .expect("write skill");
        std::fs::write(
            animus.join("workflows.yaml"),
            "phases:\n  research:\n    mode: agent\n    skills:\n      - deep-search\n      - ghost-skill\nworkflows:\n  - id: research-only\n    name: Research Only\n    phases:\n      - research\n",
        )
        .expect("write workflows.yaml");

        // v0.6: config is plugin-sourced; register the compiled project YAML as
        // the base in place of an installed config_source plugin for this load.
        let _seam = orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
            std::path::Path::new(project_root),
        );

        let payload = resolve_workflow_skills_payload(project_root);
        let research = payload.phases.get("research").expect("research phase resolved");
        assert!(research.requested.contains(&"deep-search".to_string()));
        assert_eq!(research.resolved.len(), 1);
        assert_eq!(research.resolved[0].definition.name, "deep-search");
        assert!(matches!(research.resolved[0].source, orchestrator_config::skill_scoping::SkillSourceOrigin::Project));
        assert_eq!(research.missing, vec!["ghost-skill"]);
    }

    #[test]
    fn metadata_population_records_requested_resolved_applied() {
        let resolution = PhaseSkillsResolution {
            requested: vec!["claude-only".to_string(), "everywhere".to_string(), "ghost".to_string()],
            resolved: vec![
                resolved("claude-only", "activation:\n  tools: [claude]\nprompt:\n  prefix: claude prefix\n"),
                resolved("everywhere", "prompt:\n  suffix: everywhere suffix\n"),
            ],
            missing: vec!["ghost".to_string()],
        };
        let applied = apply_phase_skills(&resolution, "codex", None);

        let mut metadata = PhaseExecutionMetadata {
            phase_id: "review".to_string(),
            phase_mode: "agent".to_string(),
            phase_definition_hash: String::new(),
            agent_runtime_config_hash: String::new(),
            agent_runtime_schema: String::new(),
            agent_runtime_version: 0,
            agent_runtime_source: String::new(),
            agent_id: None,
            agent_profile_hash: None,
            selected_tool: Some("codex".to_string()),
            selected_model: None,
            effective_capabilities: Default::default(),
            requested_skills: Vec::new(),
            resolved_skills: Vec::new(),
            applied_skills: Vec::new(),
            skill_application: None,
        };
        populate_phase_skills_metadata(&mut metadata, &resolution, &applied);

        assert_eq!(metadata.requested_skills, vec!["claude-only", "everywhere", "ghost"]);
        assert_eq!(metadata.resolved_skills.len(), 2);
        assert_eq!(metadata.applied_skills.len(), 1);
        assert_eq!(metadata.applied_skills[0].definition.name, "everywhere");
        let application = metadata.skill_application.as_ref().expect("non-empty application recorded");
        assert_eq!(application.prompt_suffixes, vec!["everywhere suffix"]);
    }

    #[test]
    fn deserialized_payload_drives_prompt_injection() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_str().expect("utf-8 tempdir");

        let mut payload = WorkflowSkillsPayload::default();
        payload.phases.insert(
            "implementation".to_string(),
            PhaseSkillsResolution {
                requested: vec!["wire-skill".to_string()],
                resolved: vec![resolved(
                    "wire-skill",
                    "prompt:\n  system: WIRE SYSTEM FRAGMENT\n  prefix: WIRE PREFIX\n  suffix: WIRE SUFFIX\n  directives:\n    - WIRE DIRECTIVE\n",
                )],
                missing: Vec::new(),
            },
        );
        let json = serde_json::to_string(&payload).expect("serialize payload");
        let payload: WorkflowSkillsPayload = serde_json::from_str(&json).expect("deserialize payload");

        let ctx = make_ctx(|_, _| {});
        let resolution = phase_skills_resolution(Some(&payload), project_root, &ctx, "implementation");
        let applied = apply_phase_skills(&resolution, "claude", None);

        let rendered = crate::phase_prompt::render_phase_prompt_with_ctx_overrides(
            &ctx,
            &crate::phase_prompt::PhaseRenderParams {
                project_root,
                execution_cwd: project_root,
                workflow_id: "wf-test",
                subject_id: "task:TEST-1",
                subject_title: "Test task",
                subject_description: "Test description",
                phase_id: "implementation",
            },
            crate::phase_prompt::PhasePromptInputs::default(),
            None,
            applied.skill_result(),
        );

        assert!(rendered.final_prompt.contains("WIRE PREFIX"), "prefix missing from final prompt");
        assert!(rendered.final_prompt.contains("WIRE SUFFIX"), "suffix missing from final prompt");
        assert!(rendered.final_prompt.contains("WIRE DIRECTIVE"), "directive missing from final prompt");
        assert!(
            rendered.system_prompt.as_deref().unwrap_or_default().contains("WIRE SYSTEM FRAGMENT"),
            "system fragment missing from system prompt"
        );
    }

    #[test]
    fn skill_mcp_servers_join_the_contract_like_phase_bindings() {
        let _guard = crate::test_env::scoped_state_serializer();
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_str().expect("utf-8 tempdir");

        let ctx = make_ctx(|workflow, _| {
            let definition: orchestrator_config::McpServerDefinition =
                serde_json::from_value(serde_json::json!({ "command": "context7-mcp" }))
                    .expect("mcp server definition should deserialize");
            workflow.mcp_servers.insert("context7".to_string(), definition);
        });
        let applied = AppliedPhaseSkills {
            application: SkillApplicationResult {
                mcp_servers: vec!["context7".to_string(), "undefined-server".to_string()],
                ..Default::default()
            },
            applied: Vec::new(),
        };

        let mut contract = serde_json::json!({ "mcp": {} });
        inject_skill_mcp_servers(&mut contract, project_root, &ctx, "implementation", &applied);

        assert!(
            contract.pointer("/mcp/additional_servers/context7").is_some(),
            "skill-declared MCP server must join the contract: {contract}"
        );
        assert!(
            contract.pointer("/mcp/additional_servers/undefined-server").is_none(),
            "undefined server must be skipped, not injected"
        );
    }

    #[test]
    fn skill_capability_overrides_overlay_phase_capabilities() {
        let base = protocol::PhaseCapabilities { writes_files: true, requires_commit: true, ..Default::default() };
        let overrides = BTreeMap::from([
            ("writes_files".to_string(), false),
            ("is_research".to_string(), true),
            ("unknown_key".to_string(), true),
        ]);
        let merged = apply_skill_capability_overrides(base, &overrides);
        assert!(!merged.writes_files, "skill override must flip writes_files off");
        assert!(merged.is_research, "skill override must flip is_research on");
        assert!(merged.requires_commit, "untouched capability must survive the overlay");
    }

    #[test]
    fn env_payload_loader_ignores_malformed_payloads() {
        let _guard = crate::test_env::scoped_state_serializer();
        std::env::set_var(ANIMUS_PHASE_SKILLS_ENV, "not json");
        assert!(load_workflow_skills_payload_from_env().is_none());
        std::env::set_var(ANIMUS_PHASE_SKILLS_ENV, "{\"phases\":{}}");
        assert!(load_workflow_skills_payload_from_env().is_some());
        std::env::remove_var(ANIMUS_PHASE_SKILLS_ENV);
        assert!(load_workflow_skills_payload_from_env().is_none());
    }
}
