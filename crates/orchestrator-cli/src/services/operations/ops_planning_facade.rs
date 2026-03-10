use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::{
    services::ServiceHub, RequirementsExecutionInput, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};

use super::ops_planning::{
    RequirementsDraftInputPayload, RequirementsRefineInputPayload, VisionDraftInputPayload,
    VisionRefineInputPayload,
};
use crate::{
    IdArgs, PlanningCommand, PlanningRequirementsCommand, RequirementsCommand,
    RequirementsExecuteArgs, VisionCommand, WorkflowCommand, WorkflowExecuteArgs,
};

const BUILTIN_VISION_DRAFT_WORKFLOW_REF: &str = "builtin/vision-draft";
const BUILTIN_VISION_REFINE_WORKFLOW_REF: &str = "builtin/vision-refine";
const BUILTIN_REQUIREMENTS_DRAFT_WORKFLOW_REF: &str = "builtin/requirements-draft";
const BUILTIN_REQUIREMENTS_REFINE_WORKFLOW_REF: &str = "builtin/requirements-refine";
const BUILTIN_REQUIREMENTS_EXECUTE_WORKFLOW_REF: &str = "builtin/requirements-execute";

pub(crate) async fn handle_planning(
    command: PlanningCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        PlanningCommand::Vision { command } => match command {
            VisionCommand::Draft(args) => {
                let input_json = match args.input_json {
                    Some(raw) => Some(raw),
                    None => Some(serde_json::to_string(&VisionDraftInputPayload {
                        project_name: args.project_name,
                        problem_statement: args.problem,
                        target_users: args.target_user,
                        goals: args.goal,
                        constraints: args.constraint,
                        value_proposition: args.value_proposition,
                        use_ai_complexity: args.use_ai_complexity,
                        tool: args.tool,
                        model: args.model,
                        timeout_secs: args.timeout_secs,
                        start_runner: args.start_runner,
                        allow_heuristic_fallback: args.allow_heuristic_fallback,
                    })?),
                };
                run_facade_workflow(
                    WorkflowExecuteArgs {
                        task_id: None,
                        requirement_id: None,
                        title: Some("planning:vision-draft".to_string()),
                        description: Some("Draft the project vision.".to_string()),
                        workflow_ref: Some(BUILTIN_VISION_DRAFT_WORKFLOW_REF.to_string()),
                        phase: None,
                        model: None,
                        tool: None,
                        phase_timeout_secs: None,
                        input_json,
                        quiet: false,
                        verbose: false,
                        vars: Vec::new(),
                    },
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            VisionCommand::Refine(args) => {
                let input_json = match args.input_json {
                    Some(raw) => Some(raw),
                    None => Some(serde_json::to_string(&VisionRefineInputPayload {
                        focus: args.focus,
                        use_ai: args.use_ai,
                        tool: args.tool,
                        model: args.model,
                        timeout_secs: args.timeout_secs,
                        start_runner: args.start_runner,
                        allow_heuristic_fallback: args.allow_heuristic_fallback,
                        preserve_core: args.preserve_core,
                    })?),
                };
                run_facade_workflow(
                    WorkflowExecuteArgs {
                        task_id: None,
                        requirement_id: None,
                        title: Some("planning:vision-refine".to_string()),
                        description: Some("Refine the project vision.".to_string()),
                        workflow_ref: Some(BUILTIN_VISION_REFINE_WORKFLOW_REF.to_string()),
                        phase: None,
                        model: None,
                        tool: None,
                        phase_timeout_secs: None,
                        input_json,
                        quiet: false,
                        verbose: false,
                        vars: Vec::new(),
                    },
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            VisionCommand::Get => {
                super::handle_vision(VisionCommand::Get, hub.clone(), project_root, json).await
            }
        },
        PlanningCommand::Requirements { command } => match command {
            PlanningRequirementsCommand::Draft(args) => {
                let input_json = match args.input_json {
                    Some(raw) => Some(raw),
                    None => Some(serde_json::to_string(&RequirementsDraftInputPayload {
                        include_codebase_scan: args.include_codebase_scan,
                        append_only: args.append_only,
                        max_requirements: args.max_requirements,
                        draft_strategy: args.draft_strategy,
                        po_parallelism: args.po_parallelism,
                        quality_repair_attempts: args.quality_repair_attempts,
                        allow_heuristic_complexity: args.allow_heuristic_complexity,
                        tool: args.tool,
                        model: args.model,
                        timeout_secs: args.timeout_secs,
                        start_runner: args.start_runner,
                    })?),
                };
                run_facade_workflow(
                    WorkflowExecuteArgs {
                        task_id: None,
                        requirement_id: None,
                        title: Some("planning:requirements-draft".to_string()),
                        description: Some("Draft project requirements.".to_string()),
                        workflow_ref: Some(BUILTIN_REQUIREMENTS_DRAFT_WORKFLOW_REF.to_string()),
                        phase: None,
                        model: None,
                        tool: None,
                        phase_timeout_secs: None,
                        input_json,
                        quiet: false,
                        verbose: false,
                        vars: Vec::new(),
                    },
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            PlanningRequirementsCommand::Refine(args) => {
                let input_json = match args.input_json {
                    Some(raw) => Some(raw),
                    None => Some(serde_json::to_string(&RequirementsRefineInputPayload {
                        requirement_ids: args.requirement_ids,
                        focus: args.focus,
                        use_ai: args.use_ai,
                        tool: args.tool,
                        model: args.model,
                        timeout_secs: args.timeout_secs,
                        start_runner: args.start_runner,
                    })?),
                };
                run_facade_workflow(
                    WorkflowExecuteArgs {
                        task_id: None,
                        requirement_id: None,
                        title: Some("planning:requirements-refine".to_string()),
                        description: Some("Refine project requirements.".to_string()),
                        workflow_ref: Some(BUILTIN_REQUIREMENTS_REFINE_WORKFLOW_REF.to_string()),
                        phase: None,
                        model: None,
                        tool: None,
                        phase_timeout_secs: None,
                        input_json,
                        quiet: false,
                        verbose: false,
                        vars: Vec::new(),
                    },
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            PlanningRequirementsCommand::Execute(args) => {
                run_facade_workflow(
                    build_planning_requirements_execute_command(args)?,
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            PlanningRequirementsCommand::List => {
                super::handle_requirements(
                    RequirementsCommand::List,
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
            PlanningRequirementsCommand::Get(IdArgs { id }) => {
                super::handle_requirements(
                    RequirementsCommand::Get(IdArgs { id }),
                    hub.clone(),
                    project_root,
                    json,
                )
                .await
            }
        },
    }
}

async fn run_facade_workflow(
    args: WorkflowExecuteArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    super::handle_workflow(WorkflowCommand::Execute(args), hub, project_root, json).await
}

fn build_planning_requirements_execute_command(
    args: RequirementsExecuteArgs,
) -> Result<WorkflowExecuteArgs> {
    let mut requirement_ids = args
        .requirement_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();

    if requirement_ids.is_empty() {
        return Err(anyhow!(
            "missing --id value for `planning requirements execute`; pass a requirement id to delegate to `workflow execute --requirement-id`"
        ));
    }

    if requirement_ids.len() > 1 {
        return Err(anyhow!(
            "`planning requirements execute` currently supports a single --id because it delegates to `workflow execute --requirement-id`"
        ));
    }

    let requirement_id = requirement_ids.remove(0);
    let input_json = match args.input_json {
        Some(raw) => Some(raw),
        None => Some(serde_json::to_string(&RequirementsExecutionInput {
            requirement_ids: vec![requirement_id.clone()],
            start_workflows: args.start_workflows,
            workflow_ref: args.workflow_ref,
            include_wont: args.include_wont,
        })?),
    };

    let workflow_ref = if args.start_workflows {
        BUILTIN_REQUIREMENTS_EXECUTE_WORKFLOW_REF.to_string()
    } else {
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string()
    };

    Ok(WorkflowExecuteArgs {
        task_id: None,
        requirement_id: Some(requirement_id),
        title: None,
        description: None,
        workflow_ref: Some(workflow_ref),
        phase: None,
        model: None,
        tool: None,
        phase_timeout_secs: None,
        input_json,
        quiet: false,
        verbose: false,
        vars: Vec::new(),
    })
}
