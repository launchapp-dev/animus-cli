use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::skill_definition::SkillDefinition;
use crate::skill_scoping::{load_skill_sources, SkillSource, SkillSourceOrigin};

#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub definition: SkillDefinition,
    pub source: SkillSourceOrigin,
}

pub fn resolve_skill(name: &str, sources: &[SkillSource]) -> Result<ResolvedSkill> {
    for source in sources.iter().rev() {
        if let Some(def) = source.skills.get(name) {
            return Ok(ResolvedSkill {
                definition: def.clone(),
                source: source.origin.clone(),
            });
        }
    }

    let available: Vec<String> = list_available_skills(sources)
        .into_iter()
        .map(|r| r.definition.name)
        .collect();

    if available.is_empty() {
        bail!("skill '{}' not found (no skills available)", name);
    } else {
        bail!(
            "skill '{}' not found. Available skills: {}",
            name,
            available.join(", ")
        );
    }
}

pub fn resolve_skills(names: &[String], sources: &[SkillSource]) -> Result<Vec<ResolvedSkill>> {
    let mut resolved = Vec::with_capacity(names.len());
    for name in names {
        resolved.push(resolve_skill(name, sources)?);
    }
    Ok(resolved)
}

pub fn resolve_skills_for_project(
    names: &[String],
    project_root: &Path,
) -> Result<Vec<ResolvedSkill>> {
    let sources = load_skill_sources(project_root, None)?;
    resolve_skills(names, &sources)
}

pub fn list_available_skills(sources: &[SkillSource]) -> Vec<ResolvedSkill> {
    let mut map: BTreeMap<String, ResolvedSkill> = BTreeMap::new();

    for source in sources {
        for (name, def) in &source.skills {
            map.insert(
                name.clone(),
                ResolvedSkill {
                    definition: def.clone(),
                    source: source.origin.clone(),
                },
            );
        }
    }

    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str) -> SkillDefinition {
        serde_yaml::from_str(&format!("name: {}\ndescription: {} description\n", name, name))
            .unwrap()
    }

    fn make_source(origin: SkillSourceOrigin, skills: &[&str]) -> SkillSource {
        let mut map = BTreeMap::new();
        for name in skills {
            map.insert(name.to_string(), make_skill(name));
        }
        SkillSource {
            origin,
            skills: map,
        }
    }

    #[test]
    fn test_resolve_skill_found() {
        let sources = vec![
            make_source(SkillSourceOrigin::Builtin, &["alpha"]),
            make_source(SkillSourceOrigin::User, &["beta"]),
        ];

        let resolved = resolve_skill("alpha", &sources).unwrap();
        assert_eq!(resolved.definition.name, "alpha");
        assert_eq!(resolved.source, SkillSourceOrigin::Builtin);
    }

    #[test]
    fn test_resolve_skill_not_found() {
        let sources = vec![make_source(SkillSourceOrigin::Builtin, &["alpha"])];
        let err = resolve_skill("missing", &sources).unwrap_err();
        assert!(err.to_string().contains("missing"));
        assert!(err.to_string().contains("alpha"));
    }

    #[test]
    fn test_resolve_skill_not_found_empty() {
        let sources: Vec<SkillSource> = vec![];
        let err = resolve_skill("missing", &sources).unwrap_err();
        assert!(err.to_string().contains("no skills available"));
    }

    #[test]
    fn test_priority_project_over_user_over_builtin() {
        let sources = vec![
            make_source(SkillSourceOrigin::Builtin, &["shared"]),
            make_source(SkillSourceOrigin::User, &["shared"]),
            make_source(SkillSourceOrigin::Project, &["shared"]),
        ];

        let resolved = resolve_skill("shared", &sources).unwrap();
        assert_eq!(resolved.source, SkillSourceOrigin::Project);
    }

    #[test]
    fn test_priority_user_over_builtin() {
        let sources = vec![
            make_source(SkillSourceOrigin::Builtin, &["shared"]),
            make_source(SkillSourceOrigin::User, &["shared"]),
        ];

        let resolved = resolve_skill("shared", &sources).unwrap();
        assert_eq!(resolved.source, SkillSourceOrigin::User);
    }

    #[test]
    fn test_resolve_skills_all_found() {
        let sources = vec![
            make_source(SkillSourceOrigin::Builtin, &["a", "b"]),
            make_source(SkillSourceOrigin::User, &["c"]),
        ];

        let names = vec!["a".to_string(), "c".to_string()];
        let resolved = resolve_skills(&names, &sources).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].definition.name, "a");
        assert_eq!(resolved[1].definition.name, "c");
    }

    #[test]
    fn test_resolve_skills_first_failure_stops() {
        let sources = vec![make_source(SkillSourceOrigin::Builtin, &["a"])];
        let names = vec!["missing".to_string(), "a".to_string()];
        let err = resolve_skills(&names, &sources).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn test_list_available_skills_deduplicates() {
        let sources = vec![
            make_source(SkillSourceOrigin::Builtin, &["alpha", "beta"]),
            make_source(SkillSourceOrigin::User, &["beta", "gamma"]),
            make_source(SkillSourceOrigin::Project, &["gamma", "delta"]),
        ];

        let available = list_available_skills(&sources);
        let names: Vec<&str> = available.iter().map(|r| r.definition.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "delta", "gamma"]);

        let beta = available.iter().find(|r| r.definition.name == "beta").unwrap();
        assert_eq!(beta.source, SkillSourceOrigin::User);

        let gamma = available.iter().find(|r| r.definition.name == "gamma").unwrap();
        assert_eq!(gamma.source, SkillSourceOrigin::Project);
    }

    #[test]
    fn test_list_available_skills_empty() {
        let available = list_available_skills(&[]);
        assert!(available.is_empty());
    }
}
