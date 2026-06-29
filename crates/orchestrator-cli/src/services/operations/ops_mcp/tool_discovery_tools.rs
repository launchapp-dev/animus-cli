//! `animus.tools.*` MCP discovery meta-tools.
//!
//! Agents — especially providers with tight context budgets — should not have
//! to carry all built-in tool schemas just to find the one they need. These
//! tools search the server's own live `ToolRouter` (`self.tool_router`), so
//! every registered tool is discoverable automatically, including the
//! discovery tools themselves and the management-gated tools when serving
//! with `--management`. There is no hand-maintained tool table.

use super::*;
use rmcp::model::Tool;
use std::collections::{BTreeMap, BTreeSet};

const TOOLS_SEARCH_SCHEMA: &str = "animus.tools.search.v1";
const TOOLS_LIST_SCHEMA: &str = "animus.tools.list.v1";
const DEFAULT_TOOLS_SEARCH_LIMIT: usize = 8;
const MAX_TOOLS_SEARCH_LIMIT: usize = 50;

// Per-token weights: a hit in the tool name outranks a hit in the description,
// which outranks a hit in the parameter names/descriptions.
const NAME_TOKEN_WEIGHT: u32 = 10;
const DESCRIPTION_TOKEN_WEIGHT: u32 = 3;
const PARAM_TOKEN_WEIGHT: u32 = 1;
// Querying an exact tool name must return that tool first, ahead of any
// accumulation of partial matches elsewhere.
const EXACT_NAME_BONUS: u32 = 1000;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct ToolsSearchInput {
    /// Keywords matched against tool names, descriptions, and parameter
    /// names. Example: "pause workflow" or "animus.queue.hold".
    pub(super) query: String,
    /// Maximum number of matches to return. Defaults to 8, clamped to 1..=50.
    #[serde(default)]
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(super) struct ToolsListInput {}

#[derive(Debug, Clone, Serialize)]
struct ParamSummary {
    name: String,
    #[serde(rename = "type")]
    type_label: String,
    required: bool,
    description: String,
}

#[derive(Debug, Clone)]
struct ToolSearchEntry {
    name: String,
    description: String,
    params: Vec<ParamSummary>,
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

fn schema_type_label(prop: &Value) -> String {
    match prop.get("type") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(items)) => {
            let parts: Vec<&str> = items.iter().filter_map(Value::as_str).filter(|value| *value != "null").collect();
            if parts.is_empty() {
                "any".to_string()
            } else {
                parts.join("|")
            }
        }
        _ if prop.get("enum").is_some() => "enum".to_string(),
        _ if prop.get("$ref").is_some() || prop.get("anyOf").is_some() || prop.get("oneOf").is_some() => {
            "object".to_string()
        }
        _ => "any".to_string(),
    }
}

fn param_summaries(input_schema: &JsonObject) -> Vec<ParamSummary> {
    let required: BTreeSet<&str> = input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = input_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut params: Vec<ParamSummary> = properties
        .iter()
        .map(|(name, prop)| ParamSummary {
            name: name.clone(),
            type_label: schema_type_label(prop),
            required: required.contains(name.as_str()),
            description: prop.get("description").and_then(Value::as_str).map(first_line).unwrap_or_default(),
        })
        .collect();
    params.sort_by(|a, b| (b.required, &a.name).cmp(&(a.required, &b.name)));
    params
}

fn tool_search_entries(tools: &[Tool]) -> Vec<ToolSearchEntry> {
    tools
        .iter()
        .map(|tool| ToolSearchEntry {
            name: tool.name.to_string(),
            description: tool.description.as_deref().unwrap_or_default().to_string(),
            params: param_summaries(&tool.input_schema),
        })
        .collect()
}

fn tokenize(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
        .filter(|token| seen.insert(token.to_string()))
        .map(str::to_string)
        .collect()
}

fn score_entry(entry: &ToolSearchEntry, tokens: &[String], normalized_query: &str) -> u32 {
    let name = entry.name.to_lowercase();
    let description = entry.description.to_lowercase();
    let params: Vec<(String, String)> =
        entry.params.iter().map(|param| (param.name.to_lowercase(), param.description.to_lowercase())).collect();

    let mut score = 0;
    for token in tokens {
        if name.contains(token.as_str()) {
            score += NAME_TOKEN_WEIGHT;
        }
        if description.contains(token.as_str()) {
            score += DESCRIPTION_TOKEN_WEIGHT;
        }
        if params
            .iter()
            .any(|(param_name, param_desc)| param_name.contains(token.as_str()) || param_desc.contains(token.as_str()))
        {
            score += PARAM_TOKEN_WEIGHT;
        }
    }
    if score > 0 && name == normalized_query {
        score += EXACT_NAME_BONUS;
    }
    score
}

fn tools_search_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_TOOLS_SEARCH_LIMIT).clamp(1, MAX_TOOLS_SEARCH_LIMIT)
}

pub(super) fn build_tools_search_result(tools: &[Tool], query: &str, limit: Option<usize>) -> Result<Value, String> {
    let trimmed = query.trim();
    let tokens = tokenize(trimmed);
    if tokens.is_empty() {
        return Err("query must contain at least one keyword".to_string());
    }
    let normalized_query = trimmed.to_lowercase();
    let limit = tools_search_limit(limit);

    let entries = tool_search_entries(tools);
    let mut scored: Vec<(u32, &ToolSearchEntry)> = entries
        .iter()
        .map(|entry| (score_entry(entry, &tokens, &normalized_query), entry))
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    let total_matches = scored.len();
    scored.truncate(limit);

    let matches: Vec<Value> = scored
        .into_iter()
        .map(|(score, entry)| {
            json!({
                "name": entry.name,
                "score": score,
                "description": entry.description,
                "params": entry.params,
            })
        })
        .collect();

    Ok(json!({
        "schema": TOOLS_SEARCH_SCHEMA,
        "query": trimmed,
        "limit": limit,
        "total_tools": tools.len(),
        "total_matches": total_matches,
        "count": matches.len(),
        "matches": matches,
    }))
}

fn one_line_summary(description: &str) -> String {
    let description = first_line(description);
    // Built-in descriptions follow "Summary. Purpose: ... Example: ..."; the
    // leading summary sentence is the one-liner.
    if let Some(index) = description.find(" Purpose:") {
        return description[..index].trim_end().to_string();
    }
    match description.find(". ") {
        Some(index) => description[..=index].trim_end().to_string(),
        None => description,
    }
}

fn tool_group(name: &str) -> String {
    let mut segments = name.split('.');
    match (segments.next(), segments.next()) {
        (Some("animus"), Some(group)) => group.to_string(),
        _ => name.to_string(),
    }
}

pub(super) fn build_tools_list_result(tools: &[Tool]) -> Value {
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for tool in tools {
        groups.entry(tool_group(&tool.name)).or_default().push(json!({
            "name": tool.name,
            "summary": one_line_summary(tool.description.as_deref().unwrap_or_default()),
        }));
    }
    let groups: Vec<Value> =
        groups.into_iter().map(|(group, tools)| json!({ "group": group, "tools": tools })).collect();
    json!({
        "schema": TOOLS_LIST_SCHEMA,
        "count": tools.len(),
        "groups": groups,
    })
}

#[tool_router(router = tool_discovery_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.tools.search",
        description = "Search the live MCP tool registry by intent keywords. Purpose: Discover which animus.* tools exist for a goal without loading every schema — keywords are matched against tool names, descriptions, and parameter names, and results are ranked (name hits outrank description hits outrank parameter hits; an exact tool-name query always ranks first) with a compact parameter summary per match. Prerequisites: None. Example: {\"query\": \"pause workflow\"} or {\"query\": \"queue hold\", \"limit\": 5}. Sequencing: Call this first when unsure which tool fits an intent, then call the matched tool; use animus.tools.list for a grouped overview of the whole surface.",
        input_schema = ao_schema_for_type::<ToolsSearchInput>()
    )]
    async fn ao_tools_search(&self, params: Parameters<ToolsSearchInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        match build_tools_search_result(&self.tool_router.list_all(), &input.query, input.limit) {
            Ok(payload) => Ok(CallToolResult::structured(payload)),
            Err(message) => Ok(CallToolResult::structured_error(json!({
                "tool": "animus.tools.search",
                "error": { "message": message },
            }))),
        }
    }

    #[tool(
        name = "animus.tools.list",
        description = "List every registered MCP tool grouped by family. Purpose: Get a compact catalog of the whole animus.* tool surface — one-line summary per tool, no input schemas. Prerequisites: None. Example: {}. Sequencing: Use animus.tools.search to drill into a specific intent and retrieve parameter summaries for the matched tools.",
        input_schema = ao_schema_for_type::<ToolsListInput>()
    )]
    async fn ao_tools_list(&self, params: Parameters<ToolsListInput>) -> Result<CallToolResult, McpError> {
        let _ = params;
        Ok(CallToolResult::structured(build_tools_list_result(&self.tool_router.list_all())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(value: Value) -> std::sync::Arc<JsonObject> {
        std::sync::Arc::new(value.as_object().expect("schema fixture should be an object").clone())
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "animus.queue.hold",
                "Hold one or more queued subject dispatches.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "subject_id": { "type": "string", "description": "Subject to hold" },
                        "project_root": { "type": ["string", "null"] }
                    },
                    "required": ["subject_id"]
                })),
            ),
            Tool::new(
                "animus.workflow.pause",
                "Pause a running workflow at the next phase boundary.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "workflow_id": { "type": "string", "description": "Workflow to pause" }
                    },
                    "required": ["workflow_id"]
                })),
            ),
            Tool::new(
                "animus.subject.list",
                "List subjects. Purpose: enumerate items; supports a pause flag in filters.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "Subject kind" }
                    },
                    "required": ["kind"]
                })),
            ),
            Tool::new(
                "animus.daemon.status",
                "Show daemon status.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "pause": { "type": "boolean", "description": "Include pause overlay" }
                    }
                })),
            ),
        ]
    }

    fn match_names(result: &Value) -> Vec<String> {
        result
            .get("matches")
            .and_then(Value::as_array)
            .expect("matches array")
            .iter()
            .map(|item| item.get("name").and_then(Value::as_str).expect("match name").to_string())
            .collect()
    }

    #[test]
    fn name_hits_outrank_description_hits_outrank_param_hits() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "pause", None).expect("search should succeed");
        let names = match_names(&result);
        // name hit (workflow.pause) > description hit (subject.list) > param hit (daemon.status)
        assert_eq!(names, vec!["animus.workflow.pause", "animus.subject.list", "animus.daemon.status"]);
    }

    #[test]
    fn multi_token_query_accumulates_across_fields() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "queue hold", None).expect("search should succeed");
        let names = match_names(&result);
        assert_eq!(names.first().map(String::as_str), Some("animus.queue.hold"));
    }

    #[test]
    fn exact_tool_name_query_ranks_that_tool_first() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "animus.subject.list", None).expect("search should succeed");
        let names = match_names(&result);
        assert_eq!(names.first().map(String::as_str), Some("animus.subject.list"));
        let top_score = result.pointer("/matches/0/score").and_then(Value::as_u64).expect("score");
        assert!(top_score > u64::from(EXACT_NAME_BONUS), "exact-name bonus should apply, got {top_score}");
    }

    #[test]
    fn limit_truncates_matches_and_is_clamped() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "animus", Some(2)).expect("search should succeed");
        assert_eq!(match_names(&result).len(), 2);
        assert_eq!(result.get("total_matches").and_then(Value::as_u64), Some(4));

        let clamped = build_tools_search_result(&tools, "animus", Some(0)).expect("search should succeed");
        assert_eq!(match_names(&clamped).len(), 1);
        assert_eq!(clamped.get("limit").and_then(Value::as_u64), Some(1));
        assert_eq!(tools_search_limit(Some(MAX_TOOLS_SEARCH_LIMIT + 10)), MAX_TOOLS_SEARCH_LIMIT);
        assert_eq!(tools_search_limit(None), DEFAULT_TOOLS_SEARCH_LIMIT);
    }

    #[test]
    fn empty_query_is_rejected() {
        let tools = fixture_tools();
        assert!(build_tools_search_result(&tools, "   ", None).is_err());
        assert!(build_tools_search_result(&tools, "...", None).is_err());
    }

    #[test]
    fn unmatched_query_returns_zero_matches() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "zzzznonexistent", None).expect("search should succeed");
        assert_eq!(result.get("count").and_then(Value::as_u64), Some(0));
        assert!(match_names(&result).is_empty());
    }

    #[test]
    fn matches_carry_compact_param_summaries() {
        let tools = fixture_tools();
        let result = build_tools_search_result(&tools, "animus.queue.hold", Some(1)).expect("search should succeed");
        let params = result.pointer("/matches/0/params").and_then(Value::as_array).expect("params array");
        assert_eq!(params.len(), 2);
        // Required params sort first.
        assert_eq!(params[0].get("name").and_then(Value::as_str), Some("subject_id"));
        assert_eq!(params[0].get("type").and_then(Value::as_str), Some("string"));
        assert_eq!(params[0].get("required").and_then(Value::as_bool), Some(true));
        assert_eq!(params[0].get("description").and_then(Value::as_str), Some("Subject to hold"));
        assert_eq!(params[1].get("name").and_then(Value::as_str), Some("project_root"));
        assert_eq!(params[1].get("type").and_then(Value::as_str), Some("string"));
        assert_eq!(params[1].get("required").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn every_registered_tool_is_searchable_by_its_exact_name() {
        // Live-registry coverage: searching any registered tool's exact name
        // must return that tool first. Management mode = full built-in set.
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", true, None, None, None);
        let tools = server.tool_router.list_all();
        assert!(!tools.is_empty(), "live tool router should not be empty");
        for tool in &tools {
            let result = build_tools_search_result(&tools, &tool.name, Some(1))
                .unwrap_or_else(|error| panic!("search for {} failed: {error}", tool.name));
            let names = match_names(&result);
            assert_eq!(
                names.first().map(String::as_str),
                Some(tool.name.as_ref()),
                "exact-name search for {} should return it first",
                tool.name
            );
        }
    }

    #[test]
    fn discovery_tools_appear_in_their_own_search_results() {
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", false, None, None, None);
        let tools = server.tool_router.list_all();
        let result = build_tools_search_result(&tools, "search tools registry", None).expect("search should succeed");
        let names = match_names(&result);
        assert!(
            names.iter().any(|name| name == "animus.tools.search"),
            "animus.tools.search should be discoverable through itself, got {names:?}"
        );
    }

    #[test]
    fn tools_list_groups_the_live_registry_without_schemas() {
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", false, None, None, None);
        let tools = server.tool_router.list_all();
        let result = build_tools_list_result(&tools);
        assert_eq!(result.get("schema").and_then(Value::as_str), Some(TOOLS_LIST_SCHEMA));
        assert_eq!(result.get("count").and_then(Value::as_u64), Some(tools.len() as u64));

        let groups = result.get("groups").and_then(Value::as_array).expect("groups array");
        let group_names: Vec<&str> =
            groups.iter().filter_map(|group| group.get("group").and_then(Value::as_str)).collect();
        assert!(group_names.contains(&"tools"), "tools group should be present, got {group_names:?}");
        assert!(group_names.contains(&"queue"), "queue group should be present, got {group_names:?}");

        let listed: usize =
            groups.iter().filter_map(|group| group.get("tools").and_then(Value::as_array)).map(Vec::len).sum();
        assert_eq!(listed, tools.len(), "every registered tool should appear exactly once");
        for group in groups {
            for tool in group.get("tools").and_then(Value::as_array).expect("group tools") {
                assert!(tool.get("schema").is_none() && tool.get("params").is_none());
                let summary = tool.get("summary").and_then(Value::as_str).expect("summary");
                assert!(!summary.contains("Purpose:"), "summary should be the leading sentence only: {summary}");
            }
        }
    }

    #[test]
    fn one_line_summary_takes_leading_sentence() {
        assert_eq!(
            one_line_summary("Hold queued dispatches. Purpose: stop selection. Example: {}."),
            "Hold queued dispatches."
        );
        assert_eq!(one_line_summary("Plain sentence. Second sentence."), "Plain sentence.");
        assert_eq!(one_line_summary("No trailing period"), "No trailing period");
    }

    #[tokio::test]
    async fn tools_search_handler_returns_structured_result_through_dispatch_surface() {
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", false, None, None, None);
        let result = server
            .ao_tools_search(Parameters(ToolsSearchInput { query: "animus.tools.search".to_string(), limit: Some(3) }))
            .await
            .expect("handler should succeed");
        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload.get("schema").and_then(Value::as_str), Some(TOOLS_SEARCH_SCHEMA));
        assert_eq!(payload.pointer("/matches/0/name").and_then(Value::as_str), Some("animus.tools.search"));
    }

    #[tokio::test]
    async fn tools_search_handler_rejects_empty_query_as_structured_error() {
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", false, None, None, None);
        let result = server
            .ao_tools_search(Parameters(ToolsSearchInput { query: "  ".to_string(), limit: None }))
            .await
            .expect("handler should not hard-error");
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn tools_list_handler_returns_structured_catalog() {
        let server = super::super::new_ao_mcp_server_with_options("/tmp/project", false, None, None, None);
        let result = server.ao_tools_list(Parameters(ToolsListInput {})).await.expect("handler should succeed");
        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload.get("schema").and_then(Value::as_str), Some(TOOLS_LIST_SCHEMA));
    }
}
