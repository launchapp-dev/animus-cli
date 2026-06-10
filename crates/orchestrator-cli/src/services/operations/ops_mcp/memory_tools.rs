use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryGetInput {
    pub(super) agent_id: String,
    #[serde(default)]
    pub(super) entry_id: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryListInput {
    pub(super) agent_id: String,
    #[serde(default)]
    pub(super) prefix: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryAppendInput {
    pub(super) agent_id: String,
    pub(super) text: String,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct MemoryClearInput {
    pub(super) agent_id: String,
    #[serde(default)]
    pub(super) entry_id: Option<String>,
    #[serde(default)]
    pub(super) delete_all: Option<bool>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

fn memory_project_root(default: &str, override_value: Option<String>) -> String {
    override_value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn entry_to_json(entry: &animus_runtime_shared::AgentMemoryEntry) -> Value {
    serde_json::json!({
        "id": entry.id,
        "text": entry.text,
        "created_at": entry.created_at,
        "source": entry.source,
    })
}

fn document_to_json(document: &animus_runtime_shared::AgentMemoryDocument) -> Value {
    serde_json::json!({
        "agent_id": document.agent_id,
        "updated_at": document.updated_at,
        "entries": document.entries.iter().map(entry_to_json).collect::<Vec<_>>(),
    })
}

fn structured_ok(tool_name: &str, data: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::structured(json!({
        "tool": tool_name,
        "result": data,
    })))
}

fn structured_err(tool_name: &str, message: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::structured_error(json!({
        "tool": tool_name,
        "error": message,
    })))
}

#[tool_router(router = memory_tool_router, vis = "pub(super)")]
impl MemoryMcpServer {
    #[tool(
        name = "animus.memory.get",
        description = "Read project-scoped agent memory. Purpose: Fetch the full memory document for an agent profile, optionally narrowing to a single entry by id. Returns: { agent_id, updated_at, entries: [{ id, text, created_at, source }] } and, when entry_id is provided, an `entry` field with that single entry or null. Example: {\"agent_id\": \"architect\"}.",
        input_schema = ao_schema_for_type::<MemoryGetInput>()
    )]
    async fn ao_memory_get(&self, params: Parameters<MemoryGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.get", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::load_agent_memory(&project_root, &agent_id) {
            Ok(document) => {
                let mut payload = document_to_json(&document);
                if let Some(entry_id) = input.entry_id.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
                    let entry = document
                        .entries
                        .iter()
                        .find(|entry| entry.id == entry_id)
                        .map(entry_to_json)
                        .unwrap_or(Value::Null);
                    if let Some(object) = payload.as_object_mut() {
                        object.insert("entry".to_string(), entry);
                    }
                }
                structured_ok("animus.memory.get", payload)
            }
            Err(err) => structured_err("animus.memory.get", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.list",
        description = "List project-scoped agent memory entries. Purpose: Enumerate all memory entries for an agent profile with optional case-sensitive `prefix` filter on entry text. Returns: { agent_id, count, entries: [{ id, text, created_at, source }] }. Example: {\"agent_id\": \"architect\", \"prefix\": \"decision:\"}.",
        input_schema = ao_schema_for_type::<MemoryListInput>()
    )]
    async fn ao_memory_list(&self, params: Parameters<MemoryListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.list", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::load_agent_memory(&project_root, &agent_id) {
            Ok(document) => {
                let prefix = input.prefix.as_deref().map(str::trim).map(ToOwned::to_owned);
                let mut entries: Vec<Value> = document
                    .entries
                    .iter()
                    .filter(|entry| match prefix.as_deref() {
                        Some(prefix) if !prefix.is_empty() => entry.text.starts_with(prefix),
                        _ => true,
                    })
                    .map(entry_to_json)
                    .collect();
                if let Some(limit) = input.limit {
                    if entries.len() > limit {
                        entries.truncate(limit);
                    }
                }
                let payload = serde_json::json!({
                    "agent_id": document.agent_id,
                    "count": entries.len(),
                    "entries": entries,
                });
                structured_ok("animus.memory.list", payload)
            }
            Err(err) => structured_err("animus.memory.list", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.append",
        description = "Append a project-scoped agent memory entry. Purpose: Add a new memory entry for an agent profile. The entry is assigned a fresh uuid and timestamp. Returns: { agent_id, updated_at, entry: { id, text, created_at, source } }. Example: {\"agent_id\": \"architect\", \"text\": \"Prefer explicit contracts.\", \"source\": \"phase:architecture\"}.",
        input_schema = ao_schema_for_type::<MemoryAppendInput>()
    )]
    async fn ao_memory_append(&self, params: Parameters<MemoryAppendInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.append", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::append_agent_memory(&project_root, &agent_id, &input.text, input.source.as_deref())
        {
            Ok(document) => {
                let latest = document.entries.last().map(entry_to_json).unwrap_or(Value::Null);
                let payload = serde_json::json!({
                    "agent_id": document.agent_id,
                    "updated_at": document.updated_at,
                    "entry": latest,
                });
                structured_ok("animus.memory.append", payload)
            }
            Err(err) => structured_err("animus.memory.append", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.clear",
        description = "Clear project-scoped agent memory. Purpose: Delete a single memory entry by `entry_id`, or all entries for an agent profile when `delete_all` is true. Either `entry_id` or `delete_all: true` is required. Returns: { agent_id, updated_at, removed_entry_id?, deleted_count, entries }. Example single: {\"agent_id\": \"architect\", \"entry_id\": \"abc-uuid\"}. Example all: {\"agent_id\": \"architect\", \"delete_all\": true}.",
        input_schema = ao_schema_for_type::<MemoryClearInput>()
    )]
    async fn ao_memory_clear(&self, params: Parameters<MemoryClearInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.clear", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        let entry_id =
            input.entry_id.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
        let delete_all = input.delete_all.unwrap_or(false);
        if entry_id.is_none() && !delete_all {
            return structured_err(
                "animus.memory.clear",
                "either entry_id or delete_all=true is required to clear memory".to_string(),
            );
        }
        if delete_all {
            match animus_runtime_shared::clear_agent_memory(&project_root, &agent_id) {
                Ok(document) => {
                    let payload = serde_json::json!({
                        "agent_id": document.agent_id,
                        "updated_at": document.updated_at,
                        "deleted_count": "all",
                        "entries": [],
                    });
                    structured_ok("animus.memory.clear", payload)
                }
                Err(err) => structured_err("animus.memory.clear", err.to_string()),
            }
        } else {
            let entry_id = entry_id.expect("entry_id checked above");
            match animus_runtime_shared::delete_agent_memory_entry(&project_root, &agent_id, &entry_id) {
                Ok((document, removed)) => {
                    let payload = serde_json::json!({
                        "agent_id": document.agent_id,
                        "updated_at": document.updated_at,
                        "removed_entry_id": entry_id,
                        "deleted_count": if removed { 1 } else { 0 },
                        "entries": document.entries.iter().map(entry_to_json).collect::<Vec<_>>(),
                    });
                    structured_ok("animus.memory.clear", payload)
                }
                Err(err) => structured_err("animus.memory.clear", err.to_string()),
            }
        }
    }
}

#[tool_router(router = memory_tool_router_for_ao, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.memory.get",
        description = "Read project-scoped agent memory. Purpose: Fetch the full memory document for an agent profile, optionally narrowing to a single entry by id. Returns: { agent_id, updated_at, entries: [{ id, text, created_at, source }] } and, when entry_id is provided, an `entry` field with that single entry or null. Example: {\"agent_id\": \"architect\"}.",
        input_schema = ao_schema_for_type::<MemoryGetInput>()
    )]
    async fn ao_memory_get(&self, params: Parameters<MemoryGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.get", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::load_agent_memory(&project_root, &agent_id) {
            Ok(document) => {
                let mut payload = document_to_json(&document);
                if let Some(entry_id) = input.entry_id.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
                    let entry = document
                        .entries
                        .iter()
                        .find(|entry| entry.id == entry_id)
                        .map(entry_to_json)
                        .unwrap_or(Value::Null);
                    if let Some(object) = payload.as_object_mut() {
                        object.insert("entry".to_string(), entry);
                    }
                }
                structured_ok("animus.memory.get", payload)
            }
            Err(err) => structured_err("animus.memory.get", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.list",
        description = "List project-scoped agent memory entries. Purpose: Enumerate memory entries for an agent profile with optional case-sensitive `prefix` filter on entry text. Returns: { agent_id, count, entries: [{ id, text, created_at, source }] }. Example: {\"agent_id\": \"architect\", \"prefix\": \"decision:\"}.",
        input_schema = ao_schema_for_type::<MemoryListInput>()
    )]
    async fn ao_memory_list(&self, params: Parameters<MemoryListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.list", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::load_agent_memory(&project_root, &agent_id) {
            Ok(document) => {
                let prefix = input.prefix.as_deref().map(str::trim).map(ToOwned::to_owned);
                let mut entries: Vec<Value> = document
                    .entries
                    .iter()
                    .filter(|entry| match prefix.as_deref() {
                        Some(prefix) if !prefix.is_empty() => entry.text.starts_with(prefix),
                        _ => true,
                    })
                    .map(entry_to_json)
                    .collect();
                if let Some(limit) = input.limit {
                    if entries.len() > limit {
                        entries.truncate(limit);
                    }
                }
                let payload = serde_json::json!({
                    "agent_id": document.agent_id,
                    "count": entries.len(),
                    "entries": entries,
                });
                structured_ok("animus.memory.list", payload)
            }
            Err(err) => structured_err("animus.memory.list", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.append",
        description = "Append a project-scoped agent memory entry. Purpose: Add a new memory entry for an agent profile. The entry is assigned a fresh uuid and timestamp. Returns: { agent_id, updated_at, entry: { id, text, created_at, source } }. Example: {\"agent_id\": \"architect\", \"text\": \"Prefer explicit contracts.\", \"source\": \"phase:architecture\"}.",
        input_schema = ao_schema_for_type::<MemoryAppendInput>()
    )]
    async fn ao_memory_append(&self, params: Parameters<MemoryAppendInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.append", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::append_agent_memory(&project_root, &agent_id, &input.text, input.source.as_deref())
        {
            Ok(document) => {
                let latest = document.entries.last().map(entry_to_json).unwrap_or(Value::Null);
                let payload = serde_json::json!({
                    "agent_id": document.agent_id,
                    "updated_at": document.updated_at,
                    "entry": latest,
                });
                structured_ok("animus.memory.append", payload)
            }
            Err(err) => structured_err("animus.memory.append", err.to_string()),
        }
    }

    #[tool(
        name = "animus.memory.clear",
        description = "Clear project-scoped agent memory. Purpose: Delete a single memory entry by `entry_id`, or all entries for an agent profile when `delete_all` is true. Either `entry_id` or `delete_all: true` is required. Returns: { agent_id, updated_at, removed_entry_id?, deleted_count, entries }. Example single: {\"agent_id\": \"architect\", \"entry_id\": \"abc-uuid\"}. Example all: {\"agent_id\": \"architect\", \"delete_all\": true}.",
        input_schema = ao_schema_for_type::<MemoryClearInput>()
    )]
    async fn ao_memory_clear(&self, params: Parameters<MemoryClearInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = input.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return structured_err("animus.memory.clear", "agent_id must not be empty".to_string());
        }
        let project_root = memory_project_root(&self.default_project_root, input.project_root);
        let entry_id =
            input.entry_id.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
        let delete_all = input.delete_all.unwrap_or(false);
        if entry_id.is_none() && !delete_all {
            return structured_err(
                "animus.memory.clear",
                "either entry_id or delete_all=true is required to clear memory".to_string(),
            );
        }
        if delete_all {
            match animus_runtime_shared::clear_agent_memory(&project_root, &agent_id) {
                Ok(document) => {
                    let payload = serde_json::json!({
                        "agent_id": document.agent_id,
                        "updated_at": document.updated_at,
                        "deleted_count": "all",
                        "entries": [],
                    });
                    structured_ok("animus.memory.clear", payload)
                }
                Err(err) => structured_err("animus.memory.clear", err.to_string()),
            }
        } else {
            let entry_id = entry_id.expect("entry_id checked above");
            match animus_runtime_shared::delete_agent_memory_entry(&project_root, &agent_id, &entry_id) {
                Ok((document, removed)) => {
                    let payload = serde_json::json!({
                        "agent_id": document.agent_id,
                        "updated_at": document.updated_at,
                        "removed_entry_id": entry_id,
                        "deleted_count": if removed { 1 } else { 0 },
                        "entries": document.entries.iter().map(entry_to_json).collect::<Vec<_>>(),
                    });
                    structured_ok("animus.memory.clear", payload)
                }
                Err(err) => structured_err("animus.memory.clear", err.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod memory_tool_tests {
    use super::super::new_memory_mcp_server;
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::Value;
    use tempfile::tempdir;

    // Agent memory lives under `protocol::scoped_state_root`, which resolves
    // through the live `$HOME`. Pin HOME to a per-test tempdir for the whole
    // test so sibling tests that swap HOME (via `EnvVarGuard`, which holds
    // the process-wide env lock this guard also takes) can neither move the
    // scope mid-test nor delete it out from under us.
    fn isolated_home() -> (tempfile::TempDir, protocol::test_utils::EnvVarGuard) {
        let home = tempdir().expect("home tempdir");
        let guard = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        (home, guard)
    }

    fn structured(result: &rmcp::model::CallToolResult) -> Value {
        result.structured_content.clone().expect("expected structured_content on tool result")
    }

    fn data(result: &rmcp::model::CallToolResult) -> Value {
        let payload = structured(result);
        payload.get("result").cloned().expect("structured result should include `result`")
    }

    fn error_message(result: &rmcp::model::CallToolResult) -> String {
        let payload = structured(result);
        payload.get("error").and_then(Value::as_str).unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn memory_router_has_all_four_tools() {
        let server = new_memory_mcp_server("/tmp/project");
        let names: Vec<String> = server.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        assert!(names.contains(&"animus.memory.get".to_string()), "router missing animus.memory.get");
        assert!(names.contains(&"animus.memory.list".to_string()), "router missing animus.memory.list");
        assert!(names.contains(&"animus.memory.append".to_string()), "router missing animus.memory.append");
        assert!(names.contains(&"animus.memory.clear".to_string()), "router missing animus.memory.clear");
        assert!(server.tool_router.has_route("animus.memory.get"));
    }

    #[tokio::test]
    async fn memory_append_then_get_roundtrip() {
        let (_home, _home_guard) = isolated_home();
        let project = tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let server = new_memory_mcp_server(&project_root);

        let append = server
            .ao_memory_append(Parameters(MemoryAppendInput {
                agent_id: "architect".to_string(),
                text: "Prefer explicit contracts.".to_string(),
                source: Some("phase:architecture".to_string()),
                project_root: None,
            }))
            .await
            .expect("append should not error");
        let appended = data(&append);
        let entry_id = appended.pointer("/entry/id").and_then(Value::as_str).expect("entry id").to_string();

        let fetched = server
            .ao_memory_get(Parameters(MemoryGetInput {
                agent_id: "architect".to_string(),
                entry_id: None,
                project_root: None,
            }))
            .await
            .expect("get should not error");
        let payload = data(&fetched);
        let entries = payload.pointer("/entries").and_then(Value::as_array).expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pointer("/text").and_then(Value::as_str), Some("Prefer explicit contracts."));
        assert_eq!(entries[0].pointer("/source").and_then(Value::as_str), Some("phase:architecture"));

        let fetched_by_id = server
            .ao_memory_get(Parameters(MemoryGetInput {
                agent_id: "architect".to_string(),
                entry_id: Some(entry_id.clone()),
                project_root: None,
            }))
            .await
            .expect("get by id should not error");
        let entry = data(&fetched_by_id).pointer("/entry").cloned().expect("entry field");
        assert_eq!(entry.pointer("/id").and_then(Value::as_str), Some(entry_id.as_str()));
    }

    #[tokio::test]
    async fn memory_list_returns_appended_entries_and_respects_prefix() {
        let (_home, _home_guard) = isolated_home();
        let project = tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let server = new_memory_mcp_server(&project_root);

        for text in ["decision: ship", "decision: revert", "note: ignored"] {
            server
                .ao_memory_append(Parameters(MemoryAppendInput {
                    agent_id: "architect".to_string(),
                    text: text.to_string(),
                    source: None,
                    project_root: None,
                }))
                .await
                .expect("append");
        }

        let all = server
            .ao_memory_list(Parameters(MemoryListInput {
                agent_id: "architect".to_string(),
                prefix: None,
                limit: None,
                project_root: None,
            }))
            .await
            .expect("list");
        assert_eq!(data(&all).pointer("/count").and_then(Value::as_u64), Some(3));

        let filtered = server
            .ao_memory_list(Parameters(MemoryListInput {
                agent_id: "architect".to_string(),
                prefix: Some("decision:".to_string()),
                limit: None,
                project_root: None,
            }))
            .await
            .expect("list with prefix");
        let payload = data(&filtered);
        assert_eq!(payload.pointer("/count").and_then(Value::as_u64), Some(2));
        let entries = payload.pointer("/entries").and_then(Value::as_array).expect("entries");
        assert!(entries.iter().all(|entry| entry
            .pointer("/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with("decision:"))));
    }

    #[tokio::test]
    async fn memory_clear_single_entry_keeps_other_entries() {
        let (_home, _home_guard) = isolated_home();
        let project = tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let server = new_memory_mcp_server(&project_root);

        let first = server
            .ao_memory_append(Parameters(MemoryAppendInput {
                agent_id: "architect".to_string(),
                text: "First".to_string(),
                source: None,
                project_root: None,
            }))
            .await
            .expect("append first");
        let first_id = data(&first).pointer("/entry/id").and_then(Value::as_str).expect("id").to_string();

        server
            .ao_memory_append(Parameters(MemoryAppendInput {
                agent_id: "architect".to_string(),
                text: "Second".to_string(),
                source: None,
                project_root: None,
            }))
            .await
            .expect("append second");

        let cleared = server
            .ao_memory_clear(Parameters(MemoryClearInput {
                agent_id: "architect".to_string(),
                entry_id: Some(first_id.clone()),
                delete_all: None,
                project_root: None,
            }))
            .await
            .expect("clear single");
        let payload = data(&cleared);
        assert_eq!(payload.pointer("/deleted_count").and_then(Value::as_u64), Some(1));
        let remaining = payload.pointer("/entries").and_then(Value::as_array).expect("entries");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].pointer("/text").and_then(Value::as_str), Some("Second"));
    }

    #[tokio::test]
    async fn memory_clear_all_wipes_every_entry() {
        let (_home, _home_guard) = isolated_home();
        let project = tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let server = new_memory_mcp_server(&project_root);

        for text in ["a", "b", "c"] {
            server
                .ao_memory_append(Parameters(MemoryAppendInput {
                    agent_id: "architect".to_string(),
                    text: text.to_string(),
                    source: None,
                    project_root: None,
                }))
                .await
                .expect("append");
        }

        let cleared = server
            .ao_memory_clear(Parameters(MemoryClearInput {
                agent_id: "architect".to_string(),
                entry_id: None,
                delete_all: Some(true),
                project_root: None,
            }))
            .await
            .expect("clear all");
        let payload = data(&cleared);
        assert_eq!(payload.pointer("/deleted_count").and_then(Value::as_str), Some("all"));
        let entries = payload.pointer("/entries").and_then(Value::as_array).expect("entries");
        assert!(entries.is_empty());

        let after = server
            .ao_memory_list(Parameters(MemoryListInput {
                agent_id: "architect".to_string(),
                prefix: None,
                limit: None,
                project_root: None,
            }))
            .await
            .expect("list after clear");
        assert_eq!(data(&after).pointer("/count").and_then(Value::as_u64), Some(0));
    }

    #[tokio::test]
    async fn memory_clear_without_target_returns_error() {
        let (_home, _home_guard) = isolated_home();
        let project = tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let server = new_memory_mcp_server(&project_root);

        let result = server
            .ao_memory_clear(Parameters(MemoryClearInput {
                agent_id: "architect".to_string(),
                entry_id: None,
                delete_all: None,
                project_root: None,
            }))
            .await
            .expect("clear should still produce a structured result");
        assert!(error_message(&result).contains("entry_id or delete_all"));
    }

    #[tokio::test]
    async fn ao_mcp_serve_router_also_exposes_memory_tools() {
        use super::super::new_ao_mcp_server;
        let server = new_ao_mcp_server("/tmp/project");
        let names: Vec<String> = server.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        assert!(
            names.contains(&"animus.memory.get".to_string()),
            "ao mcp serve router missing animus.memory.get; saw: {:?}",
            names
        );
        assert!(
            names.contains(&"animus.memory.append".to_string()),
            "ao mcp serve router missing animus.memory.append"
        );
        assert!(names.contains(&"animus.memory.list".to_string()), "ao mcp serve router missing animus.memory.list");
        assert!(names.contains(&"animus.memory.clear".to_string()), "ao mcp serve router missing animus.memory.clear");
    }
}
