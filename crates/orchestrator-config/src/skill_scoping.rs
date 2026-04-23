use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::skill_definition::{
    parse_skill_manifest, validate_skill_definition, SkillActivation, SkillDefinition, SkillModelPreference,
    SkillPrompt,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSourceOrigin {
    Builtin,
    Installed { registry: String, source: String, version: String, integrity: String, artifact: String },
    User,
    Project,
}

impl std::fmt::Display for SkillSourceOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillSourceOrigin::Builtin => write!(f, "built-in"),
            SkillSourceOrigin::Installed { .. } => write!(f, "installed"),
            SkillSourceOrigin::User => write!(f, "user"),
            SkillSourceOrigin::Project => write!(f, "project"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillSource {
    pub origin: SkillSourceOrigin,
    pub skills: BTreeMap<String, SkillDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledSkillRecord {
    pub name: String,
    pub version: String,
    pub source: String,
    pub registry: String,
    pub integrity: String,
    pub artifact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<SkillDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InstalledSkillRegistryStateV1 {
    #[serde(default)]
    installed: Vec<InstalledSkillRecord>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MarkdownSkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    metadata: MarkdownSkillMetadata,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MarkdownSkillMetadata {
    #[serde(default)]
    version: Option<String>,
}

pub fn load_skill_sources(project_root: &Path, user_config_dir: Option<&Path>) -> Result<Vec<SkillSource>> {
    let mut sources = Vec::new();

    let builtin = load_builtin_skills()?;
    sources.push(builtin);

    for entry in load_installed_skill_entries(project_root)? {
        let Some(mut definition) = entry.definition.clone() else {
            continue;
        };
        definition.name = entry.name.clone();
        sources.push(SkillSource {
            origin: SkillSourceOrigin::Installed {
                registry: entry.registry.clone(),
                source: entry.source.clone(),
                version: entry.version.clone(),
                integrity: entry.integrity.clone(),
                artifact: entry.artifact.clone(),
            },
            skills: BTreeMap::from([(entry.name.clone(), definition)]),
        });
    }

    let user_dir = match user_config_dir {
        Some(dir) => dir.join("config").join("skill_definitions"),
        None => user_skills_dir(),
    };
    let user_markdown_dir = match user_config_dir {
        Some(dir) => dir.join("skills"),
        None => user_markdown_skills_dir(),
    };
    let user_skills = merge_skill_scope_sources(&user_markdown_dir, &user_dir)?;
    if !user_skills.is_empty() {
        sources.push(SkillSource { origin: SkillSourceOrigin::User, skills: user_skills });
    }

    let proj_dir = project_skills_dir(project_root);
    let project_markdown_dir = project_markdown_skills_dir(project_root);
    let project_skills = merge_skill_scope_sources(&project_markdown_dir, &proj_dir)?;
    if !project_skills.is_empty() {
        sources.push(SkillSource { origin: SkillSourceOrigin::Project, skills: project_skills });
    }

    Ok(sources)
}

fn merge_skill_scope_sources(markdown_dir: &Path, yaml_dir: &Path) -> Result<BTreeMap<String, SkillDefinition>> {
    let mut skills = load_markdown_skills_from_directory(markdown_dir)?;
    skills.extend(load_skills_from_directory(yaml_dir)?);
    Ok(skills)
}

fn installed_skills_registry_path(project_root: &Path) -> PathBuf {
    let scoped_root = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    scoped_root.join("state").join("skills-registry.v1.json")
}

fn compare_installed_versions_desc(left: &str, right: &str) -> Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => right.cmp(left),
    }
}

pub fn load_installed_skill_entries(project_root: &Path) -> Result<Vec<InstalledSkillRecord>> {
    let path = installed_skills_registry_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path)?;
    let mut state: InstalledSkillRegistryStateV1 = serde_json::from_str(&content)?;
    state.installed.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| compare_installed_versions_desc(&left.version, &right.version))
            .then_with(|| left.registry.cmp(&right.registry))
            .then_with(|| right.version.cmp(&left.version))
            .then_with(|| left.integrity.cmp(&right.integrity))
            .then_with(|| left.artifact.cmp(&right.artifact))
    });
    state.installed.dedup_by(|left, right| left.name == right.name && left.source == right.source);
    Ok(state.installed)
}

pub fn load_skills_from_directory(dir: &Path) -> Result<BTreeMap<String, SkillDefinition>> {
    let mut skills = BTreeMap::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(skills),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: could not read skill file {}: {}", path.display(), e);
                continue;
            }
        };

        if let Ok(manifest) = parse_skill_manifest(&content) {
            for (name, def) in manifest.skills {
                skills.insert(name, def);
            }
            continue;
        }

        match serde_yaml::from_str::<SkillDefinition>(&content) {
            Ok(def) => {
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
                skills.insert(name, def);
            }
            Err(e) => {
                eprintln!("warning: could not parse skill file {}: {}", path.display(), e);
            }
        }
    }

    Ok(skills)
}

fn load_markdown_skills_from_directory(dir: &Path) -> Result<BTreeMap<String, SkillDefinition>> {
    let mut skills = BTreeMap::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(skills),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = markdown_skill_file_for_path(&entry.path());
        if !path.is_file() {
            continue;
        }

        match load_markdown_skill_file(&path) {
            Ok(skill) => {
                skills.insert(skill.name.clone(), skill);
            }
            Err(error) => {
                eprintln!("warning: could not parse markdown skill {}: {}", path.display(), error);
            }
        }
    }

    Ok(skills)
}

pub fn markdown_skill_file_for_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    }
}

pub fn load_markdown_skill_file(path: &Path) -> Result<SkillDefinition> {
    let content = fs::read_to_string(path)?;
    let default_name = markdown_skill_default_name(path);
    parse_markdown_skill_definition(&content, &default_name)
}

fn markdown_skill_default_name(path: &Path) -> String {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let candidate = if file_name.eq_ignore_ascii_case("SKILL.md") {
        path.parent().and_then(|parent| parent.file_name()).and_then(|name| name.to_str())
    } else {
        path.file_stem().and_then(|name| name.to_str())
    };

    candidate.filter(|name| !name.trim().is_empty()).unwrap_or("unknown").to_string()
}

pub fn parse_markdown_skill_definition(content: &str, default_name: &str) -> Result<SkillDefinition> {
    let normalized = content.replace("\r\n", "\n");
    let normalized = normalized.trim_start_matches('\u{feff}');
    let (frontmatter, body) = split_markdown_frontmatter(normalized);
    let metadata = match frontmatter {
        Some(frontmatter) => serde_yaml::from_str::<MarkdownSkillFrontmatter>(frontmatter)?,
        None => MarkdownSkillFrontmatter::default(),
    };

    let name =
        metadata.name.as_deref().map(str::trim).filter(|value| !value.is_empty()).unwrap_or(default_name).to_string();
    let description = metadata.description.unwrap_or_default().trim().to_string();
    let prompt_body = body.trim();

    let skill = SkillDefinition {
        name,
        version: metadata.version.or(metadata.metadata.version),
        description,
        category: None,
        activation: SkillActivation::default(),
        prompt: SkillPrompt {
            system: (!prompt_body.is_empty()).then(|| prompt_body.to_string()),
            prefix: None,
            suffix: None,
            directives: Vec::new(),
        },
        tool_policy: None,
        model: SkillModelPreference::default(),
        mcp_servers: Vec::new(),
        timeout_secs: None,
        capabilities: BTreeMap::new(),
        extra_args: Vec::new(),
        env: BTreeMap::new(),
        codex_config_overrides: Vec::new(),
        adapters: BTreeMap::new(),
        tags: Vec::new(),
    };
    validate_skill_definition(&skill)?;
    Ok(skill)
}

fn split_markdown_frontmatter(content: &str) -> (Option<&str>, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (None, content);
    };

    if let Some(idx) = rest.find("\n---\n") {
        let frontmatter = &rest[..idx];
        let body = &rest[idx + 5..];
        return (Some(frontmatter), body);
    }

    if let Some(frontmatter) = rest.strip_suffix("\n---") {
        return (Some(frontmatter), "");
    }

    (None, content)
}

pub fn project_skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join("config").join("skill_definitions")
}

pub fn project_markdown_skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join("skills")
}

pub fn user_skills_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".ao").join("config").join("skill_definitions")
}

pub fn user_markdown_skills_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".ao").join("skills")
}

const BUILTIN_SKILL_YAMLS: &[(&str, &str)] = &[
    ("implementation", include_str!("../config/skills/implementation.yaml")),
    ("debugging", include_str!("../config/skills/debugging.yaml")),
    ("refactoring", include_str!("../config/skills/refactoring.yaml")),
    ("unit-testing", include_str!("../config/skills/unit-testing.yaml")),
    // These aliases keep existing persona/task references valid until dedicated skill content exists.
    ("testing", include_str!("../config/skills/unit-testing.yaml")),
    ("code-review", include_str!("../config/skills/code-review.yaml")),
    ("deep-search", include_str!("../config/skills/deep-search.yaml")),
    ("code-analysis", include_str!("../config/skills/code-analysis.yaml")),
    ("architecture-review", include_str!("../config/skills/architecture-review.yaml")),
    ("impact-analysis", include_str!("../config/skills/impact-analysis.yaml")),
    ("technical-writing", include_str!("../config/skills/technical-writing.yaml")),
    ("api-documentation", include_str!("../config/skills/api-documentation.yaml")),
    ("task-decomposition", include_str!("../config/skills/task-decomposition.yaml")),
    ("prioritization", include_str!("../config/skills/prioritization.yaml")),
    ("queue-management", include_str!("../config/skills/prioritization.yaml")),
    ("scheduling", include_str!("../config/skills/prioritization.yaml")),
    ("risk-management", include_str!("../config/skills/impact-analysis.yaml")),
    ("vision-alignment", include_str!("../config/skills/technical-writing.yaml")),
    ("requirements-management", include_str!("../config/skills/task-decomposition.yaml")),
    ("acceptance-criteria", include_str!("../config/skills/task-decomposition.yaml")),
    ("deliverable-validation", include_str!("../config/skills/code-review.yaml")),
    ("incident-response", include_str!("../config/skills/incident-response.yaml")),
    ("ci-cd-authoring", include_str!("../config/skills/ci-cd-authoring.yaml")),
    ("release-management", include_str!("../config/skills/release-management.yaml")),
    ("pr-summary", include_str!("../config/skills/pr-summary.yaml")),
    ("changelog-generation", include_str!("../config/skills/changelog-generation.yaml")),
    ("security-audit", include_str!("../config/skills/security-audit.yaml")),
];

pub fn load_builtin_skills() -> Result<SkillSource> {
    let mut skills = BTreeMap::new();
    for (name, yaml) in BUILTIN_SKILL_YAMLS {
        let mut def: SkillDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse built-in skill '{}': {}", name, e))?;
        def.name = name.to_string();
        skills.insert(name.to_string(), def);
    }
    Ok(SkillSource { origin: SkillSourceOrigin::Builtin, skills })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, EnvVarGuard};
    use std::fs;
    use tempfile::TempDir;

    fn write_manifest_yaml(dir: &Path, filename: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn test_load_skills_from_directory_with_manifest() {
        let tmp = TempDir::new().unwrap();
        let yaml = r#"
schema: "ao.skills.v1"
skills:
  greet:
    name: greet
    description: A greeting skill
  farewell:
    name: farewell
    description: A farewell skill
"#;
        write_manifest_yaml(tmp.path(), "skills.yaml", yaml);

        let skills = load_skills_from_directory(tmp.path()).unwrap();
        assert_eq!(skills.len(), 2);
        assert!(skills.contains_key("greet"));
        assert!(skills.contains_key("farewell"));
    }

    #[test]
    fn test_load_skills_from_directory_single_definition() {
        let tmp = TempDir::new().unwrap();
        let yaml = r#"
name: solo
description: A standalone skill
"#;
        write_manifest_yaml(tmp.path(), "solo.yml", yaml);

        let skills = load_skills_from_directory(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert!(skills.contains_key("solo"));
    }

    #[test]
    fn test_load_skills_from_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let skills = load_skills_from_directory(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_from_missing_directory() {
        let skills = load_skills_from_directory(Path::new("/nonexistent/path")).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_skips_non_yaml_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("readme.txt"), "not a skill").unwrap();
        fs::write(tmp.path().join("data.json"), "{}").unwrap();

        let skills = load_skills_from_directory(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_skips_unparseable_yaml() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("bad.yaml"), "not: valid: skill: yaml: [[").unwrap();

        let skills = load_skills_from_directory(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_markdown_skills_from_directory_with_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("rust-skills");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: rust-skills
description: Rust-specific guidance
metadata:
  version: "1.2.3"
---

# Rust Skill

Use this skill when editing Rust code.
"#,
        )
        .unwrap();

        let skills = load_markdown_skills_from_directory(tmp.path()).unwrap();
        let skill = skills.get("rust-skills").expect("markdown skill should load");
        assert_eq!(skill.description, "Rust-specific guidance");
        assert_eq!(skill.version.as_deref(), Some("1.2.3"));
        assert!(skill.prompt.system.as_deref().is_some_and(|body| body.contains("# Rust Skill")));
    }

    #[test]
    fn test_load_markdown_skills_from_directory_with_direct_md_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("review.md"),
            r#"---
description: Review guidance
---

# Review Skill

Check behavior before style.
"#,
        )
        .unwrap();

        let skills = load_markdown_skills_from_directory(tmp.path()).unwrap();
        let skill = skills.get("review").expect("direct markdown skill should load");
        assert_eq!(skill.name, "review");
        assert_eq!(skill.description, "Review guidance");
        assert!(skill.prompt.system.as_deref().is_some_and(|body| body.contains("Check behavior before style.")));
    }

    #[test]
    fn test_project_skills_dir() {
        let dir = project_skills_dir(Path::new("/repo"));
        assert_eq!(dir, PathBuf::from("/repo/.ao/config/skill_definitions"));
    }

    #[test]
    fn test_project_markdown_skills_dir() {
        let dir = project_markdown_skills_dir(Path::new("/repo"));
        assert_eq!(dir, PathBuf::from("/repo/.ao/skills"));
    }

    #[test]
    fn test_load_builtin_skills() {
        let source = load_builtin_skills().unwrap();
        assert_eq!(source.origin, SkillSourceOrigin::Builtin);
        assert_eq!(source.skills.len(), 27);
        assert!(source.skills.contains_key("implementation"));
        assert!(source.skills.contains_key("code-review"));
        assert!(source.skills.contains_key("deep-search"));
        assert!(source.skills.contains_key("security-audit"));
        assert!(source.skills.contains_key("prioritization"));
        assert!(source.skills.contains_key("testing"));
        assert!(source.skills.contains_key("queue-management"));
    }

    #[test]
    fn test_all_builtin_skills_validate() {
        use crate::skill_definition::validate_skill_definition;
        let source = load_builtin_skills().unwrap();
        for (name, def) in &source.skills {
            validate_skill_definition(def)
                .unwrap_or_else(|e| panic!("Built-in skill '{}' failed validation: {}", name, e));
        }
    }

    #[test]
    fn test_skill_source_origin_display() {
        assert_eq!(SkillSourceOrigin::Builtin.to_string(), "built-in");
        assert_eq!(
            SkillSourceOrigin::Installed {
                registry: "project".to_string(),
                source: "demo".to_string(),
                version: "1.0.0".to_string(),
                integrity: "sha256:test".to_string(),
                artifact: "demo-1.0.0.tgz".to_string(),
            }
            .to_string(),
            "installed"
        );
        assert_eq!(SkillSourceOrigin::User.to_string(), "user");
        assert_eq!(SkillSourceOrigin::Project.to_string(), "project");
    }

    #[test]
    fn test_load_skill_sources_with_project_skills() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = project_skills_dir(tmp.path());
        let yaml = r#"
name: proj-skill
description: Project skill
"#;
        write_manifest_yaml(&skill_dir, "proj.yaml", yaml);

        let sources = load_skill_sources(tmp.path(), None).unwrap();
        assert!(sources.len() >= 2);
        assert_eq!(sources[0].origin, SkillSourceOrigin::Builtin);
        let project_source = sources.iter().find(|s| s.origin == SkillSourceOrigin::Project);
        assert!(project_source.is_some());
        assert!(project_source.unwrap().skills.contains_key("proj"));
    }

    #[test]
    fn test_load_skill_sources_with_project_markdown_skills() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = project_markdown_skills_dir(tmp.path()).join("rust-skills");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: rust-skills
description: Rust local skill
---

# Rust Local Skill

Prefer borrowing over cloning.
"#,
        )
        .unwrap();

        let sources = load_skill_sources(tmp.path(), None).unwrap();
        let project_source = sources.iter().find(|source| source.origin == SkillSourceOrigin::Project);
        let project_source = project_source.expect("project markdown source should be present");
        let skill = project_source.skills.get("rust-skills").expect("markdown skill should resolve");
        assert_eq!(skill.description, "Rust local skill");
        assert!(skill.prompt.system.as_deref().is_some_and(|body| body.contains("Prefer borrowing over cloning")));
    }

    #[test]
    fn test_load_skill_sources_includes_installed_skill_snapshots() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = TempDir::new().unwrap();
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let tmp = TempDir::new().unwrap();
        let state_dir = protocol::scoped_state_root(tmp.path()).unwrap_or_else(|| tmp.path().join(".ao")).join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("skills-registry.v1.json"),
            serde_json::json!({
                "installed": [
                    {
                        "name": "registry-review",
                        "version": "1.2.3",
                        "source": "acme",
                        "registry": "project",
                        "integrity": "sha256:abc",
                        "artifact": "registry-review-1.2.3.tgz",
                        "definition": {
                            "name": "ignored-name",
                            "description": "Registry-backed skill"
                        }
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let sources = load_skill_sources(tmp.path(), None).unwrap();
        let installed = sources
            .iter()
            .find(|source| matches!(source.origin, SkillSourceOrigin::Installed { .. }))
            .expect("installed source should be present");
        assert!(installed.skills.contains_key("registry-review"));
        assert_eq!(installed.skills["registry-review"].name, "registry-review");
    }

    #[test]
    fn test_load_installed_skill_entries_prefers_semver_latest() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = TempDir::new().unwrap();
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let tmp = TempDir::new().unwrap();
        let state_dir = protocol::scoped_state_root(tmp.path()).unwrap_or_else(|| tmp.path().join(".ao")).join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("skills-registry.v1.json"),
            serde_json::json!({
                "installed": [
                    {
                        "name": "registry-review",
                        "version": "9.0.0",
                        "source": "acme",
                        "registry": "project",
                        "integrity": "sha256:old",
                        "artifact": "registry-review-9.0.0.tgz"
                    },
                    {
                        "name": "registry-review",
                        "version": "10.0.0",
                        "source": "acme",
                        "registry": "project",
                        "integrity": "sha256:new",
                        "artifact": "registry-review-10.0.0.tgz"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let installed = load_installed_skill_entries(tmp.path()).unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "10.0.0");
    }
}
