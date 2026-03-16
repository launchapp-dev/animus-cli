use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use protocol::{SubjectExecutionFact, SUBJECT_KIND_CUSTOM, SUBJECT_KIND_REQUIREMENT, SUBJECT_KIND_TASK};

use crate::services::ServiceHub;

use super::project_task_execution_fact;

#[async_trait]
pub trait ExecutionProjector: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn project(&self, hub: Arc<dyn ServiceHub>, root: &str, fact: &SubjectExecutionFact) -> Result<()>;
}

#[derive(Clone, Default)]
pub struct ExecutionProjectorRegistry {
    projectors: HashMap<String, Arc<dyn ExecutionProjector>>,
}

impl ExecutionProjectorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn register(mut self, projector: Arc<dyn ExecutionProjector>) -> Self {
        self.projectors.insert(projector.kind().to_string(), projector);
        self
    }

    pub async fn project(&self, hub: Arc<dyn ServiceHub>, root: &str, fact: &SubjectExecutionFact) -> Result<bool> {
        let Some(projector) = self.projector_for_fact(fact) else {
            return Ok(false);
        };
        projector.project(hub, root, fact).await?;
        Ok(true)
    }

    fn projector_for_fact(&self, fact: &SubjectExecutionFact) -> Option<&Arc<dyn ExecutionProjector>> {
        execution_fact_subject_kind(fact).and_then(|kind| self.projectors.get(kind))
    }
}

#[must_use]
pub fn builtin_execution_projector_registry() -> ExecutionProjectorRegistry {
    ExecutionProjectorRegistry::new()
        .register(Arc::new(TaskExecutionProjector))
        .register(Arc::new(NoopExecutionProjector::new(SUBJECT_KIND_REQUIREMENT)))
        .register(Arc::new(NoopExecutionProjector::new(SUBJECT_KIND_CUSTOM)))
}

pub fn execution_fact_subject_kind(fact: &SubjectExecutionFact) -> Option<&str> {
    fact.subject_kind.as_deref().or_else(|| {
        if fact.task_id.is_some() {
            Some(SUBJECT_KIND_TASK)
        } else if fact.schedule_id.is_some() || fact.subject_id.starts_with("schedule:") {
            Some(SUBJECT_KIND_CUSTOM)
        } else {
            None
        }
    })
}

struct TaskExecutionProjector;

#[async_trait]
impl ExecutionProjector for TaskExecutionProjector {
    fn kind(&self) -> &'static str {
        SUBJECT_KIND_TASK
    }

    async fn project(&self, hub: Arc<dyn ServiceHub>, root: &str, fact: &SubjectExecutionFact) -> Result<()> {
        project_task_execution_fact(hub, root, fact).await;
        Ok(())
    }
}

struct NoopExecutionProjector {
    kind: &'static str,
}

impl NoopExecutionProjector {
    const fn new(kind: &'static str) -> Self {
        Self { kind }
    }
}

#[async_trait]
impl ExecutionProjector for NoopExecutionProjector {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn project(&self, _hub: Arc<dyn ServiceHub>, _root: &str, _fact: &SubjectExecutionFact) -> Result<()> {
        Ok(())
    }
}
