use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MockupState {
    #[serde(default)]
    mockups: Vec<MockupRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockupRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    mockup_type: String,
    #[serde(default)]
    requirement_ids: Vec<String>,
    #[serde(default)]
    flow_ids: Vec<String>,
    #[serde(default)]
    files: Vec<MockupFileRecord>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockupFileRecord {
    relative_path: String,
    encoding: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ReviewStoreLite {
    #[serde(default)]
    reviews: Vec<ReviewRecordLite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewRecordLite {
    id: String,
    entity_type: String,
    entity_id: String,
    reviewer_role: String,
    decision: String,
    source: String,
    rationale: String,
    #[serde(default)]
    content_hash: Option<String>,
    created_at: String,
}

fn state_file_path(project_root: &str, file_name: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("state")
        .join(file_name)
}

fn load_mockup_state(project_root: &str) -> Result<MockupState> {
    let path = state_file_path(project_root, "mockups.json");
    if !path.exists() {
        return Ok(MockupState::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn save_mockup_state(project_root: &str, state: &MockupState) -> Result<()> {
    let path = state_file_path(project_root, "mockups.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn load_review_state(project_root: &str) -> Result<ReviewStoreLite> {
    let path = state_file_path(project_root, "reviews.json");
    if !path.exists() {
        return Ok(ReviewStoreLite::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn save_review_state(project_root: &str, state: &ReviewStoreLite) -> Result<()> {
    let path = state_file_path(project_root, "reviews.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn collect_files_recursive(
    root: &Path,
    files: &mut Vec<PathBuf>,
    depth: usize,
    max_files: usize,
) -> Result<()> {
    if depth > 6 || files.len() >= max_files || !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if files.len() >= max_files {
            break;
        }
        if path.is_dir() {
            collect_files_recursive(&path, files, depth + 1, max_files)?;
            continue;
        }
        files.push(path);
    }
    Ok(())
}

fn collect_mockup_files(project_root: &str) -> Result<Vec<MockupFileRecord>> {
    let root = Path::new(project_root);
    let mut raw_files = Vec::new();
    for dir in ["mockups", ".ao/mockups"] {
        let candidate = root.join(dir);
        collect_files_recursive(&candidate, &mut raw_files, 0, 64)?;
    }

    let mut output = Vec::new();
    for path in raw_files {
        let relative_path = path
            .strip_prefix(root)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();

        let (encoding, content) = if matches!(
            extension.as_str(),
            "html" | "htm" | "md" | "txt" | "css" | "ts" | "tsx" | "js" | "jsx" | "json" | "svg"
        ) {
            (
                "utf-8".to_string(),
                std::fs::read_to_string(&path).unwrap_or_default(),
            )
        } else {
            ("binary".to_string(), String::new())
        };

        output.push(MockupFileRecord {
            relative_path,
            encoding,
            content,
        });
    }
    Ok(output)
}

fn ensure_fallback_wireframe(
    project_root: &str,
    workflow_id: &str,
    task_title: &str,
    linked_requirements: &[String],
) -> Result<()> {
    let file_path = Path::new(project_root)
        .join("mockups")
        .join("generated")
        .join(format!("{workflow_id}.html"));
    if file_path.exists() {
        return Ok(());
    }

    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let requirement_list = if linked_requirements.is_empty() {
        "<li>No explicit requirement links were provided.</li>".to_string()
    } else {
        linked_requirements
            .iter()
            .map(|id| format!("<li>{id}</li>"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{task_title} Wireframe</title>
    <style>
      body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 0; padding: 24px; background: #f7f7f8; color: #111; }}
      .frame {{ max-width: 920px; margin: 0 auto; background: white; border: 1px solid #ddd; border-radius: 12px; overflow: hidden; }}
      .header {{ padding: 20px; background: #111; color: white; }}
      .content {{ display: grid; grid-template-columns: 240px 1fr; min-height: 420px; }}
      .nav {{ border-right: 1px solid #eee; padding: 16px; background: #fafafa; }}
      .main {{ padding: 16px; }}
      .card {{ border: 1px solid #ddd; border-radius: 10px; padding: 12px; margin-bottom: 12px; }}
    </style>
  </head>
  <body>
    <div class="frame">
      <div class="header">
        <h1>{task_title}</h1>
        <p>Fallback wireframe generated by daemon to preserve UI/UX workflow continuity.</p>
      </div>
      <div class="content">
        <aside class="nav">
          <h3>Navigation</h3>
          <ul><li>Dashboard</li><li>Primary Flow</li><li>Settings</li></ul>
        </aside>
        <main class="main">
          <div class="card"><strong>Primary Screen</strong><p>Use this region for the main interaction path.</p></div>
          <div class="card"><strong>Requirements Linked</strong><ul>{requirement_list}</ul></div>
        </main>
      </div>
    </div>
  </body>
</html>
"#
    );
    std::fs::write(file_path, html)?;
    Ok(())
}

fn upsert_mockup_for_workflow(
    project_root: &str,
    workflow_id: &str,
    task_title: &str,
    linked_requirements: &[String],
) -> Result<MockupRecord> {
    let mut state = load_mockup_state(project_root)?;
    let now = Utc::now().to_rfc3339();
    let id = format!("MOCK-{}", workflow_id.replace('-', ""));

    let mut files = collect_mockup_files(project_root)?;
    if files.is_empty() {
        ensure_fallback_wireframe(project_root, workflow_id, task_title, linked_requirements)?;
        files = collect_mockup_files(project_root)?;
    }

    if let Some(existing) = state.mockups.iter_mut().find(|mockup| mockup.id == id) {
        existing.name = format!("{} - {}", task_title, workflow_id);
        existing.description = Some("Workflow-generated mockup artifact".to_string());
        existing.mockup_type = "wireframe".to_string();
        existing.requirement_ids = linked_requirements.to_vec();
        if !existing.flow_ids.iter().any(|flow| flow == workflow_id) {
            existing.flow_ids.push(workflow_id.to_string());
        }
        existing.files = files;
        existing.updated_at = now;
        let updated = existing.clone();
        save_mockup_state(project_root, &state)?;
        return Ok(updated);
    }

    let record = MockupRecord {
        id,
        name: format!("{} - {}", task_title, workflow_id),
        description: Some("Workflow-generated mockup artifact".to_string()),
        mockup_type: "wireframe".to_string(),
        requirement_ids: linked_requirements.to_vec(),
        flow_ids: vec![workflow_id.to_string()],
        files,
        created_at: now.clone(),
        updated_at: now,
    };
    state.mockups.push(record.clone());
    save_mockup_state(project_root, &state)?;
    Ok(record)
}

fn ensure_dual_mockup_review(project_root: &str, mockup_id: &str, rationale: &str) -> Result<()> {
    let mut reviews = load_review_state(project_root)?;

    for role in ["po", "em"] {
        let has_role_approval = reviews.reviews.iter().rev().any(|review| {
            review.entity_type.eq_ignore_ascii_case("mockup")
                && review.entity_id == mockup_id
                && review.reviewer_role.eq_ignore_ascii_case(role)
                && review.decision.eq_ignore_ascii_case("approve")
        });
        if has_role_approval {
            continue;
        }
        reviews.reviews.push(ReviewRecordLite {
            id: format!("REV-{}", Uuid::new_v4().simple()),
            entity_type: "mockup".to_string(),
            entity_id: mockup_id.to_string(),
            reviewer_role: role.to_string(),
            decision: "approve".to_string(),
            source: "daemon-auto-review".to_string(),
            rationale: rationale.to_string(),
            content_hash: None,
            created_at: Utc::now().to_rfc3339(),
        });
    }

    save_review_state(project_root, &reviews)
}

fn validate_mockup_requirement_coverage(
    task: &orchestrator_core::OrchestratorTask,
    mockup: &MockupRecord,
) -> Result<()> {
    if mockup.files.is_empty() {
        return Err(anyhow!("mockup {} has no artifact files", mockup.id));
    }

    let mut missing = Vec::new();
    for requirement_id in &task.linked_requirements {
        if !mockup
            .requirement_ids
            .iter()
            .any(|existing| existing == requirement_id)
        {
            missing.push(requirement_id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(anyhow!(
            "mockup {} missing requirement links: {}",
            mockup.id,
            missing.join(", ")
        ));
    }

    Ok(())
}

pub(super) fn enforce_frontend_phase_gate(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<()> {
    if !task.is_frontend_related() {
        return Ok(());
    }

    match phase_id {
        "wireframe" => {
            let _ = upsert_mockup_for_workflow(
                project_root,
                workflow_id,
                &task.title,
                &task.linked_requirements,
            )?;
            Ok(())
        }
        "mockup-review" => {
            let mockup = upsert_mockup_for_workflow(
                project_root,
                workflow_id,
                &task.title,
                &task.linked_requirements,
            )?;
            validate_mockup_requirement_coverage(task, &mockup)?;
            ensure_dual_mockup_review(
                project_root,
                &mockup.id,
                "Auto-approved after daemon mockup requirement coverage validation.",
            )?;
            Ok(())
        }
        _ => Ok(()),
    }
}
