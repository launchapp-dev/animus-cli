use super::*;

#[tool_router(router = requirements_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.requirements.list",
        description = "List requirements. Purpose: Discover requirements for planning and task creation. Prerequisites: None. Example: {\"limit\": 20} or {\"status\": \"draft\"}. Sequencing: Use ao.requirements.get for details, then ao.task.create to create tasks linked to requirements.",
        input_schema = ao_schema_for_type::<PaginatedProjectRootInput>()
    )]
    async fn ao_requirements_list(
        &self,
        params: Parameters<PaginatedProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_list_tool(
            "ao.requirements.list",
            vec!["requirements".to_string(), "list".to_string()],
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.requirements.get",
        description = "Get a requirement by its ID. Purpose: View full requirement details including title, description, priority, status, and linked tasks. Prerequisites: None. Example: {\"id\": \"REQ-001\"}. Sequencing: Use after ao.requirements.list to get details, or before ao.task.create to link new tasks.",
        input_schema = ao_schema_for_type::<RequirementGetInput>()
    )]
    async fn ao_requirements_get(&self, params: Parameters<RequirementGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_get_args(input.id);
        self.run_tool("ao.requirements.get", args, input.project_root).await
    }

    #[tool(
        name = "ao.requirements.create",
        description = "Create a requirement in AO. Purpose: Add a new requirement with structured metadata and acceptance criteria. Prerequisites: None. Example: {\"title\": \"Offline mode\", \"priority\": \"must\", \"acceptance_criterion\": [\"Sync resumes after reconnect\"]}. Sequencing: Use ao.requirements.get or ao.requirements.update to inspect or refine the new requirement, and ao.task.create to create derived tasks.",
        input_schema = ao_schema_for_type::<RequirementCreateInput>()
    )]
    async fn ao_requirements_create(
        &self,
        params: Parameters<RequirementCreateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_create_args(&input);
        self.run_tool("ao.requirements.create", args, input.project_root).await
    }

    #[tool(
        name = "ao.requirements.update",
        description = "Update a requirement in AO. Purpose: Modify requirement title, description, priority, status, acceptance criteria, and linked tasks. Prerequisites: Requirement must exist. Example: {\"id\": \"REQ-001\", \"status\": \"in-progress\", \"acceptance_criterion\": [\"Exports CSV\"]}. Sequencing: Use ao.requirements.get first to inspect current state, or ao.requirements.refine for AI-assisted refinement.",
        input_schema = ao_schema_for_type::<RequirementUpdateInput>()
    )]
    async fn ao_requirements_update(
        &self,
        params: Parameters<RequirementUpdateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_update_args(&input);
        self.run_tool("ao.requirements.update", args, input.project_root).await
    }

    #[tool(
        name = "ao.requirements.delete",
        description = "Delete a requirement from AO. Purpose: Remove a requirement that is obsolete or created in error. Prerequisites: Requirement must exist. Example: {\"id\": \"REQ-099\"}. Sequencing: Use ao.requirements.get first to verify the requirement, and ensure linked tasks have been handled before deletion.",
        input_schema = ao_schema_for_type::<RequirementDeleteInput>()
    )]
    async fn ao_requirements_delete(
        &self,
        params: Parameters<RequirementDeleteInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_delete_args(input.id);
        self.run_tool("ao.requirements.delete", args, input.project_root).await
    }

    #[tool(
        name = "ao.requirements.refine",
        description = "Refine requirements in AO. Purpose: Improve one or more requirements, optionally with AI assistance, before planning or task derivation. Prerequisites: Requirements should exist. Example: {\"id\": [\"REQ-001\"], \"focus\": \"tighten acceptance criteria\"}. Sequencing: Use ao.requirements.get or ao.requirements.list to choose targets first, then ao.requirements.update or ao.task.create to apply follow-up work.",
        input_schema = ao_schema_for_type::<RequirementRefineInput>()
    )]
    async fn ao_requirements_refine(
        &self,
        params: Parameters<RequirementRefineInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_refine_args(&input);
        self.run_tool("ao.requirements.refine", args, input.project_root).await
    }
}
