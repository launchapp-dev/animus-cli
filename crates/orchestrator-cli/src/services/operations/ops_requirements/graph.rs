use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::state::{project_state_dir, read_json_or_default, write_json_pretty};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct RequirementsGraphState {
    #[serde(default)]
    nodes: Vec<Value>,
    #[serde(default)]
    edges: Vec<Value>,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
}

fn requirements_graph_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("requirements-graph.json")
}

pub(super) fn load_requirements_graph(project_root: &str) -> Result<RequirementsGraphState> {
    read_json_or_default(&requirements_graph_path(project_root))
}

pub(super) fn save_requirements_graph(project_root: &str, graph: &RequirementsGraphState) -> Result<()> {
    write_json_pretty(&requirements_graph_path(project_root), graph)
}
