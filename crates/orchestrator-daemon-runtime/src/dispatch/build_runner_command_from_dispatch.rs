use protocol::{McpRuntimeConfig, PhaseRoutingConfig, SubjectDispatch};

pub fn build_runner_command_from_dispatch(dispatch: &SubjectDispatch, project_root: &str) -> std::process::Command {
    build_runner_command(dispatch, project_root, None, None)
}

pub fn build_runner_command(
    dispatch: &SubjectDispatch,
    project_root: &str,
    phase_routing: Option<&PhaseRoutingConfig>,
    mcp_config: Option<&McpRuntimeConfig>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("ao-workflow-runner");
    cmd.arg("execute");

    match &dispatch.subject {
        protocol::orchestrator::WorkflowSubject::Task { id } => {
            cmd.arg("--task-id").arg(id);
        }
        protocol::orchestrator::WorkflowSubject::Requirement { id } => {
            cmd.arg("--requirement-id").arg(id);
        }
        protocol::orchestrator::WorkflowSubject::Custom { title, description } => {
            cmd.arg("--title").arg(title);
            cmd.arg("--description").arg(description);
        }
    }

    if let Some(input) = &dispatch.input {
        cmd.arg("--input-json").arg(input.to_string());
    }

    cmd.arg("--workflow-ref").arg(&dispatch.workflow_ref).arg("--project-root").arg(project_root);

    if let Some(routing) = phase_routing {
        if let Ok(json) = serde_json::to_string(routing) {
            cmd.arg("--phase-routing-json").arg(json);
        }
    }
    if let Some(mcp) = mcp_config {
        if let Ok(json) = serde_json::to_string(mcp) {
            cmd.arg("--mcp-config-json").arg(json);
        }
    }
    cmd
}

#[cfg(test)]
mod tests {
    use protocol::orchestrator::WorkflowSubject;
    use protocol::SubjectDispatch;
    use serde_json::json;

    use super::build_runner_command_from_dispatch;

    #[test]
    fn runner_command_uses_subject_workflow_ref_and_input_from_dispatch() {
        let dispatch = SubjectDispatch::for_custom(
            "schedule:nightly",
            "nightly dispatch",
            "ops",
            Some(json!({"nightly":true})),
            "schedule",
        );
        let command = build_runner_command_from_dispatch(&dispatch, "/tmp/project");
        let program = command.get_program().to_string_lossy().into_owned();
        let args = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();

        assert_eq!(program, "ao-workflow-runner");
        assert_eq!(
            args,
            vec![
                "execute",
                "--title",
                "schedule:nightly",
                "--description",
                "nightly dispatch",
                "--input-json",
                "{\"nightly\":true}",
                "--workflow-ref",
                "ops",
                "--project-root",
                "/tmp/project",
            ]
        );
        assert_eq!(
            &dispatch.subject,
            &WorkflowSubject::Custom {
                title: "schedule:nightly".to_string(),
                description: "nightly dispatch".to_string(),
            }
        );
    }
}
