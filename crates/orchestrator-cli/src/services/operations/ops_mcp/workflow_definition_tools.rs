use super::*;

#[tool_router(router = workflow_definition_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.workflow.phases.list",
        description = "List workflow phase definitions. Purpose: View configured phases available for workflows. Prerequisites: None. Example: {}. Sequencing: Use ao.workflow.phases.get for details on a specific phase, or ao.workflow.definitions.list to see how workflows are composed.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_phases_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.workflow.phases.list",
            vec![
                "workflow".to_string(),
                "phases".to_string(),
                "list".to_string(),
            ],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.workflow.phases.get",
        description = "Get a workflow phase definition. Purpose: View full details of a specific phase including runtime config. Prerequisites: Phase must exist (use ao.workflow.phases.list to find phase ids). Example: {\"phase\": \"implementation\"}. Sequencing: Use after ao.workflow.phases.list to inspect a specific phase.",
        input_schema = ao_schema_for_type::<WorkflowPhaseGetInput>()
    )]
    async fn ao_workflow_phases_get(
        &self,
        params: Parameters<WorkflowPhaseGetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "phases".to_string(),
            "get".to_string(),
            "--phase".to_string(),
            input.phase,
        ];
        self.run_tool("ao.workflow.phases.get", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.definitions.list",
        description = "List workflow definitions. Purpose: View available workflows and their phase composition. Prerequisites: None. Example: {}. Sequencing: Use ao.workflow.phases.list to see individual phase details, or ao.workflow.run with a workflow_ref to execute one.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_definitions_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.workflow.definitions.list",
            vec![
                "workflow".to_string(),
                "definitions".to_string(),
                "list".to_string(),
            ],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.workflow.config.get",
        description = "Read effective workflow config. Purpose: View the resolved workflow configuration including phases, workflows, and settings. Prerequisites: None. Example: {}. Sequencing: Use ao.workflow.config.validate to check for issues, or ao.workflow.phases.list for phase details.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_config_get(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.workflow.config.get",
            vec![
                "workflow".to_string(),
                "config".to_string(),
                "get".to_string(),
            ],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.workflow.config.validate",
        description = "Validate workflow config. Purpose: Check workflow configuration for shape errors and broken references. Prerequisites: None. Example: {}. Sequencing: Use ao.workflow.config.get to view the config first, or after modifying phases/workflows to verify consistency.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_config_validate(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.workflow.config.validate",
            vec![
                "workflow".to_string(),
                "config".to_string(),
                "validate".to_string(),
            ],
            params.0.project_root,
        )
        .await
    }
}
