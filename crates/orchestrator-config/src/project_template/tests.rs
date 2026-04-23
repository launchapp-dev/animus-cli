use std::fs;
use std::path::Path;

use super::{
    list_bundled_project_templates, load_bundled_project_template, load_project_template_from_dir,
    parse_project_template_manifest, PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID,
};

#[test]
fn bundled_project_templates_list_contains_first_party_patterns() {
    let templates = list_bundled_project_templates().expect("bundled templates should load");
    let ids = templates.into_iter().map(|template| template.id).collect::<Vec<_>>();
    assert!(ids.contains(&"conductor".to_string()));
    assert!(ids.contains(&"task-queue".to_string()));
    assert!(ids.contains(&"direct-workflow".to_string()));
}

#[test]
fn bundled_project_template_loads_manifest_and_files() {
    let template = load_bundled_project_template("task-queue").expect("task-queue template should load");
    assert_eq!(template.manifest.pattern, "task-queue");
    assert!(template.files.iter().any(|file| file.relative_path == Path::new(".ao/workflows/standard-workflow.yaml")));
}

#[test]
fn local_project_template_loads_recursive_skeleton() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    fs::write(
        temp.path().join("template.toml"),
        format!(
            r#"schema = "{PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID}"
id = "local-template"
version = "0.1.0"
title = "Local Template"
description = "Local test template"
pattern = "local"

[source]
mode = "copy"
root = "skeleton"
"#
        ),
    )
    .expect("template manifest should be written");
    let workflows_dir = temp.path().join("skeleton").join(".ao").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("skeleton directories should exist");
    fs::write(workflows_dir.join("custom.yaml"), b"default_workflow_ref: standard-workflow\n")
        .expect("template file should be written");

    let template = load_project_template_from_dir(temp.path()).expect("local template should load");
    assert_eq!(template.manifest.id, "local-template");
    assert_eq!(template.files.len(), 1);
    assert_eq!(template.files[0].relative_path, Path::new(".ao/workflows/custom.yaml"));
}

#[test]
fn project_template_manifest_rejects_invalid_schema() {
    let error = parse_project_template_manifest(
        r#"
schema = "wrong"
id = "invalid"
version = "0.1.0"
title = "Invalid"
pattern = "invalid"

[source]
mode = "copy"
root = "skeleton"
"#,
    )
    .expect_err("invalid schema should fail");
    assert!(error.to_string().contains("project template schema"));
}
