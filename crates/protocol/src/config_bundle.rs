use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::orchestrator::OrchestratorTask;
use crate::orchestrator::RequirementItem;

/// Represents a bundle of project configuration files to be pushed to cloud.
/// Includes workflow YAML files, project configuration, tasks, and requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigBundle {
    /// Config files indexed by relative path from project root
    pub files: BTreeMap<String, String>,
    /// Task objects
    #[serde(default)]
    pub tasks: Vec<OrchestratorTask>,
    /// Requirement objects
    #[serde(default)]
    pub requirements: Vec<RequirementItem>,
}

impl ConfigBundle {
    /// Create a new empty config bundle
    pub fn new() -> Self {
        Self { files: BTreeMap::new(), tasks: Vec::new(), requirements: Vec::new() }
    }

    /// Add a file to the bundle
    pub fn add_file(&mut self, path: String, content: String) {
        self.files.insert(path, content);
    }

    /// Add tasks to the bundle
    pub fn set_tasks(&mut self, tasks: Vec<OrchestratorTask>) {
        self.tasks = tasks;
    }

    /// Add requirements to the bundle
    pub fn set_requirements(&mut self, requirements: Vec<RequirementItem>) {
        self.requirements = requirements;
    }

    /// Check if bundle has any content
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.tasks.is_empty() && self.requirements.is_empty()
    }

    /// Get number of files in bundle
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Get number of tasks in bundle
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Get number of requirements in bundle
    pub fn requirement_count(&self) -> usize {
        self.requirements.len()
    }
}

impl Default for ConfigBundle {
    fn default() -> Self {
        Self::new()
    }
}
