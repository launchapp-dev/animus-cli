use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::skill_definition::{parse_skill_manifest, SkillDefinition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSourceOrigin {
    Builtin,
    User,
    Project,
}

impl std::fmt::Display for SkillSourceOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillSourceOrigin::Builtin => write!(f, "built-in"),
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

pub fn load_skill_sources(project_root: &Path, user_config_dir: Option<&Path>) -> Result<Vec<SkillSource>> {
    let mut sources = Vec::new();

    let builtin = load_builtin_skills()?;
    sources.push(builtin);

    let user_dir = match user_config_dir {
        Some(dir) => dir.join("config").join("skill_definitions"),
        None => user_skills_dir(),
    };
    if user_dir.is_dir() {
        let skills = load_skills_from_directory(&user_dir)?;
        if !skills.is_empty() {
            sources.push(SkillSource { origin: SkillSourceOrigin::User, skills });
        }
    }

    let proj_dir = project_skills_dir(project_root);
    if proj_dir.is_dir() {
        let skills = load_skills_from_directory(&proj_dir)?;
        if !skills.is_empty() {
            sources.push(SkillSource { origin: SkillSourceOrigin::Project, skills });
        }
    }

    Ok(sources)
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

pub fn project_skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join("config").join("skill_definitions")
}

pub fn user_skills_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".ao").join("config").join("skill_definitions")
}

const BUILTIN_SKILL_YAMLS: &[(&str, &str)] = &[
    ("implementation", include_str!("../config/skills/implementation.yaml")),
    ("debugging", include_str!("../config/skills/debugging.yaml")),
    ("refactoring", include_str!("../config/skills/refactoring.yaml")),
    ("unit-testing", include_str!("../config/skills/unit-testing.yaml")),
    ("code-review", include_str!("../config/skills/code-review.yaml")),
    ("deep-search", include_str!("../config/skills/deep-search.yaml")),
    ("code-analysis", include_str!("../config/skills/code-analysis.yaml")),
    ("architecture-review", include_str!("../config/skills/architecture-review.yaml")),
    ("impact-analysis", include_str!("../config/skills/impact-analysis.yaml")),
    ("technical-writing", include_str!("../config/skills/technical-writing.yaml")),
    ("api-documentation", include_str!("../config/skills/api-documentation.yaml")),
    ("task-decomposition", include_str!("../config/skills/task-decomposition.yaml")),
    ("prioritization", include_str!("../config/skills/prioritization.yaml")),
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
        let def: SkillDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse built-in skill '{}': {}", name, e))?;
        skills.insert(name.to_string(), def);
    }
    Ok(SkillSource { origin: SkillSourceOrigin::Builtin, skills })
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_project_skills_dir() {
        let dir = project_skills_dir(Path::new("/repo"));
        assert_eq!(dir, PathBuf::from("/repo/.ao/config/skill_definitions"));
    }

    #[test]
    fn test_load_builtin_skills() {
        let source = load_builtin_skills().unwrap();
        assert_eq!(source.origin, SkillSourceOrigin::Builtin);
        assert_eq!(source.skills.len(), 19);
        assert!(source.skills.contains_key("implementation"));
        assert!(source.skills.contains_key("code-review"));
        assert!(source.skills.contains_key("deep-search"));
        assert!(source.skills.contains_key("security-audit"));
        assert!(source.skills.contains_key("prioritization"));
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
}
