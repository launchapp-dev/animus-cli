use anyhow::Result;
use std::path::PathBuf;

use super::super::{project_state_dir, read_json_or_default, write_json_pretty};
use super::model::{SkillLockStateV1, SkillRegistryStateV1};

fn skills_registry_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("skills-registry.v1.json")
}

fn skills_lock_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("skills-lock.v1.json")
}

pub(super) fn load_skill_registry_state(project_root: &str) -> Result<SkillRegistryStateV1> {
    let mut state = read_json_or_default::<SkillRegistryStateV1>(&skills_registry_path(project_root))?;
    state.normalize();
    Ok(state)
}

pub(super) fn save_skill_registry_state_if_changed(project_root: &str, state: &SkillRegistryStateV1) -> Result<bool> {
    let path = skills_registry_path(project_root);
    let mut next = state.clone();
    next.normalize();
    let mut current = read_json_or_default::<SkillRegistryStateV1>(&path)?;
    current.normalize();
    if path.exists() && current == next {
        return Ok(false);
    }
    write_json_pretty(&path, &next)?;
    Ok(true)
}

pub(super) fn load_skill_lock_state(project_root: &str) -> Result<SkillLockStateV1> {
    let mut state = read_json_or_default::<SkillLockStateV1>(&skills_lock_path(project_root))?;
    state.normalize();
    Ok(state)
}

pub(super) fn save_skill_lock_state_if_changed(project_root: &str, state: &SkillLockStateV1) -> Result<bool> {
    let path = skills_lock_path(project_root);
    let mut next = state.clone();
    next.normalize();
    let mut current = read_json_or_default::<SkillLockStateV1>(&path)?;
    current.normalize();
    if path.exists() && current == next {
        return Ok(false);
    }
    write_json_pretty(&path, &next)?;
    Ok(true)
}
