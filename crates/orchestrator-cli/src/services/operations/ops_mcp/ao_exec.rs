#![allow(dead_code)]

use animus_actor::Actor;
use anyhow::{bail, Context, Result};

pub(super) fn build_ao_args(
    project_root: &str,
    requested_args: &[String],
    pinned_actor: Option<&Actor>,
) -> Result<Vec<String>> {
    let mut args = vec!["--json".to_string(), "--project-root".to_string(), project_root.to_string()];
    args.extend(requested_args.iter().cloned());

    let Some(actor) = pinned_actor else {
        return Ok(args);
    };
    if requested_args.iter().any(|arg| arg == "--actor-json" || arg.starts_with("--actor-json=")) {
        bail!("MCP child command attempted to override the server-pinned actor");
    }
    if !command_accepts_actor(requested_args) {
        bail!(
            "actor-bound MCP command is not actor-aware and was denied before execution: {}",
            requested_args.join(" ")
        );
    }

    args.push("--actor-json".to_string());
    args.push(serde_json::to_string(actor).context("failed to serialize server-pinned MCP actor")?);
    Ok(args)
}

pub(super) fn command_accepts_actor(requested_args: &[String]) -> bool {
    match requested_args {
        [scope, command, ..] if scope == "workflow" && command == "run" => true,
        [scope, command, ..] if scope == "workflow" && command == "list" => true,
        [scope, command, ..] if scope == "workflow" && command == "get" => true,
        [scope, group, command, ..]
            if scope == "workflow" && group == "config" && matches!(command.as_str(), "get" | "validate") =>
        {
            true
        }
        [scope, command, ..] if scope == "chat" && command == "send" => true,
        [scope, command, ..] if scope == "output" && matches!(command.as_str(), "read" | "phase-outputs") => true,
        [scope, command, ..]
            if scope == "subject"
                && matches!(
                    command.as_str(),
                    "list" | "get" | "create" | "update" | "batch-create" | "batch-update" | "status" | "delete"
                ) =>
        {
            true
        }
        _ => false,
    }

    // TASK-970 follow-up: workflow config mutation commands are deliberately
    // absent. Their config_source write protocol carries no Actor and
    // read_modify_write currently resolves the plugin with actor=None. Passing
    // a flag there would either break clap or falsely claim tenant isolation.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        Actor {
            user_id: "alice".to_string(),
            claims: vec!["workflow:run".to_string()],
            tenant_id: Some("tenant-7".to_string()),
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn forwarded_actor(args: &[String]) -> Option<Actor> {
        let index = args.iter().position(|arg| arg == "--actor-json")?;
        serde_json::from_str(args.get(index + 1)?).ok()
    }

    #[test]
    fn forwards_pinned_actor_to_workflow_run_for_common_and_batch_dispatch() {
        let requested = strings(&["workflow", "run", "--subject-id", "TASK-970"]);
        let args = build_ao_args("/project", &requested, Some(&actor())).expect("actor-aware args should build");

        assert_eq!(forwarded_actor(&args), Some(actor()));
        assert_eq!(&args[..3], strings(&["--json", "--project-root", "/project"]));
        assert_eq!(&args[3..3 + requested.len()], requested);
    }

    #[test]
    fn forwards_pinned_actor_to_actor_aware_config_reads() {
        for command in ["get", "validate"] {
            let requested = strings(&["workflow", "config", command]);
            let args = build_ao_args("/project", &requested, Some(&actor())).expect("actor-aware args should build");
            assert_eq!(forwarded_actor(&args), Some(actor()));
        }
    }

    #[test]
    fn forwards_pinned_actor_to_portal_workflow_and_output_reads() {
        for requested in [
            strings(&["workflow", "list"]),
            strings(&["workflow", "get", "--id", "wf-1"]),
            strings(&["output", "read", "--run-id", "run-1"]),
            strings(&["output", "phase-outputs", "--workflow-id", "wf-1"]),
            strings(&["subject", "list", "--kind", "task"]),
            strings(&["subject", "get", "--kind", "task", "--id", "TASK-1"]),
            strings(&["subject", "create", "--kind", "task", "--title", "Owned"]),
            strings(&["subject", "status", "--kind", "task", "--id", "TASK-1", "--status", "done"]),
        ] {
            let args = build_ao_args("/project", &requested, Some(&actor())).expect("owned read should build");
            assert_eq!(forwarded_actor(&args), Some(actor()));
        }
    }

    #[test]
    fn leaves_legitimately_actorless_tools_unchanged_on_a_global_server() {
        for requested in [
            strings(&["subject", "list", "--kind", "task"]),
            strings(&["workflow", "config", "agent-set", "--id", "reviewer"]),
        ] {
            let args = build_ao_args("/project", &requested, None).expect("global actorless args should build");
            assert_eq!(args, [strings(&["--json", "--project-root", "/project"]), requested].concat());
        }
    }

    #[test]
    fn denies_unscoped_queue_and_config_paths_for_distinct_actor_tenants() {
        let actors = [
            Actor { user_id: "alice".to_string(), claims: Vec::new(), tenant_id: Some("tenant-a".to_string()) },
            Actor { user_id: "bob".to_string(), claims: Vec::new(), tenant_id: Some("tenant-b".to_string()) },
        ];
        for actor in actors {
            for requested in
                [strings(&["queue", "list"]), strings(&["workflow", "config", "agent-set", "--id", "reviewer"])]
            {
                let error = build_ao_args("/project", &requested, Some(&actor))
                    .expect_err("actor-bound call must never downgrade into an unscoped backend");
                assert!(error.to_string().contains("not actor-aware"));
            }
        }
    }

    #[test]
    fn rejects_child_attempt_to_override_pinned_actor() {
        let requested = strings(&["workflow", "run", "--subject-id", "TASK-970", "--actor-json", "{}"]);
        let error = build_ao_args("/project", &requested, Some(&actor()))
            .expect_err("child actor override must not create an unscoped or differently scoped command");
        assert!(error.to_string().contains("override the server-pinned actor"));
    }
}
