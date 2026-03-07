use super::phase_output::{
    format_prior_phase_outputs, load_prior_phase_outputs, pipeline_phase_order_for_workflow,
};
use super::runtime_contract_builder::{
    load_phase_capabilities, phase_decision_contract_for, phase_output_contract_for,
    phase_system_prompt_for,
};
use super::phase_executor::load_agent_runtime_config;

pub(super) const WORKFLOW_PHASE_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/runtime/workflow_phase.prompt"
));

pub fn phase_directive_for(project_root: &str, phase_id: &str) -> String {
    let config = load_agent_runtime_config(project_root);
    config
        .phase_directive(phase_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            "Execute the current workflow phase with production-quality output.".to_string()
        })
}

pub fn build_phase_prompt(
    project_root: &str,
    workflow_id: &str,
    subject_id: &str,
    subject_title: &str,
    subject_description: &str,
    phase_id: &str,
    rework_context: Option<&str>,
    pipeline_vars: Option<&std::collections::HashMap<String, String>>,
) -> String {
    let caps = load_phase_capabilities(project_root, phase_id);
    let phase_action_rule = if caps.writes_files {
        "Requirements:\n- Make concrete file changes in this repository."
    } else {
        "Requirements:\n- This is a READ-ONLY phase. Do NOT create, edit, or write any files. Do NOT run commands that modify the repository.\n- Read and analyze the codebase to assess the task. Your only output should be your assessment and phase decision."
    };
    let phase_contract = phase_output_contract_for(project_root, phase_id);
    let require_commit_message = phase_requires_commit_message_with_config(project_root, phase_id);
    let product_change_rule = if caps.enforce_product_changes {
        "- For this phase, changes must include product source/config/test files outside `.ao/` unless the task is explicitly documentation-only."
    } else {
        ""
    };
    let implementation_commit_rule = if require_commit_message {
        format!(
            "- Before finishing implementation, emit one JSON line exactly like: {{\"kind\":\"{}\",\"commit_message\":\"<clear commit subject>\"}}.",
            phase_contract
                .as_ref()
                .map(|contract| contract.kind.as_str())
                .unwrap_or("implementation_result")
        )
    } else {
        String::new()
    };
    let phase_directive = phase_directive_for(project_root, phase_id);
    let phase_safety_rules = phase_safety_rules(&caps);
    let phase_decision_rule = if phase_decision_contract_for(project_root, phase_id).is_some() {
        format!(
            "- Before finishing, emit one JSON line with your phase assessment:\n  {{\"kind\":\"phase_decision\",\"phase_id\":\"{phase_id}\",\"verdict\":\"advance|rework|fail|skip\",\"confidence\":0.0-1.0,\"risk\":\"low|medium|high\",\"reason\":\"...\",\"evidence\":[{{\"kind\":\"...\",\"description\":\"...\"}}]}}\n- Set verdict to \"advance\" if work is complete and correct.\n- Set verdict to \"rework\" if issues remain that need another pass.\n- Set verdict to \"fail\" only if problems are unrecoverable.\n- Set verdict to \"skip\" to close the task without further work. Use with a reason from: \"already_done\", \"duplicate\", \"no_longer_valid\", \"out_of_scope\".\n- Be honest about confidence. 0.5 = uncertain, 0.8+ = confident."
        )
    } else {
        String::new()
    };

    let phase_order = pipeline_phase_order_for_workflow(project_root, workflow_id);
    let prior_outputs = load_prior_phase_outputs(project_root, workflow_id, phase_id, &phase_order);
    let prior_phase_context = format_prior_phase_outputs(&prior_outputs);
    let rework_context = rework_context.map(str::trim).filter(|value| !value.is_empty());
    let mut prior_context = prior_phase_context;
    if let Some(context) = rework_context {
        prior_context.push_str("\n\nFailure context:\n");
        prior_context.push_str(context);
    }

    let mut phase_prompt = WORKFLOW_PHASE_PROMPT_TEMPLATE
        .replace("__PROJECT_ROOT__", project_root)
        .replace("__WORKFLOW_ID__", workflow_id)
        .replace("__SUBJECT_ID__", subject_id)
        .replace("__SUBJECT_TITLE__", subject_title)
        .replace("__SUBJECT_DESCRIPTION__", subject_description)
        .replace("__PHASE_ID__", phase_id)
        .replace("__PHASE_DIRECTIVE__", phase_directive.trim())
        .replace("__PHASE_ACTION_RULE__", phase_action_rule)
        .replace("__PRODUCT_CHANGE_RULE__", product_change_rule)
        .replace("__PHASE_SAFETY_RULES__", phase_safety_rules)
        .replace("__PHASE_DECISION_RULE__", &phase_decision_rule)
        .replace(
            "__IMPLEMENTATION_COMMIT_RULE__",
            implementation_commit_rule.as_str(),
        )
        .replace("__PRIOR_PHASE_OUTPUTS__", &prior_context);

    if let Some(vars) = pipeline_vars {
        if !vars.is_empty() {
            phase_prompt = orchestrator_core::workflow_config::expand_variables(&phase_prompt, vars);
        }
    }

    if let Some(system_prompt) = phase_system_prompt_for(project_root, phase_id) {
        if !system_prompt.trim().is_empty() {
            let mut system_prompt = system_prompt;
            if let Some(vars) = pipeline_vars {
                if !vars.is_empty() {
                    system_prompt = orchestrator_core::workflow_config::expand_variables(&system_prompt, vars);
                }
            }
            return format!("{system_prompt}\n\n{phase_prompt}");
        }
    }

    phase_prompt
}

pub(super) fn phase_safety_rules(caps: &protocol::PhaseCapabilities) -> &'static str {
    if caps.is_research {
        return "- For research phases, treat greenfield repositories as valid: missing app source files is not a blocker by itself.\n- Do targeted discovery only: inspect first-party code (`src/`, `apps/`, `db/`, `tests/`) and active `.ao` task/requirement docs; avoid broad recursive listings.\n- Do not scan dependency or checkpoint trees unless explicitly required: skip `node_modules/`, `.git/`, `.ao/workflow-state/checkpoints/`, and `.ao/runs/`.\n- If code context is limited, produce concrete assumptions, risks, and a build-ready plan in repository artifacts instead of stopping.\n- Emit `research_required` only for true external blockers that cannot be reasonably unblocked with explicit assumptions.";
    }

    ""
}

pub fn phase_requires_commit_message(phase_id: &str) -> bool {
    protocol::PhaseCapabilities::defaults_for_phase(phase_id).requires_commit
}

pub fn phase_requires_commit_message_with_config(project_root: &str, phase_id: &str) -> bool {
    phase_output_contract_for(project_root, phase_id)
        .map(|contract| contract.requires_field("commit_message"))
        .unwrap_or_else(|| phase_requires_commit_message(phase_id))
}

pub(super) fn phase_result_kind_for(project_root: &str, phase_id: &str) -> String {
    phase_output_contract_for(project_root, phase_id)
        .map(|contract| contract.kind)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or_else(|| "implementation_result".to_string())
}
