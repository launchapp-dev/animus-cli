use std::collections::BTreeMap;
use std::path::Path;

use orchestrator_config::skill_definition::{
    SkillActivation, SkillCategory, SkillDefinition, SkillModelPreference, SkillPrompt,
};
use orchestrator_config::skill_resolution::{list_available_skills, resolve_skill};
use orchestrator_config::skill_scoping::{
    load_skill_sources, validate_skill_slug, write_project_skill_yaml, AgentHostScope, SkillSourceOrigin,
    SkillWriteOutcome,
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
    /// separators. Becomes the file name `.animus/config/skill_definitions/<name>.yaml`.
    name: String,
    /// Human-readable description shown in `list`/`search`.
    description: String,
    /// The skill's instruction body. Stored as `prompt.system`.
    prompt: String,
    /// Optional discovery tags (case-insensitive substring matched by search).
    #[serde(default)]
    tags: Vec<String>,
    /// Optional tool allow/deny policy. Trusted because this is a
    /// project-scoped skill.
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
    /// Slug of the existing PROJECT-scoped skill to patch.
    name: String,
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
    serde_json::from_value::<SkillCategory>(json!(raw.trim().to_ascii_lowercase())).map_err(|_| {
        McpError::invalid_params(
            format!(
                "unknown category '{}': expected one of implementation, testing, review, research, documentation, operations, planning",
                raw.trim()
            ),
            None,
        )
    })
}

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

fn skill_full(definition: &SkillDefinition, origin: &SkillSourceOrigin) -> Value {
    let mut payload = json!({
        "definition": definition,
        "source": source_tag(origin),
        "source_detail": source_detail(origin),
    });
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
        description = "List Animus skills discoverable from this project across every source: installed packs, registry-tracked installs, user-scoped (~/.animus/skills), project-scoped (.animus/skills), and agent-host probes (~/.claude/skills/, ~/.codex/skills/, etc.). Optional `source` filter accepts \"installed\", \"user\", \"project\", \"agent_host\", or a host id like \"claude-code\". \"builtin\" remains accepted as a backward-compatible filter, but current builds do not emit builtin rows. Each result carries provenance via `source` + `source_detail` so callers can reason about trust tier.",
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
        description = "Resolve a skill by name and return its full SkillDefinition plus source provenance. Resolution honors the priority chain: project > user > installed/pack > agent-host. Returns the parsed definition (prompt, tool_policy, model, mcp_servers, capabilities, adapters, tags, etc.) under `definition`. For agent-host sources, structural fields are stripped at parse time and a `notice` field warns that only prompt text is trusted.",
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
        description = "Author a PROJECT-scoped Animus skill. Writes a full-fidelity SkillDefinition as YAML to <project_root>/.animus/config/skill_definitions/<name>.yaml (the project YAML tier that resolution reads at highest priority). `name` must be a slug (lowercase ASCII letters/digits plus '-'/'_', no path separators); `description` and `prompt` are required. Optional: tags, tool_policy {allow,deny}, model, mcp_servers, category, activation {tools,models}, capabilities. Refuses to overwrite an existing skill unless `overwrite` is true. The written file is re-parsed to guarantee it round-trips, so a malformed skill is never left on disk. Project-scope only — structural fields (tool_policy, mcp_servers) are trusted because the skill is project-local. The new skill is immediately discoverable via animus.skill.list / animus.skill.search.",
        input_schema = ao_schema_for_type::<SkillCreateInput>()
    )]
    async fn ao_skill_create(&self, params: Parameters<SkillCreateInput>) -> Result<CallToolResult, McpError> {
        let SkillCreateInput {
            project_root,
            name,
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

        let (path, outcome) = write_project_skill_yaml(Path::new(&project_root), &definition, overwrite)
            .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.create",
            "result": {
                "name": name,
                "path": path.to_string_lossy(),
                "source": "project",
                "outcome": skill_write_outcome_str(outcome),
            }
        })))
    }

    #[tool(
        name = "animus.skill.update",
        description = "Patch an existing PROJECT-scoped skill at .animus/config/skill_definitions/<name>.yaml. Only the supplied fields change; every other field is preserved from the existing definition. Patchable: description, prompt, tags, tool_policy {allow,deny}, model, mcp_servers, category, capabilities. Fails if the named skill does not resolve to a PROJECT source (use animus.skill.create for new skills; user/pack/agent-host skills are not editable via MCP). The rewritten file is re-parsed to guarantee it round-trips.",
        input_schema = ao_schema_for_type::<SkillUpdateInput>()
    )]
    async fn ao_skill_update(&self, params: Parameters<SkillUpdateInput>) -> Result<CallToolResult, McpError> {
        let SkillUpdateInput {
            project_root,
            name,
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
        let name = validate_skill_slug(&name).map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        let sources = load_skill_sources(Path::new(&project_root), None)
            .map_err(|err| McpError::internal_error(format!("failed to load skill sources: {err}"), None))?;
        let resolved = resolve_skill(&name, &sources)
            .map_err(|err| McpError::invalid_params(format!("skill '{}' not found: {}", name, err), None))?;
        if !matches!(resolved.source, SkillSourceOrigin::Project) {
            return Err(McpError::invalid_params(
                format!(
                    "skill '{}' resolves to a {} source; only project-scoped skills can be updated via MCP",
                    name,
                    source_tag(&resolved.source)
                ),
                None,
            ));
        }

        let mut definition = resolved.definition;

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

        let (path, outcome) = write_project_skill_yaml(Path::new(&project_root), &definition, true)
            .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

        Ok(CallToolResult::structured(json!({
            "tool": "animus.skill.update",
            "result": {
                "name": name,
                "path": path.to_string_lossy(),
                "source": "project",
                "outcome": skill_write_outcome_str(outcome),
            }
        })))
    }
}

#[cfg(test)]
mod skill_tool_tests {
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
