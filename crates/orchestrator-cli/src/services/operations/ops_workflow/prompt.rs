use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    ensure_workflow_config_compiled, load_workflow_config, services::ServiceHub,
    resolve_phase_plan_for_workflow_ref, workflow_ref_for_task, OrchestratorWorkflow,
    WorkflowDecisionAction, WorkflowSubject, STANDARD_WORKFLOW_REF,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{print_value, WorkflowPromptRenderArgs};

use ::workflow_runner_v2::{
    ensure_execution_cwd, render_phase_prompt, PhasePromptInputs, PhaseRenderParams,
    RenderedPhasePrompt,
};

#[derive(Debug, Clone, Serialize)]
struct PromptRenderOutput {
    workflow_ref: String,
    workflow_id_is_preview: bool,
    rendered: RenderedPhasePrompt,
}

#[derive(Debug, Clone)]
struct ResolvedPromptContext {
    workflow_id: String,
    workflow_id_is_preview: bool,
    workflow_ref: String,
    subject: WorkflowSubject,
    subject_title: String,
    subject_description: String,
    execution_cwd: String,
    workflow_vars: std::collections::HashMap<String, String>,
    input: Option<Value>,
}

pub(crate) async fn handle_workflow_prompt_render(
    args: WorkflowPromptRenderArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    if args.workflow_id.is_some() {
        if !args.vars.is_empty() {
            return Err(anyhow!(
                "--var cannot be used with --workflow-id; persisted workflow vars are authoritative for existing workflows"
            ));
        }
        if args.input_json.is_some() {
            return Err(anyhow!(
                "--input-json cannot be used with --workflow-id; existing workflow input is rendered from persisted state"
            ));
        }
        if args.rework_context.is_some() {
            return Err(anyhow!(
                "--rework-context cannot be used with --workflow-id; existing workflow render uses persisted decision history"
            ));
        }
    }

    let phase_renders = if let Some(workflow_id) = args.workflow_id.as_deref() {
        render_existing_workflow_prompts(workflow_id, &args, hub, project_root).await?
    } else {
        render_ad_hoc_prompts(&args, hub, project_root).await?
    };

    if json {
        print_value(phase_renders, true)
    } else {
        print_human_render_output(&phase_renders);
        Ok(())
    }
}

async fn render_existing_workflow_prompts(
    workflow_id: &str,
    args: &WorkflowPromptRenderArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<Vec<PromptRenderOutput>> {
    let workflow = hub.workflows().get(workflow_id).await?;
    let workflow_ref = effective_existing_workflow_ref(project_root, &workflow)?;
    let phase_ids = select_existing_workflow_phases(&workflow, args)?;
    let context = resolve_existing_workflow_context(&workflow, workflow_ref, hub, project_root).await?;

    Ok(phase_ids
        .into_iter()
        .map(|phase_id| {
            let inputs = PhasePromptInputs {
                rework_context: existing_workflow_rework_context(&workflow, &phase_id),
                pipeline_vars: context.workflow_vars.clone(),
                dispatch_input: context.input.as_ref().map(Value::to_string),
                schedule_input: schedule_prompt_input(&context.subject, context.input.as_ref()),
            };
            let rendered = render_phase_prompt(
                &PhaseRenderParams {
                    project_root,
                    execution_cwd: &context.execution_cwd,
                    workflow_id: &context.workflow_id,
                    subject_id: context.subject.id(),
                    subject_title: &context.subject_title,
                    subject_description: &context.subject_description,
                    phase_id: &phase_id,
                },
                inputs,
            );
            PromptRenderOutput {
                workflow_ref: context.workflow_ref.clone(),
                workflow_id_is_preview: context.workflow_id_is_preview,
                rendered,
            }
        })
        .collect())
}

async fn render_ad_hoc_prompts(
    args: &WorkflowPromptRenderArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<Vec<PromptRenderOutput>> {
    let vars = super::parse_workflow_vars(&args.vars)?;
    let input = args
        .input_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid --input-json payload for workflow prompt render")?;
    let context = resolve_ad_hoc_context(args, vars, input, hub, project_root).await?;
    let phase_ids = select_ad_hoc_phases(project_root, &context.workflow_ref, args)?;

    Ok(phase_ids
        .into_iter()
        .map(|phase_id| {
            let inputs = PhasePromptInputs {
                rework_context: args.rework_context.clone(),
                pipeline_vars: context.workflow_vars.clone(),
                dispatch_input: context.input.as_ref().map(Value::to_string),
                schedule_input: schedule_prompt_input(&context.subject, context.input.as_ref()),
            };
            let rendered = render_phase_prompt(
                &PhaseRenderParams {
                    project_root,
                    execution_cwd: &context.execution_cwd,
                    workflow_id: &context.workflow_id,
                    subject_id: context.subject.id(),
                    subject_title: &context.subject_title,
                    subject_description: &context.subject_description,
                    phase_id: &phase_id,
                },
                inputs,
            );
            PromptRenderOutput {
                workflow_ref: context.workflow_ref.clone(),
                workflow_id_is_preview: context.workflow_id_is_preview,
                rendered,
            }
        })
        .collect())
}

fn effective_existing_workflow_ref(
    project_root: &str,
    workflow: &OrchestratorWorkflow,
) -> Result<String> {
    ensure_workflow_config_compiled(Path::new(project_root))?;
    let workflow_config = load_workflow_config(Path::new(project_root))?;
    Ok(workflow
        .workflow_ref
        .clone()
        .unwrap_or_else(|| workflow_config.default_workflow_ref.clone()))
}

fn select_existing_workflow_phases(
    workflow: &OrchestratorWorkflow,
    args: &WorkflowPromptRenderArgs,
) -> Result<Vec<String>> {
    if args.all_phases {
        return Ok(workflow.phases.iter().map(|phase| phase.phase_id.clone()).collect());
    }
    if let Some(phase) = args.phase.as_ref() {
        return Ok(vec![phase.clone()]);
    }
    workflow
        .current_phase
        .clone()
        .or_else(|| {
            workflow
                .phases
                .get(workflow.current_phase_index)
                .map(|phase| phase.phase_id.clone())
        })
        .map(|phase| vec![phase])
        .ok_or_else(|| anyhow!("workflow '{}' has no current phase to render", workflow.id))
}

async fn resolve_existing_workflow_context(
    workflow: &OrchestratorWorkflow,
    workflow_ref: String,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<ResolvedPromptContext> {
    let resolved = hub
        .subject_resolver()
        .resolve_subject_context(&workflow.subject, None, None)
        .await
        .with_context(|| {
            format!(
                "failed to resolve subject context for workflow '{}'",
                workflow.id
            )
        })?;
    let execution_cwd = ensure_execution_cwd(hub, project_root, resolved.task.as_ref()).await?;

    Ok(ResolvedPromptContext {
        workflow_id: workflow.id.clone(),
        workflow_id_is_preview: false,
        workflow_ref,
        subject: workflow.subject.clone(),
        subject_title: resolved.subject_title,
        subject_description: resolved.subject_description,
        execution_cwd,
        workflow_vars: workflow.vars.clone(),
        input: workflow.input.clone(),
    })
}

async fn resolve_ad_hoc_context(
    args: &WorkflowPromptRenderArgs,
    vars: std::collections::HashMap<String, String>,
    input: Option<Value>,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<ResolvedPromptContext> {
    let (subject, workflow_ref, fallback_title, fallback_description) =
        match (&args.task_id, &args.requirement_id, &args.title) {
            (Some(task_id), None, None) => {
                let task = hub.tasks().get(task_id).await?;
                let workflow_ref = args
                    .workflow_ref
                    .clone()
                    .unwrap_or_else(|| workflow_ref_for_task(&task));
                (
                    WorkflowSubject::Task {
                        id: task.id.clone(),
                    },
                    workflow_ref,
                    Some(task.title),
                    Some(task.description),
                )
            }
            (None, Some(requirement_id), None) => {
                hub.planning().get_requirement(requirement_id).await?;
                (
                    WorkflowSubject::Requirement {
                        id: requirement_id.clone(),
                    },
                    args.workflow_ref
                        .clone()
                        .unwrap_or(super::resolve_requirement_workflow_ref(project_root)?),
                    None,
                    None,
                )
            }
            (None, None, Some(title)) => (
                WorkflowSubject::Custom {
                    title: title.clone(),
                    description: args.description.clone().unwrap_or_default(),
                },
                args.workflow_ref
                    .clone()
                    .unwrap_or_else(|| STANDARD_WORKFLOW_REF.to_string()),
                Some(title.clone()),
                Some(args.description.clone().unwrap_or_default()),
            ),
            (None, None, None) => {
                return Err(anyhow!(
                    "one of --workflow-id, --task-id, --requirement-id, or --title must be provided"
                ));
            }
            _ => {
                return Err(anyhow!(
                    "--task-id, --requirement-id, and --title are mutually exclusive"
                ));
            }
        };

    let resolved = hub
        .subject_resolver()
        .resolve_subject_context(
            &subject,
            fallback_title.as_deref(),
            fallback_description.as_deref(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to resolve subject context for ad-hoc subject '{}'",
                subject.id()
            )
        })?;
    let execution_cwd = ensure_execution_cwd(hub, project_root, resolved.task.as_ref()).await?;

    Ok(ResolvedPromptContext {
        workflow_id: Uuid::new_v4().to_string(),
        workflow_id_is_preview: true,
        workflow_ref,
        subject,
        subject_title: resolved.subject_title,
        subject_description: resolved.subject_description,
        execution_cwd,
        workflow_vars: vars,
        input,
    })
}

fn select_ad_hoc_phases(
    project_root: &str,
    workflow_ref: &str,
    args: &WorkflowPromptRenderArgs,
) -> Result<Vec<String>> {
    if args.all_phases {
        return resolve_phase_plan_for_workflow_ref(
            Some(Path::new(project_root)),
            Some(workflow_ref),
        );
    }

    args.phase
        .clone()
        .map(|phase| vec![phase])
        .ok_or_else(|| anyhow!("--phase is required for ad-hoc prompt rendering unless --all-phases is set"))
}

fn existing_workflow_rework_context(
    workflow: &OrchestratorWorkflow,
    phase_id: &str,
) -> Option<String> {
    workflow
        .decision_history
        .iter()
        .rev()
        .find(|record| {
            record.decision == WorkflowDecisionAction::Rework
                && (record.target_phase.as_deref() == Some(phase_id)
                    || (record.target_phase.is_none() && record.phase_id == phase_id))
        })
        .map(|record| record.reason.clone())
}

fn schedule_prompt_input(subject: &WorkflowSubject, input: Option<&Value>) -> Option<String> {
    if subject.id().starts_with("schedule:") {
        return input.map(Value::to_string);
    }
    None
}

fn print_human_render_output(outputs: &[PromptRenderOutput]) {
    for (idx, output) in outputs.iter().enumerate() {
        if idx > 0 {
            println!();
            println!("============================================================");
            println!();
        }

        let rendered = &output.rendered;
        println!("Phase: {}", rendered.phase_id);
        println!(
            "Workflow: {}{}",
            rendered.workflow_id,
            if output.workflow_id_is_preview {
                " (preview)"
            } else {
                ""
            }
        );
        println!("Workflow Ref: {}", output.workflow_ref);
        println!("Subject: {}", rendered.subject_id);
        println!("Subject Title: {}", rendered.subject_title);
        println!("Execution Cwd: {}", rendered.execution_cwd);

        if !rendered.inputs.pipeline_vars.is_empty() {
            println!();
            println!("Pipeline Vars:");
            let mut keys = rendered
                .inputs
                .pipeline_vars
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(value) = rendered.inputs.pipeline_vars.get(&key) {
                    println!("{key}={value}");
                }
            }
        }

        if let Some(dispatch_input) = rendered.inputs.dispatch_input.as_deref() {
            println!();
            println!("Dispatch Input:");
            println!("{dispatch_input}");
        }

        if let Some(schedule_input) = rendered.inputs.schedule_input.as_deref() {
            println!();
            println!("Schedule Input:");
            println!("{schedule_input}");
        }

        if let Some(rework_context) = rendered.inputs.rework_context.as_deref() {
            println!();
            println!("Rework Context:");
            println!("{rework_context}");
        }

        print_named_section("System Prompt", rendered.system_prompt.as_deref());
        print_named_section("Phase Directive", Some(rendered.phase_directive.as_str()));
        print_named_section("Action Rule", Some(rendered.phase_action_rule.as_str()));
        print_named_section("Safety Rules", Some(rendered.phase_safety_rules.as_str()));
        print_named_section("Decision Rule", Some(rendered.phase_decision_rule.as_str()));
        print_named_section("Result Rule", Some(rendered.structured_result_rule.as_str()));
        print_named_section(
            "Pipeline Context",
            Some(rendered.pipeline_context.as_str()),
        );
        print_named_section(
            "Prior Phase Outputs",
            Some(rendered.prior_phase_outputs.as_str()),
        );
        print_named_section("Final Prompt", Some(rendered.final_prompt.as_str()));
    }
}

fn print_named_section(title: &str, content: Option<&str>) {
    let Some(content) = content.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    println!();
    println!("{title}:");
    println!("{content}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, write_agent_runtime_config,
        write_workflow_config, InMemoryServiceHub, WorkflowRunInput,
    };
    use std::collections::HashMap;

    fn write_prompt_test_config(project_root: &Path) {
        let mut workflow_config = builtin_workflow_config();
        let default_agent = builtin_agent_runtime_config()
            .agent_profile("default")
            .expect("default agent profile")
            .clone();
        workflow_config
            .agent_profiles
            .insert("default".to_string(), default_agent);
        let phase = workflow_config
            .phase_definitions
            .entry("implementation".to_string())
            .or_insert(orchestrator_core::PhaseExecutionDefinition {
                mode: orchestrator_core::PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
            });
        phase.directive = Some("Implement {{release_name}} safely.".to_string());
        phase.system_prompt = Some("System guidance for {{release_name}}.".to_string());
        write_workflow_config(project_root, &workflow_config).expect("write workflow config");
        write_agent_runtime_config(project_root, &builtin_agent_runtime_config())
            .expect("write runtime config");
    }

    fn base_args() -> WorkflowPromptRenderArgs {
        WorkflowPromptRenderArgs {
            workflow_id: None,
            task_id: None,
            requirement_id: None,
            title: None,
            description: None,
            workflow_ref: None,
            phase: None,
            all_phases: false,
            input_json: None,
            rework_context: None,
            vars: Vec::new(),
        }
    }

    #[tokio::test]
    async fn render_ad_hoc_prompts_includes_explicit_inputs_and_expands_vars() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prompt_test_config(temp.path());
        let hub = Arc::new(InMemoryServiceHub::new());
        let mut args = base_args();
        args.title = Some("Release Preview".to_string());
        args.description = Some("Inspect rendered prompt".to_string());
        args.phase = Some("implementation".to_string());
        args.input_json = Some("{\"ticket\":\"REL-9\"}".to_string());
        args.rework_context = Some("Fix the remaining release issues.".to_string());
        args.vars = vec!["release_name=Mercury".to_string()];

        let outputs = render_ad_hoc_prompts(
            &args,
            hub,
            temp.path().to_string_lossy().as_ref(),
        )
        .await
        .expect("ad-hoc prompt render should succeed");

        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].workflow_id_is_preview);
        assert_eq!(
            outputs[0]
                .rendered
                .inputs
                .pipeline_vars
                .get("release_name")
                .map(String::as_str),
            Some("Mercury")
        );
        assert_eq!(
            outputs[0].rendered.inputs.dispatch_input.as_deref(),
            Some("{\"ticket\":\"REL-9\"}")
        );
        assert_eq!(
            outputs[0].rendered.inputs.rework_context.as_deref(),
            Some("Fix the remaining release issues.")
        );
        assert!(
            outputs[0]
                .rendered
                .final_prompt
                .contains("Implement Mercury safely.")
        );
        assert!(
            outputs[0]
                .rendered
                .final_prompt
                .contains("System guidance for Mercury.")
        );
        let json = serde_json::to_value(&outputs).expect("prompt outputs should serialize");
        assert_eq!(
            json[0]["rendered"]["final_prompt"].as_str(),
            Some(outputs[0].rendered.final_prompt.as_str())
        );
    }

    #[tokio::test]
    async fn render_existing_workflow_prompts_uses_persisted_vars_and_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prompt_test_config(temp.path());
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow = hub
            .workflows()
            .run(
                WorkflowRunInput::for_custom(
                    "Release Preview".to_string(),
                    "Inspect rendered prompt".to_string(),
                    None,
                )
                .with_input(Some(serde_json::json!({"ticket":"WF-7"})))
                .with_vars(HashMap::from([(
                    "release_name".to_string(),
                    "Mercury".to_string(),
                )])),
            )
            .await
            .expect("workflow should bootstrap");
        let mut args = base_args();
        args.workflow_id = Some(workflow.id.clone());
        args.phase = Some("implementation".to_string());

        let outputs = render_existing_workflow_prompts(
            &workflow.id,
            &args,
            hub,
            temp.path().to_string_lossy().as_ref(),
        )
        .await
        .expect("existing workflow prompt render should succeed");

        assert_eq!(outputs.len(), 1);
        assert!(!outputs[0].workflow_id_is_preview);
        assert_eq!(
            outputs[0]
                .rendered
                .inputs
                .pipeline_vars
                .get("release_name")
                .map(String::as_str),
            Some("Mercury")
        );
        assert_eq!(
            outputs[0].rendered.inputs.dispatch_input.as_deref(),
            Some("{\"ticket\":\"WF-7\"}")
        );
        assert!(
            outputs[0]
                .rendered
                .final_prompt
                .contains("Implement Mercury safely.")
        );
    }

    #[tokio::test]
    async fn handle_workflow_prompt_render_rejects_ad_hoc_vars_for_existing_workflow() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prompt_test_config(temp.path());
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_custom(
                "Release Preview".to_string(),
                "Inspect rendered prompt".to_string(),
                None,
            ))
            .await
            .expect("workflow should bootstrap");
        let mut args = base_args();
        args.workflow_id = Some(workflow.id);
        args.vars = vec!["release_name=Mercury".to_string()];

        let error = handle_workflow_prompt_render(
            args,
            hub,
            temp.path().to_string_lossy().as_ref(),
            true,
        )
        .await
        .expect_err("existing workflow render should reject ad-hoc vars");
        assert!(error.to_string().contains("--var cannot be used with --workflow-id"));
    }

    #[test]
    fn schedule_prompt_input_only_applies_to_schedule_subjects() {
        let input = serde_json::json!({"window":"nightly"});
        assert_eq!(
            schedule_prompt_input(
                &WorkflowSubject::Task {
                    id: "TASK-1".to_string(),
                },
                Some(&input),
            ),
            None
        );
        assert_eq!(
            schedule_prompt_input(
                &WorkflowSubject::Custom {
                    title: "schedule:nightly".to_string(),
                    description: String::new(),
                },
                Some(&input),
            ),
            Some("{\"window\":\"nightly\"}".to_string())
        );
    }
}
