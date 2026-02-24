use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    services::ServiceHub, Complexity, Priority, RequirementItem, RequirementPriority, Scope,
    TaskCreateInput, TaskStatus, TaskType,
};
use protocol::{
    default_fallback_models_for_phase, default_primary_model_for_phase, tool_for_model_id,
    AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use super::{
    build_runtime_contract, collect_json_payload_lines, connect_runner, event_matches_run,
    runner_config_dir, write_json_line,
};

const DEFAULT_TASK_GEN_TIMEOUT_SECS: u64 = 900;
const DEFAULT_MAX_TASKS_PER_REQUIREMENT: usize = 6;
const DEFAULT_REPAIR_ATTEMPTS: usize = 1;

#[derive(Debug, Clone)]
struct TaskGenerationConfig {
    timeout_secs: u64,
    start_runner: bool,
    max_tasks_per_requirement: usize,
    max_repair_attempts: usize,
    model_candidates: Vec<String>,
    tool_override: Option<String>,
}

impl Default for TaskGenerationConfig {
    fn default() -> Self {
        Self {
            timeout_secs: env_u64("AO_TASK_GEN_TIMEOUT_SECS", DEFAULT_TASK_GEN_TIMEOUT_SECS),
            start_runner: env_bool("AO_TASK_GEN_START_RUNNER", true),
            max_tasks_per_requirement: env_usize(
                "AO_TASK_GEN_MAX_TASKS",
                DEFAULT_MAX_TASKS_PER_REQUIREMENT,
            )
            .max(1),
            max_repair_attempts: env_usize("AO_TASK_GEN_REPAIR_ATTEMPTS", DEFAULT_REPAIR_ATTEMPTS),
            model_candidates: model_candidates(),
            tool_override: env_string("AO_TASK_GEN_TOOL"),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct AiTaskGenerationSummary {
    pub requirements_considered: usize,
    pub requirements_with_existing_tasks: usize,
    pub requirements_generated: usize,
    pub task_ids_created: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TaskGenerationCandidate {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    task_type: String,
    #[serde(default)]
    complexity: String,
    #[serde(default)]
    estimated_files: Vec<String>,
    #[serde(default)]
    implementation_steps: Vec<String>,
    #[serde(default)]
    testing_notes: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TaskGenerationBatch {
    tasks: Vec<TaskGenerationCandidate>,
}

pub(crate) async fn ensure_ai_generated_tasks_for_requirements(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    requirement_ids: &[String],
) -> Result<AiTaskGenerationSummary> {
    if env_bool("AO_DISABLE_AI_TASK_GENERATION", false) {
        return Ok(AiTaskGenerationSummary::default());
    }

    let config = TaskGenerationConfig::default();
    if config.model_candidates.is_empty() {
        return Err(anyhow!(
            "no task-generation models configured; set AO_TASK_GEN_MODELS or model defaults"
        ));
    }

    if config.start_runner {
        hub.daemon()
            .start()
            .await
            .context("failed to start daemon/runner for AI task generation")?;
    }

    let planning = hub.planning();
    let mut requirements = planning.list_requirements().await?;
    requirements.sort_by(|a, b| a.id.cmp(&b.id));

    if !requirement_ids.is_empty() {
        let filter = requirement_ids
            .iter()
            .map(|id| id.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        requirements.retain(|requirement| filter.contains(&requirement.id.to_ascii_lowercase()));
    }

    if requirements.is_empty() {
        return Ok(AiTaskGenerationSummary::default());
    }

    let mut tasks = hub.tasks().list().await?;

    let mut summary = AiTaskGenerationSummary {
        requirements_considered: requirements.len(),
        ..AiTaskGenerationSummary::default()
    };

    for requirement in requirements {
        if requirement_has_active_tasks(&requirement, &tasks) {
            summary.requirements_with_existing_tasks =
                summary.requirements_with_existing_tasks.saturating_add(1);
            continue;
        }

        let mut existing_titles = tasks
            .iter()
            .filter(|task| {
                !task.cancelled
                    && task.status != TaskStatus::Cancelled
                    && task
                        .linked_requirements
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(requirement.id.as_str()))
            })
            .map(|task| normalize_task_title(&task.title))
            .collect::<HashSet<_>>();

        let (candidates, selected_model) =
            generate_requirement_tasks_via_ai(project_root, &requirement, &config).await?;

        let mut created_for_requirement = 0usize;
        for candidate in candidates
            .into_iter()
            .take(config.max_tasks_per_requirement)
        {
            let normalized_title = normalize_task_title(&candidate.title);
            if normalized_title.is_empty() || existing_titles.contains(&normalized_title) {
                continue;
            }

            let base_task = hub
                .tasks()
                .create(TaskCreateInput {
                    title: candidate.title.clone(),
                    description: build_task_description(&requirement, &candidate),
                    task_type: Some(parse_task_type(&candidate.task_type)),
                    priority: Some(requirement_priority_to_task_priority(requirement.priority)),
                    created_by: Some("requirement-review-loop-ai".to_string()),
                    tags: task_tags_from_requirement(&requirement),
                    linked_requirements: vec![requirement.id.clone()],
                })
                .await
                .with_context(|| {
                    format!(
                        "failed to create AI task '{}' for requirement {}",
                        candidate.title, requirement.id
                    )
                })?;

            let assigned_task = hub
                .tasks()
                .assign_agent(
                    base_task.id.as_str(),
                    "implementation".to_string(),
                    Some(selected_model.clone()),
                    "requirement-review-loop-ai".to_string(),
                )
                .await
                .unwrap_or(base_task);

            let mut enriched_task = assigned_task;
            enriched_task.complexity = parse_task_complexity(&candidate.complexity);
            enriched_task.scope = match enriched_task.complexity {
                Complexity::High => Scope::Large,
                Complexity::Medium => Scope::Medium,
                Complexity::Low => Scope::Small,
            };
            enriched_task.metadata.updated_at = Utc::now();
            enriched_task.metadata.updated_by = "requirement-review-loop-ai".to_string();
            enriched_task.metadata.version = enriched_task.metadata.version.saturating_add(1);

            let persisted = hub.tasks().replace(enriched_task).await?;
            existing_titles.insert(normalized_title);
            summary.task_ids_created.push(persisted.id.clone());
            tasks.push(persisted);
            created_for_requirement = created_for_requirement.saturating_add(1);
        }

        if created_for_requirement == 0 {
            return Err(anyhow!(
                "AI task generation produced no materialized tasks for requirement {} ({})",
                requirement.id,
                requirement.title
            ));
        }
        summary.requirements_generated = summary.requirements_generated.saturating_add(1);
    }

    Ok(summary)
}

fn requirement_has_active_tasks(
    requirement: &RequirementItem,
    all_tasks: &[orchestrator_core::OrchestratorTask],
) -> bool {
    let mut linked_ids = requirement.links.tasks.clone();
    linked_ids.extend(requirement.linked_task_ids.clone());
    linked_ids.sort();
    linked_ids.dedup();

    let active_task_ids = all_tasks
        .iter()
        .filter(|task| !task.cancelled && task.status != TaskStatus::Cancelled)
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();

    if linked_ids
        .iter()
        .any(|task_id| active_task_ids.contains(task_id.as_str()))
    {
        return true;
    }

    all_tasks.iter().any(|task| {
        !task.cancelled
            && task.status != TaskStatus::Cancelled
            && task
                .linked_requirements
                .iter()
                .any(|requirement_id| requirement_id.eq_ignore_ascii_case(requirement.id.as_str()))
    })
}

fn build_task_description(
    requirement: &RequirementItem,
    candidate: &TaskGenerationCandidate,
) -> String {
    let mut sections = Vec::new();
    sections.push(candidate.description.trim().to_string());

    if !candidate.implementation_steps.is_empty() {
        sections.push(format!(
            "## Implementation Steps\n{}",
            candidate
                .implementation_steps
                .iter()
                .enumerate()
                .map(|(idx, step)| format!("{}. {}", idx + 1, step.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !candidate.estimated_files.is_empty() {
        sections.push(format!(
            "## Estimated Files\n{}",
            candidate
                .estimated_files
                .iter()
                .filter(|file| !file.trim().is_empty())
                .map(|file| format!("- `{}`", file.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !candidate.testing_notes.trim().is_empty() {
        sections.push(format!(
            "## Testing Notes\n{}",
            candidate.testing_notes.trim()
        ));
    }

    if !requirement.acceptance_criteria.is_empty() {
        sections.push(format!(
            "## Requirement Acceptance Criteria\n{}",
            requirement
                .acceptance_criteria
                .iter()
                .map(|criterion| format!("- [ ] {}", criterion))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    sections
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn task_tags_from_requirement(requirement: &RequirementItem) -> Vec<String> {
    let mut tags = vec![
        "from-requirement".to_string(),
        "requirement-derived".to_string(),
        "ai-generated".to_string(),
    ];
    if requirement_is_frontend_related(requirement) {
        tags.push("frontend".to_string());
        tags.push("ui-ux".to_string());
    }
    if requirement_needs_research(requirement) {
        tags.push("needs-research".to_string());
    }
    tags.sort();
    tags.dedup();
    tags
}

fn requirement_priority_to_task_priority(priority: RequirementPriority) -> Priority {
    match priority {
        RequirementPriority::Must => Priority::High,
        RequirementPriority::Should => Priority::Medium,
        RequirementPriority::Could | RequirementPriority::Wont => Priority::Low,
    }
}

fn parse_task_type(raw: &str) -> TaskType {
    match raw.trim().to_ascii_lowercase().as_str() {
        "bugfix" => TaskType::Bugfix,
        "refactor" => TaskType::Refactor,
        "test" => TaskType::Test,
        "docs" => TaskType::Docs,
        "chore" => TaskType::Chore,
        "hotfix" => TaskType::Hotfix,
        _ => TaskType::Feature,
    }
}

fn parse_task_complexity(raw: &str) -> Complexity {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Complexity::Low,
        "high" => Complexity::High,
        _ => Complexity::Medium,
    }
}

fn normalize_task_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

fn requirement_is_frontend_related(requirement: &RequirementItem) -> bool {
    if requirement.tags.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "frontend" | "ui" | "ux" | "design" | "react" | "web" | "nextjs" | "design-system"
        )
    }) {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        requirement.title,
        requirement.description,
        requirement.acceptance_criteria.join(" ")
    )
    .to_ascii_lowercase();
    haystack.contains("frontend")
        || haystack.contains("next.js")
        || haystack.contains("nextjs")
        || haystack.contains("react")
        || haystack.contains("user interface")
        || haystack.contains("user experience")
}

fn requirement_needs_research(requirement: &RequirementItem) -> bool {
    if requirement.tags.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "needs-research" | "research" | "discovery" | "investigation" | "spike"
        )
    }) {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        requirement.title,
        requirement.description,
        requirement.acceptance_criteria.join(" ")
    )
    .to_ascii_lowercase();
    haystack.contains("research")
        || haystack.contains("investigate")
        || haystack.contains("benchmark")
        || haystack.contains("tradeoff")
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn model_candidates() -> Vec<String> {
    if let Some(raw_models) = env_string("AO_TASK_GEN_MODELS") {
        let mut parsed = raw_models
            .split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        parsed.dedup();
        if !parsed.is_empty() {
            return parsed;
        }
    }

    let mut models = vec![default_primary_model_for_phase("implementation", None).to_string()];
    models.extend(
        default_fallback_models_for_phase("implementation", None)
            .into_iter()
            .map(|model| model.to_string()),
    );
    models.sort();
    models.dedup();
    models
}

async fn generate_requirement_tasks_via_ai(
    project_root: &str,
    requirement: &RequirementItem,
    config: &TaskGenerationConfig,
) -> Result<(Vec<TaskGenerationCandidate>, String)> {
    let mut last_error: Option<anyhow::Error> = None;
    for model in &config.model_candidates {
        let tool = config
            .tool_override
            .clone()
            .unwrap_or_else(|| tool_for_model_id(model).to_string());
        match run_task_generation_with_model(project_root, requirement, config, model, &tool).await
        {
            Ok(tasks) if !tasks.is_empty() => return Ok((tasks, model.clone())),
            Ok(_) => {
                last_error = Some(anyhow!(
                    "model {} returned zero task candidates for requirement {}",
                    model,
                    requirement.id
                ));
            }
            Err(error) => {
                last_error = Some(error.context(format!(
                    "model {} failed task generation for requirement {}",
                    model, requirement.id
                )));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        anyhow!(
            "task generation failed for requirement {}",
            requirement.id.as_str()
        )
    }))
}

async fn run_task_generation_with_model(
    project_root: &str,
    requirement: &RequirementItem,
    config: &TaskGenerationConfig,
    model: &str,
    tool: &str,
) -> Result<Vec<TaskGenerationCandidate>> {
    let prompt = build_task_generation_prompt(requirement);
    let mut transcript =
        run_prompt_against_runner(project_root, &prompt, model, tool, config.timeout_secs).await?;

    if let Some(parsed) = parse_task_generation_output(&transcript) {
        return Ok(parsed);
    }

    let mut preview = transcript.trim().to_string();
    if preview.len() > 500 {
        preview.truncate(500);
    }

    for attempt in 1..=config.max_repair_attempts {
        let repair_prompt = build_task_generation_repair_prompt(
            requirement,
            attempt,
            config.max_repair_attempts,
            &preview,
        );
        transcript = run_prompt_against_runner(
            project_root,
            &repair_prompt,
            model,
            tool,
            config.timeout_secs,
        )
        .await?;
        if let Some(parsed) = parse_task_generation_output(&transcript) {
            return Ok(parsed);
        }
    }

    Err(anyhow!(
        "task generation output for requirement {} was not parseable JSON",
        requirement.id
    ))
}

async fn run_prompt_against_runner(
    project_root: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("task-gen-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "task-generation",
        "allowed_tools": ["Read", "Glob", "Grep", "WebSearch"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                return Err(anyhow!("task generation run failed: {error}"));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "task generation run exited with non-zero code: {:?}",
                        exit_code
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    if transcript.trim().is_empty() {
        return Err(anyhow!("task generation run produced empty output"));
    }

    Ok(transcript)
}

fn build_task_generation_prompt(requirement: &RequirementItem) -> String {
    format!(
        r#"You are a senior software engineer planning implementation tasks for a requirement.

## Requirement
- ID: {id}
- Title: {title}
- Priority: {priority:?}
- Description: {description}
- Acceptance Criteria: {acceptance_criteria}
- Tags: {tags}

## Instructions
1. Break the requirement into concrete, independently executable tasks.
2. Cover all acceptance criteria with minimal overlap.
3. Reference likely files/components to touch when possible.
4. Keep task descriptions implementation-ready and specific.

## Output Contract
Return exactly one JSON object:
{{
  "tasks": [
    {{
      "title": "Concise task title",
      "description": "Detailed task description with technical approach and constraints",
      "task_type": "feature|bugfix|refactor|test|docs|chore|hotfix",
      "complexity": "low|medium|high",
      "estimated_files": ["path/to/file.ts"],
      "implementation_steps": ["Step 1", "Step 2"],
      "testing_notes": "Validation strategy"
    }}
  ]
}}

Rules:
- Output valid JSON only.
- No markdown fences, no extra commentary.
- Return 2-6 tasks unless requirement is truly tiny."#,
        id = requirement.id,
        title = requirement.title,
        priority = requirement.priority,
        description = requirement.description,
        acceptance_criteria = serde_json::to_string(&requirement.acceptance_criteria)
            .unwrap_or_else(|_| "[]".to_string()),
        tags = serde_json::to_string(&requirement.tags).unwrap_or_else(|_| "[]".to_string())
    )
}

fn build_task_generation_repair_prompt(
    requirement: &RequirementItem,
    attempt: usize,
    max_attempts: usize,
    violation_preview: &str,
) -> String {
    format!(
        r#"You are repairing task generation output for a requirement.

## Requirement
- ID: {id}
- Title: {title}
- Description: {description}

## Repair Context
- Attempt: {attempt}/{max_attempts}
- Prior output preview: {preview}

Return exactly one valid JSON object with this schema:
{{
  "tasks": [
    {{
      "title": "Concise task title",
      "description": "Detailed task description",
      "task_type": "feature|bugfix|refactor|test|docs|chore|hotfix",
      "complexity": "low|medium|high",
      "estimated_files": ["path/to/file.ts"],
      "implementation_steps": ["Step 1", "Step 2"],
      "testing_notes": "Validation strategy"
    }}
  ]
}}

Rules:
- JSON only.
- No markdown.
- 1-6 tasks.
- Each task must include non-empty title and description."#,
        id = requirement.id,
        title = requirement.title,
        description = requirement.description,
        attempt = attempt,
        max_attempts = max_attempts,
        preview = violation_preview
    )
}

fn parse_task_generation_output(text: &str) -> Option<Vec<TaskGenerationCandidate>> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(parsed) = parse_task_generation_payload(&payload) {
            return Some(parsed);
        }
    }

    for block in extract_code_fence_candidates(text) {
        if let Ok(payload) = serde_json::from_str::<Value>(block.trim()) {
            if let Some(parsed) = parse_task_generation_payload(&payload) {
                return Some(parsed);
            }
        }
    }

    for block in extract_balanced_json_candidates(text) {
        if let Ok(payload) = serde_json::from_str::<Value>(block.trim()) {
            if let Some(parsed) = parse_task_generation_payload(&payload) {
                return Some(parsed);
            }
        }
    }

    let payload = serde_json::from_str::<Value>(text.trim()).ok()?;
    parse_task_generation_payload(&payload)
}

fn parse_task_generation_payload(payload: &Value) -> Option<Vec<TaskGenerationCandidate>> {
    if let Ok(batch) = serde_json::from_value::<TaskGenerationBatch>(payload.clone()) {
        let normalized = normalize_task_candidates(batch.tasks);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    if let Ok(list) = serde_json::from_value::<Vec<TaskGenerationCandidate>>(payload.clone()) {
        let normalized = normalize_task_candidates(list);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    if let Ok(single) = serde_json::from_value::<TaskGenerationCandidate>(payload.clone()) {
        let normalized = normalize_task_candidates(vec![single]);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    match payload {
        Value::Array(items) => {
            for item in items {
                if let Some(parsed) = parse_task_generation_payload(item) {
                    return Some(parsed);
                }
            }
        }
        Value::Object(object) => {
            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(parsed) = parse_task_generation_payload(value) {
                        return Some(parsed);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(value) = object.get(key).and_then(Value::as_str) {
                    if let Some(parsed) = parse_task_generation_output(value) {
                        return Some(parsed);
                    }
                }
            }
        }
        _ => {}
    }

    None
}

fn normalize_task_candidates(
    candidates: Vec<TaskGenerationCandidate>,
) -> Vec<TaskGenerationCandidate> {
    let mut dedupe = HashSet::new();
    let mut normalized = Vec::new();

    for mut candidate in candidates {
        candidate.title = candidate.title.trim().to_string();
        candidate.description = candidate.description.trim().to_string();
        candidate.task_type = candidate.task_type.trim().to_string();
        candidate.complexity = candidate.complexity.trim().to_string();
        candidate.testing_notes = candidate.testing_notes.trim().to_string();
        candidate.estimated_files = candidate
            .estimated_files
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        candidate.implementation_steps = candidate
            .implementation_steps
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();

        if candidate.title.is_empty() || candidate.description.is_empty() {
            continue;
        }

        let key = normalize_task_title(&candidate.title);
        if key.is_empty() || !dedupe.insert(key) {
            continue;
        }

        if candidate.task_type.is_empty() {
            candidate.task_type = "feature".to_string();
        }
        if candidate.complexity.is_empty() {
            candidate.complexity = "medium".to_string();
        }

        normalized.push(candidate);
    }

    normalized
}

fn extract_code_fence_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("```") {
        let after_start = &remaining[start + 3..];
        let Some(end) = after_start.find("```") else {
            break;
        };
        let block = &after_start[..end];
        let block = if let Some(newline) = block.find('\n') {
            let (header, body) = block.split_at(newline);
            let header = header.trim();
            if header.is_empty()
                || header
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == ' ')
            {
                body
            } else {
                block
            }
        } else {
            block
        };
        let trimmed = block.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
        remaining = &after_start[end + 3..];
    }
    candidates
}

fn extract_balanced_json_candidates(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut candidates = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let start = bytes[index] as char;
        if start != '{' && start != '[' {
            index = index.saturating_add(1);
            continue;
        }

        let mut stack = vec![start];
        let mut in_string = false;
        let mut escaped = false;
        let mut end_index = None;
        let mut cursor = index.saturating_add(1);
        while cursor < bytes.len() {
            let ch = bytes[cursor] as char;
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                cursor = cursor.saturating_add(1);
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' | '[' => stack.push(ch),
                '}' => {
                    if stack.pop() != Some('{') {
                        break;
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        break;
                    }
                }
                _ => {}
            }

            if stack.is_empty() {
                end_index = Some(cursor);
                break;
            }
            cursor = cursor.saturating_add(1);
        }

        if let Some(end) = end_index {
            let candidate = text[index..=end].trim();
            if !candidate.is_empty() {
                candidates.push(candidate.to_string());
            }
            index = end.saturating_add(1);
        } else {
            index = index.saturating_add(1);
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::parse_task_generation_output;

    #[test]
    fn parses_task_generation_from_wrapped_payload() {
        let text = r#"{"type":"item.completed","item":{"text":"{\"tasks\":[{\"title\":\"Build parser\",\"description\":\"Implement parser\",\"task_type\":\"feature\",\"complexity\":\"medium\",\"estimated_files\":[\"src/parser.rs\"],\"implementation_steps\":[\"Create module\"],\"testing_notes\":\"Add unit tests\"}]}"}}"#;
        let parsed = parse_task_generation_output(text).expect("task generation should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Build parser");
    }

    #[test]
    fn parses_task_generation_from_markdown_fence() {
        let text = r#"
```json
{
  "tasks": [
    {
      "title": "Create API endpoint",
      "description": "Add endpoint for uploads",
      "task_type": "feature",
      "complexity": "high",
      "estimated_files": ["src/api/upload.ts"],
      "implementation_steps": ["Create route", "Add validation"],
      "testing_notes": "Integration test with fixture upload"
    }
  ]
}
```
"#;
        let parsed = parse_task_generation_output(text).expect("task generation should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].complexity, "high");
    }
}
