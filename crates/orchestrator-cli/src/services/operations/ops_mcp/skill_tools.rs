use std::collections::BTreeMap;
use std::path::Path;

use orchestrator_config::skill_definition::{
    skill_definition_warnings, SkillActivation, SkillCategory, SkillDefinition, SkillModelPreference, SkillPrompt,
};
use orchestrator_config::skill_resolution::{list_available_skills, resolve_skill};
use orchestrator_config::skill_scoping::{
    load_skill_sources, parse_skill_category_label, validate_skill_slug, write_skill_yaml, AgentHostScope,
    SkillSourceOrigin, SkillWriteOutcome, SkillWriteScope,
};
use orchestrator_config::AgentToolPolicy;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SkillListInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Optional source filter. Accepts: "installed", "user", "project",
    /// "agent_host" (matches any agent-host source), or an agent host id
    /// like "claude-code", "codex", "cursor". "builtin" is kept as a
    /// backward-compatible filter but current builds do not emit builtin rows.
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SkillGetInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Skill name (e.g. "code-review", "rust-architect").
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SkillSearchInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Substring match on name/description/tags. Case-insensitive.
    query: String,
    /// Optional source filter (same vocabulary as `animus.skill.list`).
    #[serde(default)]
    source: Option<String>,
    /// Optional limit on returned matches (default 50).
    #[serde(default)]
    limit: Option<usize>,
}

/// Tool/model activation gates for an authored skill. Maps directly onto
/// [`SkillActivation`]. When both lists are empty the skill applies
/// unconditionally (preview mode).
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SkillActivationInput {
    /// Tool ids the skill activates for (e.g. "claude", "codex", "gemini").
    /// Empty = any tool.
    #[serde(default)]
    tools: Vec<String>,
    /// Model ids the skill activates for. Empty = any model.
    #[serde(default)]
    models: Vec<String>,
}

/// Tool allow/deny policy for an authored skill. Maps onto [`AgentToolPolicy`].
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SkillToolPolicyInput {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SkillCreateInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Skill slug: lowercase ASCII letters/digits plus `-`/`_`, no path
    /// separators. Becomes the file name `<skill_definitions dir>/<name>.yaml`.
    name: String,
    /// Authoring scope: "project" (default) writes to
    /// `.animus/config/skill_definitions/`, "user" writes to
    /// `~/.animus/config/skill_definitions/`. Project shadows user on name
    /// collision.
    #[serde(default)]
    scope: Option<String>,
    /// Human-readable description shown in `list`/`search`.
    description: String,
    /// The skill's instruction body. Stored as `prompt.system`.
    prompt: String,
    /// Optional discovery tags (case-insensitive substring matched by search).
    #[serde(default)]
    tags: Vec<String>,
    /// Optional tool allow/deny policy. Trusted because this is a
    /// project- or user-scoped skill authored locally.
    #[serde(default)]
    tool_policy: Option<SkillToolPolicyInput>,
    /// Optional preferred model id (e.g. "claude-sonnet-4-6").
    #[serde(default)]
    model: Option<String>,
    /// Optional MCP server ids the skill should attach.
    #[serde(default)]
    mcp_servers: Vec<String>,
    /// Optional category: implementation, testing, review, research,
    /// documentation, operations, planning.
    #[serde(default)]
    category: Option<String>,
    /// Optional activation gates (tool/model).
    #[serde(default)]
    activation: Option<SkillActivationInput>,
    /// Optional capability overrides (e.g. {"writes_files": true}).
    #[serde(default)]
    capabilities: Option<BTreeMap<String, bool>>,
    /// Refuse to overwrite an existing project skill unless true.
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SkillUpdateInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Slug of the existing project- or user-scoped skill to patch.
    name: String,
    /// Scope to patch at: "project" or "user". Optional when the skill exists
    /// at only one of the two scopes; REQUIRED when it exists at both.
    #[serde(default)]
    scope: Option<String>,
    /// New description (replaces existing when supplied).
    #[serde(default)]
    description: Option<String>,
    /// New instruction body (replaces `prompt.system` when supplied).
    #[serde(default)]
    prompt: Option<String>,
    /// New tags list (replaces existing when supplied).
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// New tool policy (replaces existing when supplied).
    #[serde(default)]
    tool_policy: Option<SkillToolPolicyInput>,
    /// New preferred model (replaces existing when supplied).
    #[serde(default)]
    model: Option<String>,
    /// New MCP server list (replaces existing when supplied).
    #[serde(default)]
    mcp_servers: Option<Vec<String>>,
    /// New category (replaces existing when supplied).
    #[serde(default)]
    category: Option<String>,
    /// New capability overrides (replaces existing when supplied).
    #[serde(default)]
    capabilities: Option<BTreeMap<String, bool>>,
}

const DEFAULT_SKILL_SEARCH_LIMIT: usize = 50;

fn parse_skill_category(raw: &str) -> Result<SkillCategory, McpError> {
    parse_skill_category_label(raw).map_err(|err| McpError::invalid_params(err.to_string(), None))
}

/// Parse the optional authoring `scope` param: "project" or "user".
/// Returns `None` when the caller did not specify a scope.
fn parse_skill_write_scope(raw: Option<&str>) -> Result<Option<SkillWriteScope>, McpError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "project" => Ok(Some(SkillWriteScope::Project)),
        "user" => Ok(Some(SkillWriteScope::User)),
        other => {
            Err(McpError::invalid_params(format!("unknown scope '{other}': expected \"project\" or \"user\""), None))
        }
    }
}

const PROJECT_SHADOWS_USER_NOTE: &str =
    "project-scoped skills shadow user-scoped skills with the same name during resolution";

impl From<SkillActivationInput> for SkillActivation {
    fn from(input: SkillActivationInput) -> Self {
        SkillActivation { tools: input.tools, models: input.models }
    }
}

impl From<SkillToolPolicyInput> for AgentToolPolicy {
    fn from(input: SkillToolPolicyInput) -> Self {
        AgentToolPolicy { allow: input.allow, deny: input.deny }
    }
}

fn skill_write_outcome_str(outcome: SkillWriteOutcome) -> &'static str {
    match outcome {
        SkillWriteOutcome::Created => "created",
        SkillWriteOutcome::Updated => "updated",
    }
}

fn normalize_source_filter(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_ascii_lowercase()).filter(|value| !value.is_empty())
}

fn matches_source_filter(origin: &SkillSourceOrigin, filter: &str) -> bool {
    match origin {
        SkillSourceOrigin::Builtin => matches!(filter, "builtin" | "built-in"),
        SkillSourceOrigin::Installed { .. } => filter == "installed",
        SkillSourceOrigin::User => filter == "user",
        SkillSourceOrigin::Project => filter == "project",
        SkillSourceOrigin::AgentHost { host, .. } => {
            filter == "agent_host" || filter == "agent-host" || filter == host.to_ascii_lowercase()
        }
    }
}

fn source_tag(origin: &SkillSourceOrigin) -> &'static str {
    match origin {
        SkillSourceOrigin::Builtin => "builtin",
        SkillSourceOrigin::Installed { .. } => "installed",
        SkillSourceOrigin::User => "user",
        SkillSourceOrigin::Project => "project",
        SkillSourceOrigin::AgentHost { .. } => "agent_host",
    }
}

fn agent_host_scope_str(scope: AgentHostScope) -> &'static str {
    match scope {
        AgentHostScope::Project => "project",
        AgentHostScope::Global => "global",
    }
}

fn source_detail(origin: &SkillSourceOrigin) -> Value {
    match origin {
        SkillSourceOrigin::Builtin => json!({}),
        SkillSourceOrigin::Installed { registry, source, version, integrity, artifact } => json!({
            "registry": registry,
            "source": source,
            "version": version,
            "integrity": integrity,
            "artifact": artifact,
        }),
        SkillSourceOrigin::User => json!({}),
        SkillSourceOrigin::Project => json!({}),
        SkillSourceOrigin::AgentHost { host, scope } => json!({
            "host": host,
            "scope": agent_host_scope_str(*scope),
            // The trust boundary stripped tool_policy / mcp_servers / env / extra_args
            // / capabilities / adapters / codex_config_overrides at parse time.
            // Surface that explicitly so callers know agent-host skills are
            // prompt-text-only.
            "structural_fields_stripped": true,
            "trust_tier": "prompt_text_only",
        }),
    }
}

fn category_label(definition: &SkillDefinition) -> Option<String> {
    definition
        .category
        .as_ref()
        .and_then(|category| serde_json::to_value(category).ok())
        .and_then(|value| value.as_str().map(|s| s.to_string()))
}

fn skill_summary(definition: &SkillDefinition, origin: &SkillSourceOrigin) -> Value {
    let mut payload = json!({
        "name": definition.name,
        "description": definition.description,
        "source": source_tag(origin),
        "source_detail": source_detail(origin),
        "tags": definition.tags,
    });
    if let Some(version) = definition.version.as_deref().filter(|value| !value.trim().is_empty()) {
        payload.as_object_mut().unwrap().insert("version".to_string(), json!(version));
    }
    if let Some(category) = category_label(definition) {
        payload.as_object_mut().unwrap().insert("category".to_string(), json!(category));
    }
    payload
}

/// Non-fatal definition warnings (inert `activation.tools` / `adapters`
/// entries) rendered as display strings. Empty when the definition is clean.
fn definition_warning_strings(definition: &SkillDefinition) -> Vec<String> {
    skill_definition_warnings(definition).iter().map(ToString::to_string).collect()
}

fn skill_full(definition: &SkillDefinition, origin: &SkillSourceOrigin) -> Value {
    let mut payload = json!({
        "definition": definition,
        "source": source_tag(origin),
        "source_detail": source_detail(origin),
    });
    let warnings = definition_warning_strings(definition);
    if !warnings.is_empty() {
        payload.as_object_mut().unwrap().insert("warnings".to_string(), json!(warnings));
    }
    if let SkillSourceOrigin::AgentHost { .. } = origin {
        payload
            .as_object_mut()
            .unwrap()
            .insert("notice".to_string(), json!("agent-host skill: structural fields (tool_policy, mcp_servers, env, extra_args, capabilities, adapters, codex_config_overrides) were stripped at parse time. Only prompt text and prompt directives are trusted."));
    }
    payload
}

fn substring_match(haystack: &str, needle_lc: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle_lc)
}

fn skill_matches_query(definition: &SkillDefinition, query_lc: &str) -> bool {
    if substring_match(&definition.name, query_lc) {
        return true;
    }
    if substring_match(&definition.description, query_lc) {
        return true;
    }
    definition.tags.iter().any(|tag| substring_match(tag, query_lc))
}

fn collect_skills(project_root: &str, source_filter: Option<&str>) -> Result<Vec<Value>, McpError> {
    let sources = load_skill_sources(Path::new(project_root), None)
        .map_err(|err| McpError::internal_error(format!("failed to load skill sources: {err}"), None))?;
    let available = list_available_skills(&sources);
    let rows = available
        .into_iter()
        .filter(|resolved| match source_filter {
            Some(filter) => matches_source_filter(&resolved.source, filter),
            None => true,
        })
        .map(|resolved| skill_summary(&resolved.definition, &resolved.source))
        .collect();
    Ok(rows)
}

impl AoMcpServer {
    fn skill_project_root(&self, override_root: Option<String>) -> String {
        normalize_non_empty(override_root).unwrap_or_else(|| self.default_project_root.clone())
    }
}

#[tool_router(router = skill_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.skill.list",
        description = "List Animus skills discoverable from this project across every source: installed packs, registry-tracked installs, user-scoped (~/.animus/skills, ~/.animus/config/skill_definitions), project-scoped (.animus/skills, .animus/config/skill_definitions), and agent-host probes (~/.claude/skills/, ~/.codex/skills/, etc.). Optional `source` filter accepts \"installed\", \"user\", \"project\", \"agent_host\", or a host id like \"claude-code\". \"builtin\" remains accepted as a backward-compatible filter, but current builds do not emit builtin rows. Each result carries provenance via `source` + `source_detail` so callers can reason about trust tier.",
        input_schema = ao_schema_for_type::<SkillListInput>()
    )]
    async fn ao_skill_list(&self, params: Parameters<SkillListInput>) -> Result<CallToolResult, McpError> {
        let SkillListInput { project_root, source } = params.0;
        let project_root = self.skill_project_root(project_root);
        let source_filter = normalize_source_filter(source);
        let skills = collect_skills(&project_root, source_filter.as_deref())?;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.list",
            "result": {
                "count": skills.len(),
                "project_root": project_root,
                "source_filter": source_filter,
                "skills": skills,
            }
        })))
    }

    #[tool(
        name = "animus.skill.get",
        description = "Resolve a skill by name and return its full SkillDefinition plus source provenance. Resolution honors the priority chain: project > user > installed/pack > agent-host. Returns the parsed definition (prompt, tool_policy, model, mcp_servers, capabilities, adapters, tags, etc.) under `definition`. For agent-host sources, structural fields are stripped at parse time and a `notice` field warns that only prompt text is trusted. When the definition contains likely-inert declarations (e.g. an `activation.tools` or `adapters` entry that is not a built-in tool id — claude, codex, gemini, opencode, oai-runner — and no custom CLI tool is configured with that id, it never matches), a non-fatal `warnings` array is included.",
        input_schema = ao_schema_for_type::<SkillGetInput>()
    )]
    async fn ao_skill_get(&self, params: Parameters<SkillGetInput>) -> Result<CallToolResult, McpError> {
        let SkillGetInput { project_root, name } = params.0;
        let project_root = self.skill_project_root(project_root);
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        let sources = load_skill_sources(Path::new(&project_root), None)
            .map_err(|err| McpError::internal_error(format!("failed to load skill sources: {err}"), None))?;
        let resolved = resolve_skill(&trimmed, &sources)
            .map_err(|err| McpError::invalid_params(format!("skill '{}' not found: {}", trimmed, err), None))?;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.get",
            "result": skill_full(&resolved.definition, &resolved.source),
        })))
    }

    #[tool(
        name = "animus.skill.search",
        description = "Case-insensitive substring search over discoverable skills. Matches the query against skill `name`, `description`, and `tags`. Supports the same `source` filter as `animus.skill.list` and a `limit` (default 50). Returns the same row shape as `animus.skill.list` for matched skills.",
        input_schema = ao_schema_for_type::<SkillSearchInput>()
    )]
    async fn ao_skill_search(&self, params: Parameters<SkillSearchInput>) -> Result<CallToolResult, McpError> {
        let SkillSearchInput { project_root, query, source, limit } = params.0;
        let project_root = self.skill_project_root(project_root);
        let query_trimmed = query.trim();
        if query_trimmed.is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }
        let query_lc = query_trimmed.to_ascii_lowercase();
        let source_filter = normalize_source_filter(source);
        let limit = limit.unwrap_or(DEFAULT_SKILL_SEARCH_LIMIT).max(1);

        let sources = load_skill_sources(Path::new(&project_root), None)
            .map_err(|err| McpError::internal_error(format!("failed to load skill sources: {err}"), None))?;
        let available = list_available_skills(&sources);

        let mut matches: Vec<Value> = Vec::new();
        let mut truncated = false;
        for resolved in available {
            if let Some(filter) = source_filter.as_deref() {
                if !matches_source_filter(&resolved.source, filter) {
                    continue;
                }
            }
            if !skill_matches_query(&resolved.definition, &query_lc) {
                continue;
            }
            if matches.len() >= limit {
                truncated = true;
                break;
            }
            matches.push(skill_summary(&resolved.definition, &resolved.source));
        }

        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.search",
            "result": {
                "query": query_trimmed,
                "count": matches.len(),
                "limit": limit,
                "truncated": truncated,
                "source_filter": source_filter,
                "skills": matches,
            }
        })))
    }

    #[tool(
        name = "animus.skill.create",
        description = "Author an Animus skill at project or user scope. `scope: \"project\"` (default) writes a full-fidelity SkillDefinition as YAML to <project_root>/.animus/config/skill_definitions/<name>.yaml (the tier resolution reads at highest priority); `scope: \"user\"` writes to ~/.animus/config/skill_definitions/<name>.yaml so the skill is available across every project. Project shadows user on name collision. `name` must be a slug (lowercase ASCII letters/digits plus '-'/'_', no path separators); `description` and `prompt` are required. Optional: tags, tool_policy {allow,deny}, model, mcp_servers, category, activation {tools,models}, capabilities. Refuses to overwrite an existing skill at the same scope unless `overwrite` is true. The written file is re-parsed to guarantee it round-trips, so a malformed skill is never left on disk. Structural fields (tool_policy, mcp_servers) are trusted because the skill is authored locally. The new skill is immediately discoverable via animus.skill.list / animus.skill.search.",
        input_schema = ao_schema_for_type::<SkillCreateInput>()
    )]
    async fn ao_skill_create(&self, params: Parameters<SkillCreateInput>) -> Result<CallToolResult, McpError> {
        let SkillCreateInput {
            project_root,
            name,
            scope,
            description,
            prompt,
            tags,
            tool_policy,
            model,
            mcp_servers,
            category,
            activation,
            capabilities,
            overwrite,
        } = params.0;
        let project_root = self.skill_project_root(project_root);
        let scope = parse_skill_write_scope(scope.as_deref())?.unwrap_or(SkillWriteScope::Project);

        let name = validate_skill_slug(&name).map_err(|err| McpError::invalid_params(err.to_string(), None))?;
        let description = description.trim().to_string();
        if description.is_empty() {
            return Err(McpError::invalid_params("description must not be empty", None));
        }
        let prompt_body = prompt.trim().to_string();
        if prompt_body.is_empty() {
            return Err(McpError::invalid_params("prompt must not be empty", None));
        }

        let category = match category.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            Some(raw) => Some(parse_skill_category(raw)?),
            None => None,
        };

        let definition = SkillDefinition {
            name: name.clone(),
            version: None,
            description,
            category,
            activation: activation.map(SkillActivation::from).unwrap_or_default(),
            prompt: SkillPrompt { system: Some(prompt_body), ..SkillPrompt::default() },
            tool_policy: tool_policy.map(AgentToolPolicy::from),
            model: model
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|preferred| SkillModelPreference { preferred: Some(preferred), fallback: None })
                .unwrap_or_default(),
            mcp_servers: mcp_servers.into_iter().filter(|value| !value.trim().is_empty()).collect(),
            timeout_secs: None,
            capabilities: capabilities.unwrap_or_default(),
            extra_args: Vec::new(),
            env: BTreeMap::new(),
            codex_config_overrides: Vec::new(),
            adapters: BTreeMap::new(),
            tags: tags.into_iter().filter(|value| !value.trim().is_empty()).collect(),
        };

        let (path, outcome) = write_skill_yaml(Path::new(&project_root), scope, &definition, overwrite)
            .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        let mut result = json!({
            "name": name,
            "path": path.to_string_lossy(),
            "source": scope.to_string(),
            "scope": scope.to_string(),
            "outcome": skill_write_outcome_str(outcome),
            "note": PROJECT_SHADOWS_USER_NOTE,
        });
        let warnings = definition_warning_strings(&definition);
        if !warnings.is_empty() {
            result.as_object_mut().unwrap().insert("warnings".to_string(), json!(warnings));
        }
        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.create",
            "result": result,
        })))
    }

    #[tool(
        name = "animus.skill.update",
        description = "Patch an existing project- or user-scoped skill (.animus/config/skill_definitions/<name>.yaml or ~/.animus/config/skill_definitions/<name>.yaml). Only the supplied fields change; every other field is preserved from the existing definition. Patchable: description, prompt, tags, tool_policy {allow,deny}, model, mcp_servers, category, capabilities. Scope rule: when `scope` is omitted the skill is patched at the single scope where it exists; if it exists at BOTH project and user scope the call fails and you must pass `scope: \"project\"` or `scope: \"user\"` explicitly. Fails if the named skill only resolves to an installed/pack/agent-host source (use animus.skill.create for new skills; those sources are not editable via MCP). The rewritten file is re-parsed to guarantee it round-trips.",
        input_schema = ao_schema_for_type::<SkillUpdateInput>()
    )]
    async fn ao_skill_update(&self, params: Parameters<SkillUpdateInput>) -> Result<CallToolResult, McpError> {
        let SkillUpdateInput {
            project_root,
            name,
            scope,
            description,
            prompt,
            tags,
            tool_policy,
            model,
            mcp_servers,
            category,
            capabilities,
        } = params.0;
        let project_root = self.skill_project_root(project_root);
        let requested_scope = parse_skill_write_scope(scope.as_deref())?;
        let name = validate_skill_slug(&name).map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        let sources = load_skill_sources(Path::new(&project_root), None)
            .map_err(|err| McpError::internal_error(format!("failed to load skill sources: {err}"), None))?;
        let definition_at = |origin: SkillSourceOrigin| {
            sources.iter().filter(|source| source.origin == origin).find_map(|source| source.skills.get(&name)).cloned()
        };
        let project_definition = definition_at(SkillSourceOrigin::Project);
        let user_definition = definition_at(SkillSourceOrigin::User);

        let (scope, definition) = match (requested_scope, project_definition, user_definition) {
            (Some(SkillWriteScope::Project), Some(definition), _) => (SkillWriteScope::Project, definition),
            (Some(SkillWriteScope::User), _, Some(definition)) => (SkillWriteScope::User, definition),
            (Some(requested), _, _) => {
                return Err(McpError::invalid_params(format!("skill '{name}' not found at {requested} scope"), None));
            }
            (None, Some(definition), None) => (SkillWriteScope::Project, definition),
            (None, None, Some(definition)) => (SkillWriteScope::User, definition),
            (None, Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    format!(
                        "skill '{name}' exists at both project and user scope; pass scope=\"project\" or scope=\"user\" to disambiguate"
                    ),
                    None,
                ));
            }
            (None, None, None) => {
                let message = match resolve_skill(&name, &sources) {
                    Ok(resolved) => format!(
                        "skill '{}' resolves to a {} source; only project-scoped or user-scoped skills can be updated via MCP",
                        name,
                        source_tag(&resolved.source)
                    ),
                    Err(err) => format!("skill '{name}' not found: {err}"),
                };
                return Err(McpError::invalid_params(message, None));
            }
        };

        let mut definition = definition;

        if let Some(description) = description {
            let trimmed = description.trim().to_string();
            if trimmed.is_empty() {
                return Err(McpError::invalid_params("description must not be empty", None));
            }
            definition.description = trimmed;
        }
        if let Some(prompt) = prompt {
            let trimmed = prompt.trim().to_string();
            if trimmed.is_empty() {
                return Err(McpError::invalid_params("prompt must not be empty", None));
            }
            definition.prompt.system = Some(trimmed);
        }
        if let Some(tags) = tags {
            definition.tags = tags.into_iter().filter(|value| !value.trim().is_empty()).collect();
        }
        if let Some(tool_policy) = tool_policy {
            definition.tool_policy = Some(AgentToolPolicy::from(tool_policy));
        }
        if let Some(model) = model {
            let trimmed = model.trim().to_string();
            definition.model = if trimmed.is_empty() {
                SkillModelPreference::default()
            } else {
                SkillModelPreference { preferred: Some(trimmed), fallback: None }
            };
        }
        if let Some(mcp_servers) = mcp_servers {
            definition.mcp_servers = mcp_servers.into_iter().filter(|value| !value.trim().is_empty()).collect();
        }
        if let Some(category) = category {
            let trimmed = category.trim();
            definition.category = if trimmed.is_empty() { None } else { Some(parse_skill_category(trimmed)?) };
        }
        if let Some(capabilities) = capabilities {
            definition.capabilities = capabilities;
        }

        let (path, outcome) = write_skill_yaml(Path::new(&project_root), scope, &definition, true)
            .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        let mut result = json!({
            "name": name,
            "path": path.to_string_lossy(),
            "source": scope.to_string(),
            "scope": scope.to_string(),
            "outcome": skill_write_outcome_str(outcome),
            "note": PROJECT_SHADOWS_USER_NOTE,
        });
        let warnings = definition_warning_strings(&definition);
        if !warnings.is_empty() {
            result.as_object_mut().unwrap().insert("warnings".to_string(), json!(warnings));
        }
        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.update",
            "result": result,
        })))
    }
}

#[cfg(test)]
mod skill_tool_tests {
    // Tests that pin HOME hold `crate::test_env_lock()` across tool `.await`s
    // on purpose: each test runs on its own single-threaded tokio runtime, so
    // the std mutex cannot deadlock, and the env mutation must stay serialized
    // for the whole test body.
    #![allow(clippy::await_holding_lock)]

    use super::super::new_ao_mcp_server;
    use super::*;
    use protocol::test_utils::EnvVarGuard;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;

    fn structured(result: &rmcp::model::CallToolResult) -> Value {
        result.structured_content.clone().expect("expected structured_content on tool result")
    }

    fn data(result: &rmcp::model::CallToolResult) -> Value {
        let payload = structured(result);
        payload.get("result").cloned().expect("structured result should include `result`")
    }

    /// Set HOME to a fresh tempdir so we don't pick up the contributor's
    /// real ~/.claude/skills, ~/.codex/skills, or installed pack registry.
    fn isolated_home() -> (TempDir, EnvVarGuard) {
        let home = TempDir::new().expect("create HOME tempdir");
        let guard = EnvVarGuard::set("HOME", Some(home.path().to_str().expect("home path utf-8")));
        (home, guard)
    }

    fn project_root_for(tmp: &TempDir) -> String {
        tmp.path().to_string_lossy().to_string()
    }

    fn write_agent_host_claude_skill(home: &TempDir, name: &str, body: &str) {
        let dir = home.path().join(".claude").join("skills").join(name);
        fs::create_dir_all(&dir).expect("create claude skill dir");
        fs::write(dir.join("SKILL.md"), body).expect("write SKILL.md");
    }

    #[tokio::test]
    async fn skill_router_registers_all_tools() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));
        let names: Vec<String> = server.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        assert!(names.contains(&"animus.skill.list".to_string()), "router missing animus.skill.list");
        assert!(names.contains(&"animus.skill.get".to_string()), "router missing animus.skill.get");
        assert!(names.contains(&"animus.skill.search".to_string()), "router missing animus.skill.search");
        assert!(names.contains(&"animus.skill.create".to_string()), "router missing animus.skill.create");
        assert!(names.contains(&"animus.skill.update".to_string()), "router missing animus.skill.update");
        assert!(server.tool_router.has_route("animus.skill.list"));
    }

    fn create_input(name: &str, description: &str, prompt: &str) -> SkillCreateInput {
        SkillCreateInput {
            project_root: None,
            name: name.to_string(),
            scope: None,
            description: description.to_string(),
            prompt: prompt.to_string(),
            tags: Vec::new(),
            tool_policy: None,
            model: None,
            mcp_servers: Vec::new(),
            category: None,
            activation: None,
            capabilities: None,
            overwrite: false,
        }
    }

    fn update_input(name: &str) -> SkillUpdateInput {
        SkillUpdateInput {
            project_root: None,
            name: name.to_string(),
            scope: None,
            description: None,
            prompt: None,
            tags: None,
            tool_policy: None,
            model: None,
            mcp_servers: None,
            category: None,
            capabilities: None,
        }
    }

    #[tokio::test]
    async fn skill_create_writes_yaml_and_round_trips() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut input = create_input("my-reviewer", "Reviews PRs", "You review pull requests.");
        input.tags = vec!["review".to_string(), "quality".to_string()];
        input.tool_policy =
            Some(SkillToolPolicyInput { allow: vec!["Read".to_string()], deny: vec!["Write".to_string()] });
        input.model = Some("claude-sonnet-4-6".to_string());
        input.mcp_servers = vec!["animus".to_string()];
        input.category = Some("review".to_string());
        input.capabilities = Some(BTreeMap::from([("is_review".to_string(), true)]));

        let result = server.ao_skill_create(Parameters(input)).await.expect("create skill");
        let payload = data(&result);
        assert_eq!(payload.get("name").and_then(Value::as_str), Some("my-reviewer"));
        assert_eq!(payload.get("source").and_then(Value::as_str), Some("project"));
        assert_eq!(payload.get("outcome").and_then(Value::as_str), Some("created"));

        let path = payload.get("path").and_then(Value::as_str).expect("path");
        assert!(path.ends_with(".animus/config/skill_definitions/my-reviewer.yaml"), "unexpected path {path}");
        assert!(std::path::Path::new(path).exists(), "skill file should exist on disk");

        let sources = load_skill_sources(project.path(), None).expect("load sources");
        let resolved = resolve_skill("my-reviewer", &sources).expect("resolve created skill");
        assert!(matches!(resolved.source, SkillSourceOrigin::Project));
        assert_eq!(resolved.definition.description, "Reviews PRs");
        assert_eq!(resolved.definition.prompt.system.as_deref(), Some("You review pull requests."));
        assert_eq!(resolved.definition.tags, vec!["review", "quality"]);
        assert_eq!(resolved.definition.tool_policy.as_ref().map(|p| p.allow.clone()), Some(vec!["Read".to_string()]));
        assert_eq!(resolved.definition.model.preferred.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(resolved.definition.mcp_servers, vec!["animus"]);
        assert_eq!(resolved.definition.capabilities.get("is_review"), Some(&true));
    }

    #[tokio::test]
    async fn skill_create_then_visible_in_list_and_search() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut input = create_input("discoverable-skill", "A findable skill", "Body.");
        input.tags = vec!["findme".to_string()];
        server.ao_skill_create(Parameters(input)).await.expect("create skill");

        let listed = server
            .ao_skill_list(Parameters(SkillListInput { project_root: None, source: Some("project".to_string()) }))
            .await
            .expect("list project");
        let list_payload = data(&listed);
        let listed_skills = list_payload.pointer("/skills").and_then(Value::as_array).expect("skills");
        assert!(
            listed_skills.iter().any(|s| s.get("name").and_then(Value::as_str) == Some("discoverable-skill")),
            "created skill should appear in animus.skill.list"
        );

        let searched = server
            .ao_skill_search(Parameters(SkillSearchInput {
                project_root: None,
                query: "FINDME".to_string(),
                source: None,
                limit: None,
            }))
            .await
            .expect("search");
        let search_payload = data(&searched);
        let matched = search_payload.pointer("/skills").and_then(Value::as_array).expect("skills");
        assert!(
            matched.iter().any(|s| s.get("name").and_then(Value::as_str) == Some("discoverable-skill")),
            "created skill should be found by case-insensitive tag search"
        );
    }

    #[tokio::test]
    async fn skill_create_refuses_overwrite_then_allows_with_flag() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        server.ao_skill_create(Parameters(create_input("dup", "first", "first body"))).await.expect("first create");

        let err = server
            .ao_skill_create(Parameters(create_input("dup", "second", "second body")))
            .await
            .expect_err("second create without overwrite should fail");
        assert!(err.message.contains("already exists"), "error should mention existing skill: {}", err.message);

        let mut overwrite_input = create_input("dup", "second", "second body");
        overwrite_input.overwrite = true;
        let result = server.ao_skill_create(Parameters(overwrite_input)).await.expect("overwrite create");
        assert_eq!(data(&result).get("outcome").and_then(Value::as_str), Some("updated"));

        let sources = load_skill_sources(project.path(), None).expect("load sources");
        let resolved = resolve_skill("dup", &sources).expect("resolve");
        assert_eq!(resolved.definition.description, "second");
    }

    #[tokio::test]
    async fn skill_create_rejects_bad_names_and_empty_fields() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        for bad in ["../escape", "Has Space", "UPPER", "with/slash", "with\\back", "  "] {
            let result = server.ao_skill_create(Parameters(create_input(bad, "desc", "body"))).await;
            assert!(result.is_err(), "name {bad:?} should be rejected but succeeded");
        }

        let traversal = server
            .ao_skill_create(Parameters(create_input("../escape", "desc", "body")))
            .await
            .expect_err("path traversal name must be rejected");
        assert!(traversal.message.to_lowercase().contains("path") || traversal.message.contains(".."));
        assert!(!project.path().join("escape.yaml").exists());

        let empty_desc = server
            .ao_skill_create(Parameters(create_input("ok-name", "   ", "body")))
            .await
            .expect_err("empty description rejected");
        assert!(empty_desc.message.contains("description"));

        let empty_prompt = server
            .ao_skill_create(Parameters(create_input("ok-name", "desc", "   ")))
            .await
            .expect_err("empty prompt rejected");
        assert!(empty_prompt.message.contains("prompt"));
    }

    #[tokio::test]
    async fn skill_update_patches_only_supplied_fields() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut create = create_input("patchable", "original desc", "original body");
        create.tags = vec!["keep".to_string()];
        create.model = Some("claude-sonnet-4-6".to_string());
        server.ao_skill_create(Parameters(create)).await.expect("create");

        let mut patch = update_input("patchable");
        patch.description = Some("new desc".to_string());
        let result = server.ao_skill_update(Parameters(patch)).await.expect("update");
        assert_eq!(data(&result).get("outcome").and_then(Value::as_str), Some("updated"));

        let sources = load_skill_sources(project.path(), None).expect("load sources");
        let resolved = resolve_skill("patchable", &sources).expect("resolve");
        assert_eq!(resolved.definition.description, "new desc");
        assert_eq!(resolved.definition.prompt.system.as_deref(), Some("original body"));
        assert_eq!(resolved.definition.tags, vec!["keep"]);
        assert_eq!(resolved.definition.model.preferred.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[tokio::test]
    async fn skill_create_default_scope_is_project_and_user_scope_lands_in_home() {
        let _lock = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let (home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let default_result =
            server.ao_skill_create(Parameters(create_input("default-scope", "desc", "body"))).await.expect("create");
        let default_payload = data(&default_result);
        assert_eq!(default_payload.get("scope").and_then(Value::as_str), Some("project"));
        let default_path = default_payload.get("path").and_then(Value::as_str).expect("path");
        assert!(default_path.starts_with(project.path().to_str().unwrap()), "project scope writes under project root");

        let mut user_input = create_input("user-scope", "desc", "body");
        user_input.scope = Some("user".to_string());
        let user_result = server.ao_skill_create(Parameters(user_input)).await.expect("create user-scoped");
        let user_payload = data(&user_result);
        assert_eq!(user_payload.get("scope").and_then(Value::as_str), Some("user"));
        assert_eq!(user_payload.get("source").and_then(Value::as_str), Some("user"));
        assert!(
            user_payload.get("note").and_then(Value::as_str).is_some_and(|note| note.contains("shadow")),
            "user-scope create should remind about project-shadows-user"
        );
        let user_path = user_payload.get("path").and_then(Value::as_str).expect("path");
        let expected = home.path().join(".animus").join("config").join("skill_definitions").join("user-scope.yaml");
        assert_eq!(std::path::Path::new(user_path), expected, "user scope must write to the dir the loader reads");
        assert!(expected.exists());

        let sources = load_skill_sources(project.path(), None).expect("load sources");
        let resolved = resolve_skill("user-scope", &sources).expect("resolve user-scoped skill");
        assert!(matches!(resolved.source, SkillSourceOrigin::User));
    }

    #[tokio::test]
    async fn skill_create_rejects_unknown_scope() {
        let _lock = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut input = create_input("scoped", "desc", "body");
        input.scope = Some("global".to_string());
        let err = server.ao_skill_create(Parameters(input)).await.expect_err("unknown scope rejected");
        assert!(err.message.contains("unknown scope 'global'"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn skill_update_requires_explicit_scope_when_skill_exists_at_both() {
        let _lock = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        server
            .ao_skill_create(Parameters(create_input("both", "project desc", "project body")))
            .await
            .expect("create project");
        let mut user_input = create_input("both", "user desc", "user body");
        user_input.scope = Some("user".to_string());
        server.ao_skill_create(Parameters(user_input)).await.expect("create user");

        // Ambiguous: exists at both scopes, no scope supplied.
        let mut ambiguous = update_input("both");
        ambiguous.description = Some("patched".to_string());
        let err = server.ao_skill_update(Parameters(ambiguous)).await.expect_err("ambiguous update should fail");
        assert!(err.message.contains("both project and user scope"), "got: {}", err.message);

        // Explicit user scope patches the user file and leaves project intact.
        let mut user_patch = update_input("both");
        user_patch.scope = Some("user".to_string());
        user_patch.description = Some("patched user".to_string());
        let result = server.ao_skill_update(Parameters(user_patch)).await.expect("user-scoped update");
        let payload = data(&result);
        assert_eq!(payload.get("scope").and_then(Value::as_str), Some("user"));

        let sources = load_skill_sources(project.path(), None).expect("load sources");
        let resolved = resolve_skill("both", &sources).expect("resolve");
        assert!(matches!(resolved.source, SkillSourceOrigin::Project), "project still shadows user");
        assert_eq!(resolved.definition.description, "project desc");
        let user_source = sources.iter().find(|s| matches!(s.origin, SkillSourceOrigin::User)).expect("user source");
        assert_eq!(user_source.skills.get("both").map(|s| s.description.as_str()), Some("patched user"));
    }

    #[tokio::test]
    async fn skill_update_infers_single_scope_and_rejects_missing_scope_target() {
        let _lock = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut user_input = create_input("user-only", "user desc", "user body");
        user_input.scope = Some("user".to_string());
        server.ao_skill_create(Parameters(user_input)).await.expect("create user");

        // Scope omitted: exists only at user scope, so it is patched there.
        let mut patch = update_input("user-only");
        patch.description = Some("patched".to_string());
        let result = server.ao_skill_update(Parameters(patch)).await.expect("update infers user scope");
        assert_eq!(data(&result).get("scope").and_then(Value::as_str), Some("user"));

        // Explicit project scope: no project-tier definition exists.
        let mut wrong_scope = update_input("user-only");
        wrong_scope.scope = Some("project".to_string());
        wrong_scope.description = Some("nope".to_string());
        let err = server.ao_skill_update(Parameters(wrong_scope)).await.expect_err("missing at project scope");
        assert!(err.message.contains("not found at project scope"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn skill_update_rejects_unknown_skill() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_skill_update(Parameters(update_input("does-not-exist")))
            .await
            .expect_err("unknown skill update should fail");
        assert!(err.message.contains("does-not-exist"));
    }

    #[tokio::test]
    async fn skill_update_rejects_non_project_source() {
        let (home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        write_agent_host_claude_skill(
            &home,
            "host-skill",
            "---\nname: host-skill\ndescription: agent host skill\n---\nBody.\n",
        );
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut patch = update_input("host-skill");
        patch.description = Some("hijack".to_string());
        let err = server.ao_skill_update(Parameters(patch)).await.expect_err("agent-host skill not editable");
        assert!(err.message.contains("project-scoped"), "error should explain scope: {}", err.message);
    }

    #[tokio::test]
    async fn skill_list_filters_by_source_builtin() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let result = server
            .ao_skill_list(Parameters(SkillListInput { project_root: None, source: Some("builtin".to_string()) }))
            .await
            .expect("list builtin");
        let payload = data(&result);
        let skills = payload.pointer("/skills").and_then(Value::as_array).expect("skills");
        // Every returned row must carry source = builtin (zero rows is also
        // acceptable when the pack supplies these as `installed` instead).
        for skill in skills {
            assert_eq!(skill.get("source").and_then(Value::as_str), Some("builtin"));
        }
    }

    #[tokio::test]
    async fn skill_list_filters_by_agent_host() {
        let (home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        write_agent_host_claude_skill(
            &home,
            "external",
            "---\nname: external\ndescription: External Claude skill\n---\nBody.\n",
        );
        let server = new_ao_mcp_server(&project_root_for(&project));

        let result = server
            .ao_skill_list(Parameters(SkillListInput { project_root: None, source: Some("agent_host".to_string()) }))
            .await
            .expect("list agent_host");
        let payload = data(&result);
        let skills = payload.pointer("/skills").and_then(Value::as_array).expect("skills");
        assert!(
            skills.iter().any(|skill| skill.get("name").and_then(Value::as_str) == Some("external")),
            "agent-host filter should surface the SKILL.md we wrote under HOME/.claude/skills/"
        );
        let row = skills
            .iter()
            .find(|skill| skill.get("name").and_then(Value::as_str) == Some("external"))
            .expect("external skill row");
        assert_eq!(row.get("source").and_then(Value::as_str), Some("agent_host"));
        let detail = row.get("source_detail").expect("source_detail");
        assert_eq!(detail.get("host").and_then(Value::as_str), Some("claude-code"));
        assert_eq!(detail.get("scope").and_then(Value::as_str), Some("global"));
        assert_eq!(detail.get("structural_fields_stripped").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn skill_list_filters_by_host_id() {
        let (home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        write_agent_host_claude_skill(
            &home,
            "claude-only",
            "---\nname: claude-only\ndescription: claude scoped\n---\nBody.\n",
        );
        let server = new_ao_mcp_server(&project_root_for(&project));

        let result = server
            .ao_skill_list(Parameters(SkillListInput { project_root: None, source: Some("claude-code".to_string()) }))
            .await
            .expect("filter by host id");
        let payload = data(&result);
        let skills = payload.pointer("/skills").and_then(Value::as_array).expect("skills");
        assert!(skills.iter().any(|skill| skill.get("name").and_then(Value::as_str) == Some("claude-only")));
        for skill in skills {
            let detail = skill.get("source_detail").expect("source_detail");
            assert_eq!(detail.get("host").and_then(Value::as_str), Some("claude-code"));
        }
    }

    #[tokio::test]
    async fn skill_get_surfaces_warnings_for_inert_activation_tool() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut input = create_input("typo-activation", "desc", "body");
        input.activation = Some(SkillActivationInput { tools: vec!["claud".to_string()], models: Vec::new() });
        let created = server.ao_skill_create(Parameters(input)).await.expect("create skill");
        let create_payload = data(&created);
        let create_warnings = create_payload.get("warnings").and_then(Value::as_array).expect("create warnings");
        assert!(
            create_warnings.iter().any(|w| w.as_str().is_some_and(|s| s.contains("'claud' is not a built-in tool id"))),
            "create should surface the inert activation tool: {create_warnings:?}"
        );

        let got = server
            .ao_skill_get(Parameters(SkillGetInput { project_root: None, name: "typo-activation".to_string() }))
            .await
            .expect("get skill");
        let payload = data(&got);
        let warnings = payload.get("warnings").and_then(Value::as_array).expect("get warnings");
        assert!(
            warnings.iter().any(|w| w.as_str().is_some_and(|s| s.contains("activation.tools[0]"))),
            "get should surface the inert activation tool: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn skill_get_omits_warnings_for_clean_definition() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let mut input = create_input("clean-activation", "desc", "body");
        input.activation =
            Some(SkillActivationInput { tools: vec!["claude".to_string(), "codex".to_string()], models: Vec::new() });
        let created = server.ao_skill_create(Parameters(input)).await.expect("create skill");
        assert!(data(&created).get("warnings").is_none(), "clean definition must not carry warnings");

        let got = server
            .ao_skill_get(Parameters(SkillGetInput { project_root: None, name: "clean-activation".to_string() }))
            .await
            .expect("get skill");
        assert!(data(&got).get("warnings").is_none(), "clean definition must not carry warnings");
    }

    #[tokio::test]
    async fn skill_get_returns_error_for_unknown_skill() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_skill_get(Parameters(SkillGetInput { project_root: None, name: "nonexistent-skill-xyz".to_string() }))
            .await
            .expect_err("unknown skill should be an MCP error");
        let message = err.message.to_string();
        assert!(message.contains("nonexistent-skill-xyz"), "error should mention skill name, got {message}");
    }

    #[tokio::test]
    async fn skill_search_rejects_empty_query() {
        let (_home, _guard) = isolated_home();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_skill_search(Parameters(SkillSearchInput {
                project_root: None,
                query: "   ".to_string(),
                source: None,
                limit: None,
            }))
            .await
            .expect_err("empty query should be rejected");
        assert!(err.message.contains("query"), "error should mention query: {}", err.message);
    }
}
