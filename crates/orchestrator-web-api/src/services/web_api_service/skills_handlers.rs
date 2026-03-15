use orchestrator_config::skill_resolution::{list_available_skills, resolve_skill};
use orchestrator_config::skill_scoping::load_skill_sources;
use serde_json::{json, Value};

use super::{WebApiError, WebApiService};

impl WebApiService {
    pub async fn skills_list(&self) -> Result<Value, WebApiError> {
        let project_root = std::path::Path::new(&self.context.project_root);
        let sources = load_skill_sources(project_root, None)
            .map_err(|e| WebApiError::new("internal", format!("failed to load skill sources: {e}"), 1))?;
        let skills = list_available_skills(&sources);
        let items: Vec<Value> = skills
            .iter()
            .map(|s| {
                let cat = s.definition.category.as_ref().map(|c| format!("{c:?}")).unwrap_or_default();
                json!({
                    "name": s.definition.name,
                    "description": s.definition.description,
                    "category": cat,
                    "source": s.source.to_string(),
                    "skillType": "definition",
                })
            })
            .collect();
        Ok(json!(items))
    }

    pub async fn skill_show(&self, name: &str) -> Result<Value, WebApiError> {
        let project_root = std::path::Path::new(&self.context.project_root);
        let sources = load_skill_sources(project_root, None)
            .map_err(|e| WebApiError::new("internal", format!("failed to load skill sources: {e}"), 1))?;
        let resolved = resolve_skill(name, &sources)
            .map_err(|e| WebApiError::new("not_found", format!("skill not found: {e}"), 3))?;
        let def_json = serde_json::to_value(&resolved.definition)
            .map_err(|e| WebApiError::new("internal", format!("failed to serialize skill: {e}"), 1))?;
        let cat = resolved.definition.category.as_ref().map(|c| format!("{c:?}")).unwrap_or_default();
        Ok(json!({
            "name": resolved.definition.name,
            "description": resolved.definition.description,
            "category": cat,
            "source": resolved.source.to_string(),
            "skillType": "definition",
            "definitionJson": serde_json::to_string_pretty(&def_json).unwrap_or_default(),
        }))
    }
}
