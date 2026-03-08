use crate::{DaemonRuntimeOptions, ProjectTickAction, ProjectTickMode, ProjectTickPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickScript {
    actions: Vec<ProjectTickAction>,
}

impl ProjectTickScript {
    pub fn build(
        mode: ProjectTickMode,
        _options: &DaemonRuntimeOptions,
        tick_plan: &ProjectTickPlan,
    ) -> Self {
        let mut actions = Vec::new();

        if mode == ProjectTickMode::Slim {
            actions.push(ProjectTickAction::ReconcileCompletedProcesses);
        }

        if tick_plan.should_prepare_ready_tasks {
            actions.push(ProjectTickAction::DispatchReadyTasks {
                limit: tick_plan.ready_dispatch_limit,
            });
        }

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
            &[ProjectTickAction::DispatchReadyTasks { limit: 2 },]
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
            &[ProjectTickAction::ReconcileCompletedProcesses,]
        );
    }
}
