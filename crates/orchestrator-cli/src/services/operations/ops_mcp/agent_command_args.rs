use super::{push_opt, push_opt_num, AgentRunInput};

pub(super) fn build_agent_run_args(input: &AgentRunInput) -> Vec<String> {
    let mut args = vec![
        "agent".to_string(),
        "run".to_string(),
        "--tool".to_string(),
        input.tool.clone(),
        "--stream".to_string(),
        "false".to_string(),
    ];
    if let Some(model) = &input.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    if input.detach {
        args.push("--detach".to_string());
    }
    push_opt(&mut args, "--prompt", input.prompt.clone());
    push_opt(&mut args, "--cwd", input.cwd.clone());
    push_opt_num(&mut args, "--timeout-secs", input.timeout_secs);
    push_opt(&mut args, "--context-json", input.context_json.clone());
    push_opt(&mut args, "--runtime-contract-json", input.runtime_contract_json.clone());
    push_opt(&mut args, "--run-id", input.run_id.clone());
    push_opt(&mut args, "--runner-scope", input.runner_scope.clone());
    args
}
