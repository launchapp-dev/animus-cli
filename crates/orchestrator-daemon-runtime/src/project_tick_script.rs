use crate::{DaemonRuntimeOptions, ProjectTickAction, ProjectTickMode, ProjectTickPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickScript {
    actions: Vec<ProjectTickAction>,
}

impl ProjectTickScript {
    pub fn build(
        mode: ProjectTickMode,
        options: &DaemonRuntimeOptions,
        tick_plan: &ProjectTickPlan,
    ) -> Self {
        let mut actions = vec![ProjectTickAction::BootstrapFromVision];

        if options.ai_task_generation {
            actions.push(ProjectTickAction::EnsureAiGeneratedTasks);
        }
        if options.resume_interrupted {
            actions.push(ProjectTickAction::ResumeInterrupted);
        }

        actions.push(ProjectTickAction::RecoverOrphanedRunningWorkflows);

        if options.reconcile_stale {
            actions.push(ProjectTickAction::ReconcileStaleTasks);
        }

        actions.push(ProjectTickAction::ReconcileDependencyTasks);
        actions.push(ProjectTickAction::ReconcileMergeTasks);

        if mode == ProjectTickMode::Slim {
            actions.push(ProjectTickAction::ReconcileCompletedProcesses);
        }

        if tick_plan.should_prepare_ready_tasks {
            actions.push(ProjectTickAction::RetryFailedTaskWorkflows);
            actions.push(ProjectTickAction::PromoteBacklogTasksToReady);
        }

        if tick_plan.ready_dispatch_limit > 0 {
            actions.push(ProjectTickAction::DispatchReadyTasks {
                limit: tick_plan.ready_dispatch_limit,
            });
        }

        actions.push(ProjectTickAction::RefreshRuntimeBinaries);

        Self { actions }
    }

    pub fn actions(&self) -> &[ProjectTickAction] {
        &self.actions
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::ProjectTickScript;
    use crate::{DaemonRuntimeOptions, ProjectTickAction, ProjectTickMode, ProjectTickPlan};

    #[test]
    fn builds_full_tick_actions_in_runtime_order() {
        let options = DaemonRuntimeOptions {
            ai_task_generation: true,
            resume_interrupted: true,
            reconcile_stale: true,
            max_tasks_per_tick: 3,
            ..DaemonRuntimeOptions::default()
        };
        let tick_plan = ProjectTickPlan::build(
            &options,
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            2,
        );

        let script = ProjectTickScript::build(ProjectTickMode::Full, &options, &tick_plan);

        assert_eq!(
            script.actions(),
            &[
                ProjectTickAction::BootstrapFromVision,
                ProjectTickAction::EnsureAiGeneratedTasks,
                ProjectTickAction::ResumeInterrupted,
                ProjectTickAction::RecoverOrphanedRunningWorkflows,
                ProjectTickAction::ReconcileStaleTasks,
                ProjectTickAction::ReconcileDependencyTasks,
                ProjectTickAction::ReconcileMergeTasks,
                ProjectTickAction::RetryFailedTaskWorkflows,
                ProjectTickAction::PromoteBacklogTasksToReady,
                ProjectTickAction::DispatchReadyTasks { limit: 2 },
                ProjectTickAction::RefreshRuntimeBinaries,
            ]
        );
    }

    #[test]
    fn builds_slim_tick_actions_without_full_tick_phase_execution() {
        let options = DaemonRuntimeOptions {
            ai_task_generation: false,
            resume_interrupted: false,
            reconcile_stale: false,
            ..DaemonRuntimeOptions::default()
        };
        let tick_plan = ProjectTickPlan::build(
            &options,
            Some("09:00-17:00"),
            NaiveTime::from_hms_opt(8, 0, 0).expect("time should be valid"),
            true,
            0,
        );

        let script = ProjectTickScript::build(ProjectTickMode::Slim, &options, &tick_plan);

        assert_eq!(
            script.actions(),
            &[
                ProjectTickAction::BootstrapFromVision,
                ProjectTickAction::RecoverOrphanedRunningWorkflows,
                ProjectTickAction::ReconcileDependencyTasks,
                ProjectTickAction::ReconcileMergeTasks,
                ProjectTickAction::ReconcileCompletedProcesses,
                ProjectTickAction::RefreshRuntimeBinaries,
            ]
        );
    }
}
