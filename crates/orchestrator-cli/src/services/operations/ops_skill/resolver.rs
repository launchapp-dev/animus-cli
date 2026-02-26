use anyhow::{anyhow, Result};
use semver::{Version, VersionReq};
use std::cmp::Ordering;

use super::model::{SkillLockEntry, SkillProjectConstraint, SkillVersionRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstraintOrigin {
    Cli,
    Lock,
    Project,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionConstraintOrigin {
    Cli,
    Lock,
    Project,
    None,
}

#[derive(Debug, Clone)]
pub(super) struct ResolveSkillRequest<'a> {
    pub(super) name: &'a str,
    pub(super) cli_version: Option<&'a str>,
    pub(super) cli_source: Option<&'a str>,
    pub(super) cli_registry: Option<&'a str>,
    pub(super) allow_prerelease: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ResolveSkillResult {
    pub(super) selected: SkillVersionRecord,
    pub(super) used_lock_pin: bool,
    pub(super) used_project_default: bool,
}

fn parse_version_req(raw: &str) -> Result<VersionReq> {
    VersionReq::parse(raw).map_err(|error| anyhow!("invalid version constraint '{raw}': {error}"))
}

fn parse_exact_version_req(version: &str) -> Result<VersionReq> {
    parse_version_req(&format!("={version}"))
}

fn compare_semver_desc(left: &str, right: &str) -> Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => right.cmp(left),
    }
}

fn version_matches(requirement: &VersionReq, version: &str) -> bool {
    Version::parse(version)
        .map(|parsed| requirement.matches(&parsed))
        .unwrap_or(false)
}

fn is_stable_release(version: &str) -> bool {
    Version::parse(version)
        .map(|parsed| parsed.pre.is_empty())
        .unwrap_or(false)
}

fn winner_sort(left: &SkillVersionRecord, right: &SkillVersionRecord) -> Ordering {
    compare_semver_desc(&left.version, &right.version)
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.registry.cmp(&right.registry))
        .then_with(|| right.version.cmp(&left.version))
        .then_with(|| left.integrity.cmp(&right.integrity))
        .then_with(|| left.artifact.cmp(&right.artifact))
}

pub(super) fn resolve_skill_version(
    request: &ResolveSkillRequest<'_>,
    catalog: &[SkillVersionRecord],
    lock_pin: Option<&SkillLockEntry>,
    project_default: Option<&SkillProjectConstraint>,
) -> Result<ResolveSkillResult> {
    let name = request.name.trim();
    if name.is_empty() {
        anyhow::bail!("invalid skill name");
    }

    let mut candidates: Vec<&SkillVersionRecord> = catalog
        .iter()
        .filter(|record| record.name == name)
        .collect();
    if candidates.is_empty() {
        anyhow::bail!("skill not found: {name}");
    }

    let (source_constraint, source_origin) = match request.cli_source {
        Some(source) => (Some(source.to_string()), ConstraintOrigin::Cli),
        None => match lock_pin {
            Some(pin) => (Some(pin.source.clone()), ConstraintOrigin::Lock),
            None => match project_default.and_then(|item| item.source.clone()) {
                Some(source) => (Some(source), ConstraintOrigin::Project),
                None => (None, ConstraintOrigin::None),
            },
        },
    };
    if let Some(source) = source_constraint.as_deref() {
        candidates.retain(|record| record.source == source);
    }

    let (registry_constraint, registry_origin) = match request.cli_registry {
        Some(registry) => (Some(registry.to_string()), ConstraintOrigin::Cli),
        None => match lock_pin
            .and_then(|pin| pin.registry.clone())
            .or_else(|| project_default.and_then(|item| item.registry.clone()))
        {
            Some(registry) => {
                if lock_pin
                    .and_then(|pin| pin.registry.as_deref())
                    .is_some_and(|candidate| candidate == registry)
                {
                    (Some(registry), ConstraintOrigin::Lock)
                } else {
                    (Some(registry), ConstraintOrigin::Project)
                }
            }
            None => (None, ConstraintOrigin::None),
        },
    };
    if let Some(registry) = registry_constraint.as_deref() {
        candidates.retain(|record| record.registry == registry);
    }

    if candidates.is_empty() {
        anyhow::bail!("skill not found for source/registry constraints: {name}");
    }

    let (version_constraint, version_origin): (Option<VersionReq>, VersionConstraintOrigin) =
        match request.cli_version {
            Some(version) => (
                Some(parse_version_req(version)?),
                VersionConstraintOrigin::Cli,
            ),
            None => match lock_pin {
                Some(pin) => (
                    Some(parse_exact_version_req(&pin.version)?),
                    VersionConstraintOrigin::Lock,
                ),
                None => match project_default.and_then(|item| item.version.as_deref()) {
                    Some(version) => (
                        Some(parse_version_req(version)?),
                        VersionConstraintOrigin::Project,
                    ),
                    None => (None, VersionConstraintOrigin::None),
                },
            },
        };

    if let Some(constraint) = version_constraint.as_ref() {
        let before = candidates.len();
        candidates.retain(|record| version_matches(constraint, &record.version));
        if before > 0 && candidates.is_empty() {
            match version_origin {
                VersionConstraintOrigin::Cli => {
                    anyhow::bail!(
                        "invalid version constraint unsatisfied for skill '{}': {}",
                        name,
                        request.cli_version.unwrap_or_default()
                    );
                }
                VersionConstraintOrigin::Lock => {
                    anyhow::bail!("skill version not found for lock pin: {}", name);
                }
                VersionConstraintOrigin::Project => {
                    anyhow::bail!("skill version not found for project default: {}", name);
                }
                VersionConstraintOrigin::None => {}
            }
        }
    }

    let allow_prerelease = request.allow_prerelease
        || project_default
            .map(|item| item.allow_prerelease)
            .unwrap_or(false);
    if !allow_prerelease {
        let stable_candidates: Vec<&SkillVersionRecord> = candidates
            .iter()
            .copied()
            .filter(|record| is_stable_release(&record.version))
            .collect();
        if !stable_candidates.is_empty() {
            candidates = stable_candidates;
        }
    }

    candidates.sort_by(|left, right| winner_sort(left, right));
    let selected = candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("skill not found: {name}"))?
        .clone();

    let used_lock_pin = matches!(source_origin, ConstraintOrigin::Lock)
        || matches!(registry_origin, ConstraintOrigin::Lock)
        || matches!(version_origin, VersionConstraintOrigin::Lock);
    let used_project_default = matches!(source_origin, ConstraintOrigin::Project)
        || matches!(registry_origin, ConstraintOrigin::Project)
        || matches!(version_origin, VersionConstraintOrigin::Project);

    Ok(ResolveSkillResult {
        selected,
        used_lock_pin,
        used_project_default,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_record(
        name: &str,
        version: &str,
        source: &str,
        registry: &str,
    ) -> SkillVersionRecord {
        SkillVersionRecord {
            name: name.to_string(),
            version: version.to_string(),
            source: source.to_string(),
            registry: registry.to_string(),
            integrity: format!("sha256:{name}:{version}:{source}"),
            artifact: format!("{name}-{version}.tgz"),
        }
    }

    #[test]
    fn resolver_prefers_cli_constraints_over_lock_and_project_defaults() {
        let catalog = vec![
            catalog_record("lint", "1.2.0", "s1", "project"),
            catalog_record("lint", "1.3.0", "s2", "project"),
        ];
        let lock_pin = SkillLockEntry {
            name: "lint".to_string(),
            version: "1.2.0".to_string(),
            source: "s1".to_string(),
            integrity: "sha256:old".to_string(),
            artifact: "lint-1.2.0.tgz".to_string(),
            registry: Some("project".to_string()),
        };
        let project_default = SkillProjectConstraint {
            name: "lint".to_string(),
            version: Some("=1.2.0".to_string()),
            source: Some("s1".to_string()),
            registry: Some("project".to_string()),
            allow_prerelease: false,
        };
        let request = ResolveSkillRequest {
            name: "lint",
            cli_version: Some("=1.3.0"),
            cli_source: Some("s2"),
            cli_registry: Some("project"),
            allow_prerelease: false,
        };

        let resolved =
            resolve_skill_version(&request, &catalog, Some(&lock_pin), Some(&project_default))
                .expect("resolution should succeed");
        assert_eq!(resolved.selected.version, "1.3.0");
        assert_eq!(resolved.selected.source, "s2");
        assert!(!resolved.used_lock_pin);
        assert!(!resolved.used_project_default);
    }

    #[test]
    fn resolver_prefers_stable_releases_when_prerelease_not_allowed() {
        let catalog = vec![
            catalog_record("fmt", "2.0.0-beta.1", "source-a", "project"),
            catalog_record("fmt", "1.9.0", "source-a", "project"),
        ];
        let request = ResolveSkillRequest {
            name: "fmt",
            cli_version: None,
            cli_source: None,
            cli_registry: None,
            allow_prerelease: false,
        };

        let resolved = resolve_skill_version(&request, &catalog, None, None)
            .expect("resolution should succeed");
        assert_eq!(resolved.selected.version, "1.9.0");
    }

    #[test]
    fn resolver_uses_lexical_source_tie_break_for_equal_versions() {
        let catalog = vec![
            catalog_record("build", "3.1.0", "zeta", "project"),
            catalog_record("build", "3.1.0", "alpha", "project"),
        ];
        let request = ResolveSkillRequest {
            name: "build",
            cli_version: Some("=3.1.0"),
            cli_source: None,
            cli_registry: Some("project"),
            allow_prerelease: true,
        };

        let resolved = resolve_skill_version(&request, &catalog, None, None)
            .expect("resolution should succeed");
        assert_eq!(resolved.selected.source, "alpha");
    }

    #[test]
    fn resolver_selects_highest_semver_when_multiple_candidates_match() {
        let catalog = vec![
            catalog_record("test", "1.4.0", "s", "project"),
            catalog_record("test", "1.6.0", "s", "project"),
            catalog_record("test", "1.5.1", "s", "project"),
        ];
        let request = ResolveSkillRequest {
            name: "test",
            cli_version: None,
            cli_source: Some("s"),
            cli_registry: Some("project"),
            allow_prerelease: false,
        };

        let resolved = resolve_skill_version(&request, &catalog, None, None)
            .expect("resolution should succeed");
        assert_eq!(resolved.selected.version, "1.6.0");
    }

    #[test]
    fn resolver_uses_lock_pin_before_project_default_when_cli_omits_constraints() {
        let catalog = vec![
            catalog_record("lint", "1.0.0", "lock-source", "project"),
            catalog_record("lint", "2.0.0", "project-default-source", "project"),
        ];
        let lock_pin = SkillLockEntry {
            name: "lint".to_string(),
            version: "1.0.0".to_string(),
            source: "lock-source".to_string(),
            integrity: "sha256:lock".to_string(),
            artifact: "lint-1.0.0.tgz".to_string(),
            registry: Some("project".to_string()),
        };
        let project_default = SkillProjectConstraint {
            name: "lint".to_string(),
            version: Some("=2.0.0".to_string()),
            source: Some("project-default-source".to_string()),
            registry: Some("project".to_string()),
            allow_prerelease: false,
        };
        let request = ResolveSkillRequest {
            name: "lint",
            cli_version: None,
            cli_source: None,
            cli_registry: None,
            allow_prerelease: false,
        };

        let resolved =
            resolve_skill_version(&request, &catalog, Some(&lock_pin), Some(&project_default))
                .expect("resolution should succeed");
        assert_eq!(resolved.selected.version, "1.0.0");
        assert_eq!(resolved.selected.source, "lock-source");
        assert!(resolved.used_lock_pin);
        assert!(!resolved.used_project_default);
    }

    #[test]
    fn resolver_uses_project_default_when_lock_pin_is_missing() {
        let catalog = vec![
            catalog_record("scan", "1.0.0", "default-source", "project"),
            catalog_record("scan", "2.0.0", "other-source", "project"),
        ];
        let project_default = SkillProjectConstraint {
            name: "scan".to_string(),
            version: Some("=1.0.0".to_string()),
            source: Some("default-source".to_string()),
            registry: Some("project".to_string()),
            allow_prerelease: false,
        };
        let request = ResolveSkillRequest {
            name: "scan",
            cli_version: None,
            cli_source: None,
            cli_registry: None,
            allow_prerelease: false,
        };

        let resolved = resolve_skill_version(&request, &catalog, None, Some(&project_default))
            .expect("resolution should succeed");
        assert_eq!(resolved.selected.version, "1.0.0");
        assert_eq!(resolved.selected.source, "default-source");
        assert!(!resolved.used_lock_pin);
        assert!(resolved.used_project_default);
    }
}
