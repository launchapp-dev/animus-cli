use std::collections::HashMap;

use crate::config_context::RuntimeConfigContext;
use crate::phase_output::{build_workflow_pipeline_context, format_prior_phase_outputs, load_prior_phase_outputs};
use orchestrator_config::SkillApplicationResult;
use serde::Serialize;
use serde_json::{Map, Value};

pub struct PhasePromptParams<'a> {
    pub project_root: &'a str,
    pub execution_cwd: &'a str,
    pub workflow_id: &'a str,
    pub subject_id: &'a str,
    pub subject_title: &'a str,
    pub subject_description: &'a str,
    pub phase_id: &'a str,
    pub rework_context: Option<&'a str>,
    pub pipeline_vars: Option<&'a HashMap<String, String>>,
    pub dispatch_input: Option<&'a str>,
    pub schedule_input: Option<&'a str>,
}

pub struct PhaseRenderParams<'a> {
    pub project_root: &'a str,
    pub execution_cwd: &'a str,
    pub workflow_id: &'a str,
    pub subject_id: &'a str,
    pub subject_title: &'a str,
    pub subject_description: &'a str,
    pub phase_id: &'a str,
}

pub(crate) const WORKFLOW_PHASE_PROMPT_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/runtime/workflow_phase.prompt"));

#[derive(Debug, Clone, Default, Serialize)]
pub struct PhasePromptInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework_context: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pipeline_vars: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_input: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderedPhasePrompt {
    pub project_root: String,
    pub execution_cwd: String,
    pub workflow_id: String,
    pub subject_id: String,
    pub subject_title: String,
    pub subject_description: String,
    pub phase_id: String,
    pub inputs: PhasePromptInputs,
    pub capabilities: protocol::PhaseCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_output_contract: Option<orchestrator_core::PhaseOutputContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_decision_contract: Option<orchestrator_core::PhaseDecisionContract>,
    pub phase_directive: String,
    pub phase_action_rule: String,
    pub product_change_rule: String,
    pub phase_safety_rules: String,
    pub phase_decision_rule: String,
    pub structured_result_rule: String,
    pub pipeline_context: String,
    pub prior_phase_outputs: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub phase_prompt_body: String,
    pub final_prompt: String,
}

pub fn build_phase_prompt(params: &PhasePromptParams<'_>) -> String {
    let inputs = PhasePromptInputs {
        rework_context: params.rework_context.map(ToOwned::to_owned),
        pipeline_vars: params.pipeline_vars.cloned().unwrap_or_default(),
        dispatch_input: params.dispatch_input.map(ToOwned::to_owned),
        schedule_input: params.schedule_input.map(ToOwned::to_owned),
    };
    let ctx = RuntimeConfigContext::load(params.project_root);
    render_phase_prompt_with_ctx(
        &ctx,
        &PhaseRenderParams {
            project_root: params.project_root,
            execution_cwd: params.execution_cwd,
            workflow_id: params.workflow_id,
            subject_id: params.subject_id,
            subject_title: params.subject_title,
            subject_description: params.subject_description,
            phase_id: params.phase_id,
        },
        inputs,
    )
    .final_prompt
}

pub fn render_phase_prompt(params: &PhaseRenderParams<'_>, inputs: PhasePromptInputs) -> RenderedPhasePrompt {
    let ctx = RuntimeConfigContext::load(params.project_root);
    render_phase_prompt_with_ctx(&ctx, params, inputs)
}

pub fn render_phase_prompt_with_ctx(
    ctx: &RuntimeConfigContext,
    params: &PhaseRenderParams<'_>,
    inputs: PhasePromptInputs,
) -> RenderedPhasePrompt {
    render_phase_prompt_with_ctx_overrides(ctx, params, inputs, None, None)
}

pub(crate) fn render_phase_prompt_with_ctx_overrides(
    ctx: &RuntimeConfigContext,
    params: &PhaseRenderParams<'_>,
    inputs: PhasePromptInputs,
    capabilities_override: Option<protocol::PhaseCapabilities>,
    skill_result: Option<&SkillApplicationResult>,
) -> RenderedPhasePrompt {
    let project_root = params.project_root;
    let execution_cwd = params.execution_cwd;
    let workflow_id = params.workflow_id;
    let subject_id = params.subject_id;
    let subject_title = params.subject_title;
    let subject_description = params.subject_description;
    let phase_id = params.phase_id;
    let caps = capabilities_override.unwrap_or_else(|| ctx.phase_capabilities(phase_id));
    let phase_decision_contract = ctx.phase_decision_contract(phase_id).cloned();
    let phase_action_rule = phase_action_rule(&caps);
    let phase_contract = ctx.phase_output_contract(phase_id).cloned();
    let require_commit_message = phase_requires_commit_message_with_ctx(ctx, phase_id);
    let product_change_rule = if caps.enforce_product_changes {
        "- For this phase, changes must include product source/config/test files outside `.ao/` unless the task is explicitly documentation-only."
    } else {
        ""
    };
    let phase_directive = ctx.phase_directive(phase_id);
    let phase_safety_rules = phase_safety_rules(&caps);
    let decision_extra_field_rule =
        phase_decision_contract.as_ref().map(phase_decision_extra_field_rule).unwrap_or_default();
    let result_field_description_rule = phase_contract.as_ref().map(phase_output_field_rule).unwrap_or_default();
    let structured_result_rule = match (phase_contract.as_ref(), phase_decision_contract.as_ref()) {
        (Some(contract), Some(_)) => {
            let required_fields = if contract.required_fields.is_empty() {
                "- The top-level result object has no extra required fields beyond its kind.".to_string()
            } else {
                format!(
                    "- The top-level result object must include these required fields: {}.",
                    contract.required_fields.iter().map(|field| format!("`{field}`")).collect::<Vec<_>>().join(", ")
                )
            };
            let result_example = phase_result_example_for_prompt(contract, phase_id, phase_decision_contract.as_ref());
            format!(
                "- Before finishing, emit one JSON line as the FINAL line of output with your phase result and nested phase decision:\n  {}\n{}\n{}\n- Put any prose summary BEFORE the JSON line and emit nothing after it.",
                result_example,
                required_fields,
                result_field_description_rule,
            )
        }
        (Some(contract), None) => {
            let result_rule = if require_commit_message {
                format!(
                    "- Before finishing, emit one JSON line exactly like: {{\"kind\":\"{}\",\"commit_message\":\"<clear commit subject>\"}}.",
                    contract.kind
                )
            } else {
                format!(
                    "- Before finishing, emit one JSON line as the FINAL line of output with kind `{}`.",
                    contract.kind
                )
            };
            let required_fields = if contract.required_fields.is_empty() {
                String::new()
            } else {
                format!(
                    "\n- Include these required result fields: {}.",
                    contract.required_fields.iter().map(|field| format!("`{field}`")).collect::<Vec<_>>().join(", ")
                )
            };
            format!("{result_rule}{required_fields}\n{result_field_description_rule}")
        }
        (None, _) => String::new(),
    };
    let phase_decision_rule = if phase_contract.is_some() {
        if phase_decision_contract.is_some() {
            format!(
                "- The nested `phase_decision` object must describe whether this phase should advance, rework, fail, or skip.\n- Set `phase_decision.verdict` to `advance` if work is complete and correct.\n- Set `phase_decision.verdict` to `rework` if issues remain that need another pass.\n- Set `phase_decision.verdict` to `fail` only if problems are unrecoverable.\n- Set `phase_decision.verdict` to `skip` to close the task without further work. Use with a reason from: `already_done`, `duplicate`, `no_longer_valid`, `out_of_scope`.\n{}",
                decision_extra_field_rule
            )
        } else {
            String::new()
        }
    } else if let Some(contract) = phase_decision_contract.as_ref() {
        let required_evidence = if contract.required_evidence.is_empty() {
            "- Include evidence entries when they materially support your verdict.".to_string()
        } else {
            format!(
                "- Evidence must include these kinds when applicable: {}.",
                contract
                    .required_evidence
                    .iter()
                    .map(|kind| serde_json::to_string(kind).unwrap_or_else(|_| "\"custom\"".to_string()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let missing_decision_rule = if contract.allow_missing_decision {
            ""
        } else {
            "\n- A missing phase_decision is invalid. Do not finish without emitting it."
        };
        let decision_example = phase_decision_example_for_prompt(phase_id, Some(contract));
        format!(
            "- Before finishing, emit one JSON line with your phase assessment as the FINAL line of output:\n  {}\n- Set verdict to \"advance\" if work is complete and correct.\n- Set verdict to \"rework\" if issues remain that need another pass.\n- Set verdict to \"fail\" only if problems are unrecoverable.\n- Set verdict to \"skip\" to close the task without further work. Use with a reason from: \"already_done\", \"duplicate\", \"no_longer_valid\", \"out_of_scope\".\n- Confidence must be at least {} unless you truly cannot justify a decision.\n- Risk must not exceed {:?} unless you are explicitly failing the phase.\n{}\n{}\n- Put any prose summary BEFORE the JSON line and emit nothing after it.{}",
            decision_example,
            contract.min_confidence,
            contract.max_risk,
            required_evidence,
            decision_extra_field_rule,
            missing_decision_rule
        )
    } else {
        String::new()
    };

    let (pipeline_context, phase_order) = build_workflow_pipeline_context(project_root, workflow_id, phase_id);
    let prior_outputs = load_prior_phase_outputs(project_root, workflow_id, phase_id, &phase_order);
    let prior_phase_context = format_prior_phase_outputs(&prior_outputs);
    let rework_context = inputs.rework_context.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let mut prior_context = prior_phase_context;
    if let Some(context) = rework_context {
        prior_context.push_str("\n\nFailure context:\n");
        prior_context.push_str(context);
    }

    let mut phase_prompt = WORKFLOW_PHASE_PROMPT_TEMPLATE
        .replace("__PROJECT_ROOT__", project_root)
        .replace("__EXECUTION_CWD__", execution_cwd)
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
        .replace("__IMPLEMENTATION_COMMIT_RULE__", structured_result_rule.as_str())
        .replace("__WORKFLOW_PIPELINE_CONTEXT__", &pipeline_context)
        .replace("__PRIOR_PHASE_OUTPUTS__", &prior_context);

    if !inputs.pipeline_vars.is_empty() {
        phase_prompt = orchestrator_core::workflow_config::expand_variables(&phase_prompt, &inputs.pipeline_vars);
    }

    if let Some(dispatch_input) = inputs.dispatch_input.as_deref().filter(|value| !value.is_empty()) {
        phase_prompt.push_str("\n\nDispatch input:\n");
        phase_prompt.push_str(dispatch_input);
    } else if let Some(schedule_input) = inputs.schedule_input.as_deref().filter(|value| !value.is_empty()) {
        phase_prompt.push_str("\n\nSchedule trigger input:\n");
        phase_prompt.push_str(schedule_input);
    }

    let mut prompt_sections = Vec::new();
    if let Some(skill_result) = skill_result {
        for prefix in &skill_result.prompt_prefixes {
            if let Some(expanded) = expand_prompt_fragment(prefix, &inputs.pipeline_vars) {
                prompt_sections.push(expanded);
            }
        }
        if !skill_result.directives.is_empty() {
            let directives = skill_result
                .directives
                .iter()
                .filter_map(|directive| expand_prompt_fragment(directive, &inputs.pipeline_vars))
                .collect::<Vec<_>>();
            if !directives.is_empty() {
                let mut section = String::from("Skill directives:");
                for directive in directives {
                    section.push_str("\n- ");
                    section.push_str(&directive);
                }
                prompt_sections.push(section);
            }
        }
    }
    prompt_sections.push(phase_prompt);
    if let Some(skill_result) = skill_result {
        for suffix in &skill_result.prompt_suffixes {
            if let Some(expanded) = expand_prompt_fragment(suffix, &inputs.pipeline_vars) {
                prompt_sections.push(expanded);
            }
        }
    }
    let phase_prompt_body = prompt_sections.join("\n\n");

    let mut system_prompt_sections = Vec::new();
    if let Some(prompt) = ctx.phase_system_prompt(phase_id) {
        if let Some(expanded) = expand_prompt_fragment(&prompt, &inputs.pipeline_vars) {
            system_prompt_sections.push(expanded);
        }
    }
    if let Some(skill_result) = skill_result {
        for fragment in &skill_result.system_prompt_fragments {
            if let Some(expanded) = expand_prompt_fragment(fragment, &inputs.pipeline_vars) {
                system_prompt_sections.push(expanded);
            }
        }
    }
    let system_prompt = (!system_prompt_sections.is_empty()).then(|| system_prompt_sections.join("\n\n"));
    let final_prompt = match system_prompt.as_deref() {
        Some(system_prompt) => format!("{system_prompt}\n\n{phase_prompt_body}"),
        None => phase_prompt_body.clone(),
    };

    RenderedPhasePrompt {
        project_root: project_root.to_string(),
        execution_cwd: execution_cwd.to_string(),
        workflow_id: workflow_id.to_string(),
        subject_id: subject_id.to_string(),
        subject_title: subject_title.to_string(),
        subject_description: subject_description.to_string(),
        phase_id: phase_id.to_string(),
        inputs,
        capabilities: caps,
        phase_output_contract: phase_contract,
        phase_decision_contract,
        phase_directive,
        phase_action_rule: phase_action_rule.to_string(),
        product_change_rule: product_change_rule.to_string(),
        phase_safety_rules: phase_safety_rules.to_string(),
        phase_decision_rule,
        structured_result_rule,
        pipeline_context,
        prior_phase_outputs: prior_context,
        system_prompt,
        phase_prompt_body,
        final_prompt,
    }
}

fn expand_prompt_fragment(fragment: &str, pipeline_vars: &HashMap<String, String>) -> Option<String> {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return None;
    }

    if pipeline_vars.is_empty() {
        return Some(trimmed.to_string());
    }

    Some(orchestrator_core::workflow_config::expand_variables(trimmed, pipeline_vars))
}

fn phase_decision_example_for_prompt(
    phase_id: &str,
    contract: Option<&orchestrator_core::PhaseDecisionContract>,
) -> String {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::String("phase_decision".to_string()));
    object.insert("phase_id".to_string(), Value::String(phase_id.to_string()));
    object.insert("verdict".to_string(), Value::String("advance|rework|fail|skip".to_string()));
    object.insert("confidence".to_string(), serde_json::json!(0.95));
    object.insert("risk".to_string(), Value::String("low|medium|high".to_string()));
    object.insert("reason".to_string(), Value::String("...".to_string()));
    object.insert(
        "evidence".to_string(),
        Value::Array(vec![serde_json::json!({
            "kind": "...",
            "description": "..."
        })]),
    );
    if let Some(contract) = contract {
        for (field_name, field) in &contract.fields {
            object.insert(field_name.clone(), phase_field_placeholder(field_name, field));
        }
    }
    serde_json::to_string(&Value::Object(object)).unwrap_or_else(|_| "{\"kind\":\"phase_decision\"}".to_string())
}

fn phase_result_example_for_prompt(
    contract: &orchestrator_core::PhaseOutputContract,
    phase_id: &str,
    decision_contract: Option<&orchestrator_core::PhaseDecisionContract>,
) -> String {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::String(contract.kind.clone()));
    for field_name in &contract.required_fields {
        object.insert(field_name.clone(), Value::String(format!("<{}>", field_name.replace('_', " "))));
    }
    for (field_name, field) in &contract.fields {
        object.insert(field_name.clone(), phase_field_placeholder(field_name, field));
    }
    object.insert(
        "phase_decision".to_string(),
        serde_json::from_str::<Value>(&phase_decision_example_for_prompt(phase_id, decision_contract))
            .unwrap_or_else(|_| Value::Object(Map::new())),
    );
    serde_json::to_string(&Value::Object(object)).unwrap_or_else(|_| {
        format!("{{\"kind\":\"{}\",\"phase_decision\":{{\"kind\":\"phase_decision\"}}}}", contract.kind)
    })
}

fn phase_field_placeholder(
    field_name: &str,
    field: &orchestrator_core::agent_runtime_config::PhaseFieldDefinition,
) -> Value {
    match field.field_type.as_str() {
        "string" => field
            .enum_values
            .first()
            .map(|value| Value::String(value.clone()))
            .unwrap_or_else(|| Value::String(format!("<{}>", field_name.replace('_', " ")))),
        "number" => serde_json::json!(0.0),
        "integer" => serde_json::json!(0),
        "boolean" => Value::Bool(false),
        "array" => Value::Array(vec![field
            .items
            .as_ref()
            .map(|item| phase_field_placeholder(field_name, item))
            .unwrap_or_else(|| Value::String("...".to_string()))]),
        "object" => {
            let mut map = Map::new();
            for (nested_name, nested_field) in &field.fields {
                map.insert(nested_name.clone(), phase_field_placeholder(nested_name, nested_field));
            }
            Value::Object(map)
        }
        _ => Value::String(format!("<{}>", field_name.replace('_', " "))),
    }
}

fn phase_output_field_rule(contract: &orchestrator_core::PhaseOutputContract) -> String {
    if contract.fields.is_empty() {
        return String::new();
    }

    let mut lines = vec!["- The top-level result object may include these config-defined fields:".to_string()];
    for (field_name, field) in &contract.fields {
        lines.push(format!(
            "  - `{field_name}` ({}){}{}",
            field.field_type,
            if field.required { ", required" } else { "" },
            field.description.as_deref().map(|value| format!(": {value}")).unwrap_or_default()
        ));
    }

    lines.join("\n")
}

fn phase_decision_extra_field_rule(contract: &orchestrator_core::PhaseDecisionContract) -> String {
    let mut lines = Vec::new();
    let mut required_fields = contract
        .fields
        .iter()
        .filter_map(|(field_name, field)| if field.required { Some(field_name.clone()) } else { None })
        .collect::<Vec<_>>();
    if let Some(schema) = contract.extra_json_schema.as_ref() {
        let extra_required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(serde_json::Value::as_str).map(ToOwned::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        for field_name in extra_required {
            if !required_fields.iter().any(|existing| existing.eq_ignore_ascii_case(&field_name)) {
                required_fields.push(field_name);
            }
        }
    }

    if !required_fields.is_empty() {
        lines.push(format!(
            "- The `phase_decision` object must also include these config-required fields: {}.",
            required_fields.iter().map(|field| format!("`{field}`")).collect::<Vec<_>>().join(", ")
        ));
    }

    let mut optional_fields = contract
        .fields
        .keys()
        .filter(|field| !required_fields.iter().any(|required| required == *field))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(schema) = contract.extra_json_schema.as_ref() {
        let property_names = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for field_name in property_names {
            if !required_fields.iter().any(|required| required == &field_name)
                && !optional_fields.iter().any(|existing| existing == &field_name)
            {
                optional_fields.push(field_name);
            }
        }
    }
    let optional_fields = optional_fields.into_iter().collect::<Vec<_>>();
    if !optional_fields.is_empty() {
        lines.push(format!(
            "- The `phase_decision` object may include these additional config-defined fields when relevant: {}.",
            optional_fields.iter().map(|field| format!("`{field}`")).collect::<Vec<_>>().join(", ")
        ));
    }

    if !contract.fields.is_empty() {
        lines.push("- Decision field descriptions:".to_string());
        for (field_name, field) in &contract.fields {
            let detail = field.description.as_deref().unwrap_or("No description provided.");
            lines.push(format!(
                "  - `{field_name}` ({}){}: {}",
                field.field_type,
                if field.required { ", required" } else { "" },
                detail
            ));
        }
    }

    lines.join("\n")
}

pub(crate) fn phase_safety_rules(caps: &protocol::PhaseCapabilities) -> &'static str {
    if caps.is_research {
        return "- For research phases, treat greenfield repositories as valid: missing app source files is not a blocker by itself.\n- Do targeted discovery only: inspect first-party code (`src/`, `apps/`, `db/`, `tests/`) and active `.ao` task/requirement docs; avoid broad recursive listings.\n- Do not scan dependency or checkpoint trees unless explicitly required: skip `node_modules/`, `.git/`, and scoped runtime state under `~/.ao/<scope>/`.\n- If code context is limited, produce concrete assumptions, risks, and a build-ready plan in repository artifacts instead of stopping.";
    }

    ""
}

pub(crate) fn phase_action_rule(caps: &protocol::PhaseCapabilities) -> &'static str {
    if caps.writes_files {
        return "Requirements:\n- Make concrete file changes in this repository.";
    }

    if caps.mutates_state {
        return "Requirements:\n- Do NOT create, edit, or write repository files.\n- You may use approved AO/MCP/runtime operations to perform the state mutations required by this phase.\n- Keep those mutations scoped to the current task, workflow, or managed system state, then emit the required structured phase result.";
    }

    "Requirements:\n- This is a READ-ONLY phase. Do NOT create, edit, or write any files. Do NOT run commands that modify the repository.\n- Read and analyze the codebase to assess the task. Your only output should be your assessment and phase decision."
}

pub fn phase_requires_commit_message(phase_id: &str) -> bool {
    protocol::PhaseCapabilities::defaults_for_phase(phase_id).requires_commit
}

pub fn phase_requires_commit_message_with_config(project_root: &str, phase_id: &str) -> bool {
    let ctx = RuntimeConfigContext::load(project_root);
    phase_requires_commit_message_with_ctx(&ctx, phase_id)
}

pub fn phase_requires_commit_message_with_ctx(ctx: &RuntimeConfigContext, phase_id: &str) -> bool {
    ctx.phase_output_contract(phase_id)
        .map(|contract| contract.requires_field("commit_message"))
        .unwrap_or_else(|| phase_requires_commit_message(phase_id))
}

pub(crate) fn phase_result_kind_for_ctx(ctx: &RuntimeConfigContext, phase_id: &str) -> String {
    ctx.phase_output_contract(phase_id)
        .map(|contract| contract.kind.clone())
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or_else(|| "implementation_result".to_string())
}

#[cfg(test)]
mod tests {
    use super::phase_action_rule;

    #[test]
    fn mutating_state_phases_are_not_rendered_as_strictly_read_only() {
        let rule = phase_action_rule(&protocol::PhaseCapabilities { mutates_state: true, ..Default::default() });
        assert!(rule.contains("AO/MCP/runtime operations"));
        assert!(!rule.contains("READ-ONLY phase"));
    }

    #[test]
    fn strict_read_only_phases_keep_read_only_guidance() {
        let rule = phase_action_rule(&protocol::PhaseCapabilities::default());
        assert!(rule.contains("READ-ONLY phase"));
    }
}
