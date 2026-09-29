//! The bundled `config/default-install.json`, with plugin tags filled in.
//!
//! The file lists the recommended packs (with their tags) and which plugins
//! make up the starter set, but not the plugins' tags: those live only in
//! `orchestrator_core::plugin_registry`, the same table `animus plugin
//! install-defaults` and the daemon preflight use. Every reader of the file
//! goes through [`default_install_json`] so all of them see the same tags.

use std::sync::OnceLock;

use orchestrator_core::resolve_tag_for_slug;
use serde_json::Value;

const RAW_DEFAULT_INSTALL_JSON: &str = include_str!("../../../config/default-install.json");

/// `default-install.json` with a `tag` added to every plugin entry from the
/// curated plugin registry.
pub(crate) fn default_install_json() -> &'static str {
    static RESOLVED: OnceLock<String> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let mut value: Value = serde_json::from_str(RAW_DEFAULT_INSTALL_JSON)
            .expect("bundled default-install.json must be valid JSON (checked by tests)");
        fill_plugin_tags(&mut value);
        serde_json::to_string(&value).expect("serialize default-install.json")
    })
}

/// Add each plugin entry's tag from the curated registry. An entry the
/// registry does not pin is left without a tag, and readers skip it; the
/// tests below keep that from happening to the bundled file.
fn fill_plugin_tags(value: &mut Value) {
    let Some(sections) = value.get_mut("plugins").and_then(Value::as_object_mut) else {
        return;
    };
    for entries in sections.values_mut() {
        let Some(entries) = entries.as_array_mut() else { continue };
        for entry in entries {
            let Some(repo) = entry.get("repo").and_then(Value::as_str) else { continue };
            if let (Some(tag), Some(object)) = (resolve_tag_for_slug(repo), entry.as_object_mut()) {
                object.insert("tag".to_string(), Value::String(tag.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_entries(value: &Value) -> Vec<&Value> {
        value["plugins"].as_object().expect("plugins section").values().flat_map(|v| v.as_array().unwrap()).collect()
    }

    #[test]
    fn bundled_file_leaves_plugin_tags_to_the_registry() {
        let raw: Value = serde_json::from_str(RAW_DEFAULT_INSTALL_JSON).expect("valid JSON");
        let entries = plugin_entries(&raw);
        assert!(!entries.is_empty(), "default-install.json must list plugins");
        for entry in entries {
            assert!(
                entry.get("tag").is_none(),
                "plugin tags belong in orchestrator-core's plugin_registry.rs, not default-install.json: {entry}"
            );
        }
    }

    #[test]
    fn every_listed_plugin_gets_the_registry_tag() {
        let resolved: Value = serde_json::from_str(default_install_json()).expect("valid JSON");
        for entry in plugin_entries(&resolved) {
            let repo = entry["repo"].as_str().expect("repo");
            let expected = resolve_tag_for_slug(repo).unwrap_or_else(|| {
                panic!("{repo} is listed in default-install.json but not pinned in plugin_registry.rs")
            });
            assert_eq!(entry["tag"].as_str(), Some(expected), "{repo}");
        }
    }

    #[test]
    fn packs_keep_their_own_tags() {
        let raw: Value = serde_json::from_str(RAW_DEFAULT_INSTALL_JSON).expect("valid JSON");
        let resolved: Value = serde_json::from_str(default_install_json()).expect("valid JSON");
        assert_eq!(raw["packs"], resolved["packs"]);
        assert!(resolved["packs"].as_array().is_some_and(|packs| packs.iter().all(|p| p["tag"].is_string())));
    }
}
