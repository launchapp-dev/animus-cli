use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowRunnerConfig {
    pub(crate) unified_config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowRunnerPhaseResult {
    pub(crate) phase_id: String,
    pub(crate) exit_code: i32,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowRunnerResult {
    pub(crate) exit_code: i32,
    pub(crate) phase_results: Vec<WorkflowRunnerPhaseResult>,
    pub(crate) error_messages: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct WorkflowRunner;

impl WorkflowRunner {
    pub(crate) async fn run(
        task_id: String,
        pipeline_id: Option<String>,
        project_root: String,
        config_path: String,
    ) -> Result<WorkflowRunnerResult> {
        let config = WorkflowRunnerConfig::load_from_unified_yaml(Path::new(&config_path))?;
        let _ = (&task_id, &pipeline_id, &project_root, &config.unified_config);
        Ok(WorkflowRunnerResult {
            exit_code: 0,
            phase_results: Vec::new(),
            error_messages: Vec::new(),
        })
    }
}

impl WorkflowRunnerConfig {
    fn load_from_unified_yaml(path: &Path) -> Result<WorkflowRunnerConfig> {
        let content =
            fs::read_to_string(path).with_context(|| format!("read workflow config from {}", path.display()))?;
        let unified_config =
            serde_yaml::from_str(&content).with_context(|| {
                format!("parse unified workflow runner config from {}", path.display())
            })?;
        Ok(WorkflowRunnerConfig { unified_config })
    }
}
