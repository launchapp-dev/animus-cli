use super::*;

mod complexity;
mod requirements;

pub(super) use complexity::{effective_complexity_assessment, infer_complexity_assessment};
pub(super) use requirements::{
    build_requirement_candidates, missing_vision_constraint_coverage,
    requirement_matches_id_filter, task_priority_for_requirement,
};

pub(super) fn next_requirement_id(requirements: &HashMap<String, RequirementItem>) -> String {
    let next_seq = requirements
        .keys()
        .filter_map(|req_id| req_id.strip_prefix("REQ-"))
        .filter_map(|seq| seq.parse::<u32>().ok())
        .max()
        .map_or(1, |max_seq| max_seq.saturating_add(1));
    format!("REQ-{next_seq:03}")
}

pub(super) fn default_vision_project_name(project_root: &Path) -> String {
    project_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "Project".to_string())
}

pub(super) fn build_vision_markdown(
    project_name: &str,
    problem_statement: &str,
    target_users: &[String],
    goals: &[String],
    constraints: &[String],
    value_proposition: Option<&str>,
    complexity_assessment: Option<&ComplexityAssessment>,
) -> String {
    let users = if target_users.is_empty() {
        "- TBD".to_string()
    } else {
        target_users
            .iter()
            .map(|value| format!("- {}", value.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let goals = if goals.is_empty() {
        "- Define measurable user outcomes.".to_string()
    } else {
        goals
            .iter()
            .map(|value| format!("- {}", value.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let constraints = if constraints.is_empty() {
        "- No explicit constraints captured yet.".to_string()
    } else {
        constraints
            .iter()
            .map(|value| format!("- {}", value.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let problem = if problem_statement.trim().is_empty() {
        "Problem statement pending".to_string()
    } else {
        problem_statement.trim().to_string()
    };

    let value_line = value_proposition
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Value proposition to be refined with stakeholders.");

    let complexity_block = complexity_assessment
        .map(|assessment| {
            let tier = match assessment.tier {
                ComplexityTier::Simple => "simple",
                ComplexityTier::Medium => "medium",
                ComplexityTier::Complex => "complex",
            };
            let task_density = match assessment.task_density {
                TaskDensity::Low => "low",
                TaskDensity::Medium => "medium",
                TaskDensity::High => "high",
            };
            let confidence = assessment.confidence.clamp(0.0, 1.0);
            let rationale = assessment
                .rationale
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("No rationale captured.");
            format!(
                "\n## Complexity\n- Tier: {tier}\n- Confidence: {:.2}\n- Recommended requirement range: {}-{}\n- Task density: {task_density}\n- Rationale: {}\n",
                confidence,
                assessment.recommended_requirement_range.min,
                assessment.recommended_requirement_range.max,
                rationale,
            )
        })
        .unwrap_or_default();

    format!(
        "# Product Vision\n\n## Project\n- Name: {}\n\n## Problem\n{}\n\n## Target Users\n{}\n\n## Goals\n{}\n\n## Constraints\n{}\n\n## Value Proposition\n{}\n{}\n",
        project_name.trim(),
        problem,
        users,
        goals,
        constraints,
        value_line,
        complexity_block,
    )
}

fn parse_json_file(path: &Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&content).ok()
}

pub(super) fn collect_codebase_insight(project_root: &Path) -> CodebaseInsight {
    let mut insight = CodebaseInsight::default();

    for path in [
        "src",
        "crates/agent-runner",
        "crates/llm-cli-wrapper",
        "crates",
        "tests",
    ] {
        if project_root.join(path).exists() {
            insight.notable_paths.push(path.to_string());
        }
    }

    let package_json_path = project_root.join("package.json");
    if package_json_path.exists() {
        insight.detected_stacks.push("nodejs".to_string());
        if let Some(pkg) = parse_json_file(&package_json_path) {
            let mut deps = serde_json::Map::new();
            if let Some(values) = pkg.get("dependencies").and_then(|value| value.as_object()) {
                deps.extend(values.clone());
            }
            if let Some(values) = pkg
                .get("devDependencies")
                .and_then(|value| value.as_object())
            {
                deps.extend(values.clone());
            }

            if deps.contains_key("react") {
                insight.detected_stacks.push("react".to_string());
            }
            if deps.contains_key("vite") {
                insight.detected_stacks.push("vite".to_string());
            }
            if deps.contains_key("typescript") {
                insight.detected_stacks.push("typescript".to_string());
            }
        }
    }

    let cargo_toml_path = project_root.join("Cargo.toml");
    if cargo_toml_path.exists() {
        insight.detected_stacks.push("rust".to_string());
        if let Ok(content) = std::fs::read_to_string(cargo_toml_path) {
        }
    }

    let mut stack_set = std::collections::BTreeSet::new();
    insight
        .detected_stacks
        .retain(|stack| stack_set.insert(stack.clone()));

    let mut file_count = 0usize;
    let mut stack = vec![project_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if file_count >= 5_000 {
            break;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();

            if path.is_dir() {
                if file_name == ".git"
                    || file_name == ".ao"
                    || file_name == "node_modules"
                    || file_name == "target"
                    || file_name == "dist"
                {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                file_count = file_count.saturating_add(1);
                if file_count >= 5_000 {
                    break;
                }
            }
        }
    }
    insight.file_count_scanned = file_count;

    insight
}

fn planning_docs_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join("docs")
}

fn vision_doc_path(project_root: &Path) -> PathBuf {
    planning_docs_dir(project_root).join("product-vision.md")
}

pub(super) fn write_planning_artifacts(
    project_root: &Path,
    vision: Option<&VisionDocument>,
    _requirements: &HashMap<String, RequirementItem>,
) -> Result<()> {
    let docs_dir = planning_docs_dir(project_root);
    std::fs::create_dir_all(&docs_dir)?;

    if let Some(vision) = vision {
        std::fs::write(
            vision_doc_path(project_root),
            format!("{}\n", vision.markdown.trim()),
        )?;
    }

    Ok(())
}
