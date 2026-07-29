use super::*;

#[tool_router(router = workflow_definition_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.workflow.phases.list",
        description = "List workflow phase definitions. Purpose: View configured phases available for workflows. Prerequisites: None. Example: {}. Sequencing: Use animus.workflow.phases.get for details on a specific phase, or animus.workflow.definitions.list to see how workflows are composed.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_phases_list(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.workflow_phases_list_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.workflow.phases.get",
        description = "Get a workflow phase definition. Purpose: View full details of a specific phase including runtime config. Prerequisites: Phase must exist (use animus.workflow.phases.list to find phase ids). Example: {\"phase\": \"implementation\"}. Sequencing: Use after animus.workflow.phases.list to inspect a specific phase.",
        input_schema = ao_schema_for_type::<WorkflowPhaseGetInput>()
    )]
    async fn ao_workflow_phases_get(
        &self,
        params: Parameters<WorkflowPhaseGetInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_phase_get_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.definitions.list",
        description = "List workflow definitions. Purpose: View available workflows and their phase composition. Prerequisites: None. Example: {}. Sequencing: Use animus.workflow.phases.list to see individual phase details, or animus.workflow.run with a workflow_ref to execute one.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_definitions_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_definitions_list_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.workflow.config.get",
        description = "Read effective workflow config. Purpose: View the resolved workflow configuration including phases, workflows, and settings. Prerequisites: None. Example: {}. Sequencing: Use animus.workflow.config.validate to check for issues, or animus.workflow.phases.list for phase details.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_config_get(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.workflow_config_get_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.workflow.config.validate",
        description = "Validate workflow config. Purpose: Check workflow configuration for shape errors and broken references. Prerequisites: None. Example: {}. Sequencing: Use animus.workflow.config.get to view the config first, or after modifying phases/workflows to verify consistency.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_workflow_config_validate(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_validate_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.workflow.config.set",
        description = "Replace the entire workflow config. Purpose: Persist a full RAW source WorkflowConfig through the installed writable config_source plugin (validates the post-pack-merge result before writing). Prerequisites: a writable config_source plugin must be installed. IMPORTANT: `config` must be the RAW SOURCE model, NOT the effective config from animus.workflow.config.get (that is post-pack-merge; feeding it back would bake pack-provided entities into the source). For single-entity edits prefer the entity verbs (agent-set / workflow-set), which read the raw source model and read-modify-write it for you. `file` remains a compatibility alternative and is mutually exclusive with `config`. Fails cleanly when the source is read-only (e.g. YAML). Example: {\"config\": {\"workflows\": [], \"phase_definitions\": {}, \"agent_profiles\": {}}}.",
        input_schema = ao_schema_for_type::<WorkflowConfigSetInput>()
    )]
    async fn ao_workflow_config_set(
        &self,
        params: Parameters<WorkflowConfigSetInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_set_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.config.agent-set",
        description = "Create or replace one agent definition. Purpose: Manage a single agent in the workflow config via read-modify-write (loads the current raw source, upserts the agent, validates, writes the full model). Prerequisites: a writable config_source plugin. `profile` is the structured agent overlay; legacy `input_json` is mutually exclusive. This is the DEFINITION-management verb and does NOT collide with runtime animus.agent.* tools. Example: {\"id\": \"reviewer\", \"profile\": {\"description\":\"...\",\"system_prompt\":\"...\"}}. Sequencing: inspect existing entities with animus.workflow.config.get (read-only; do NOT feed its effective output back into config.set).",
        input_schema = ao_schema_for_type::<WorkflowConfigAgentSetInput>()
    )]
    async fn ao_workflow_config_agent_set(
        &self,
        params: Parameters<WorkflowConfigAgentSetInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_agent_set_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.config.agent-remove",
        description = "Remove one agent definition. Purpose: Delete a single agent from the workflow config via read-modify-write (validates and writes the full model). Prerequisites: a writable config_source plugin; the agent must exist. Example: {\"id\": \"reviewer\"}. Sequencing: inspect agents via animus.workflow.config.get (read-only).",
        input_schema = ao_schema_for_type::<WorkflowConfigEntityRemoveInput>()
    )]
    async fn ao_workflow_config_agent_remove(
        &self,
        params: Parameters<WorkflowConfigEntityRemoveInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_agent_remove_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.config.workflow-set",
        description = "Create or replace one workflow definition. Purpose: Manage a single workflow in the config via read-modify-write (validates and writes the full model). Prerequisites: a writable config_source plugin; `workflow` must include an `id`. Legacy `input_json` is mutually exclusive. Example: {\"workflow\": {\"id\":\"ship\",\"name\":\"Ship\",\"phases\":[\"impl\"]}}. Sequencing: inspect existing entities with animus.workflow.config.get (read-only; do NOT feed its effective output back into config.set).",
        input_schema = ao_schema_for_type::<WorkflowConfigWorkflowSetInput>()
    )]
    async fn ao_workflow_config_workflow_set(
        &self,
        params: Parameters<WorkflowConfigWorkflowSetInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_workflow_set_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.config.workflow-remove",
        description = "Remove one workflow definition. Purpose: Delete a single workflow from the config via read-modify-write (validates and writes the full model). Prerequisites: a writable config_source plugin; the workflow must exist. Example: {\"id\": \"ship\"}. Sequencing: inspect via animus.workflow.config.get (read-only).",
        input_schema = ao_schema_for_type::<WorkflowConfigEntityRemoveInput>()
    )]
    async fn ao_workflow_config_workflow_remove(
        &self,
        params: Parameters<WorkflowConfigEntityRemoveInput>,
    ) -> Result<CallToolResult, McpError> {
        self.workflow_config_workflow_remove_inproc(params.0).await
    }
}
