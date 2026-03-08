use anyhow::Result;

use crate::{ProjectTickActionExecutor, ProjectTickExecutionOutcome, ProjectTickScript};

pub async fn execute_project_tick_script<E>(
    script: &ProjectTickScript,
    executor: &mut E,
) -> Result<ProjectTickExecutionOutcome>
where
    E: ProjectTickActionExecutor,
{
    let mut outcome = ProjectTickExecutionOutcome::default();

    for action in script.actions() {
        let effect = executor.execute_action(action).await?;
        outcome.apply_effect(effect);
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;

    use super::execute_project_tick_script;
    use crate::{
        DaemonRuntimeOptions, ProjectTickAction, ProjectTickActionEffect,
        ProjectTickActionExecutor, ProjectTickExecutionOutcome, ProjectTickMode, ProjectTickPlan,
        ProjectTickScript, ReadyTaskWorkflowStart, ReadyTaskWorkflowStartSummary,
        TaskSelectionSource,
    };

    struct FakeExecutor {
        calls: Vec<ProjectTickAction>,
    }

    #[async_trait(?Send)]
    impl ProjectTickActionExecutor for FakeExecutor {
        async fn execute_action(
            &mut self,
            action: &ProjectTickAction,
        ) -> Result<ProjectTickActionEffect> {
            self.calls.push(action.clone());
            Ok(match action {
                ProjectTickAction::DispatchReadyTasks { limit } => {
                    ProjectTickActionEffect::ReadyWorkflowStarts {
                        summary: ReadyTaskWorkflowStartSummary {
                            started: *limit,
                            started_workflows: vec![ReadyTaskWorkflowStart {
                                task_id: "TASK-1".to_string(),
                                workflow_id: "wf-1".to_string(),
                                selection_source: TaskSelectionSource::FallbackPicker,
                            }],
                        },
                    }
                }
                ProjectTickAction::ReconcileCompletedProcesses => {
                    ProjectTickActionEffect::ReconciledCompletedProcesses {
                        executed_workflow_phases: 0,
                        failed_workflow_phases: 0,
                    }
                }
            })
        }
    }

    #[tokio::test]
    async fn executes_actions_in_script_order_and_accumulates_outcome() {
        let options = DaemonRuntimeOptions {
            ai_task_generation: false,
            resume_interrupted: true,
            reconcile_stale: false,
            max_tasks_per_tick: 3,
            ..DaemonRuntimeOptions::default()
        };
        let plan = ProjectTickPlan::build(
            &options,
            None,
            chrono::NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            1,
        );
        let script = ProjectTickScript::build(ProjectTickMode::Full, &options, &plan);
        let mut executor = FakeExecutor { calls: Vec::new() };

        let outcome: ProjectTickExecutionOutcome =
            execute_project_tick_script(&script, &mut executor)
                .await
                .expect("script execution should succeed");

        assert_eq!(executor.calls, script.actions().to_vec());
        assert_eq!(outcome.cleaned_stale_workflows, 0);
        assert_eq!(outcome.resumed_workflows, 0);
        assert_eq!(outcome.ready_workflow_starts.started, 1);
        assert_eq!(outcome.executed_workflow_phases, 0);
    }
}
