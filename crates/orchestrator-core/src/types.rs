pub use protocol::orchestrator::{
    AgentHandoffRequestInput, AgentHandoffResult, AgentHandoffStatus, ArchitectureEdge, ArchitectureEntity,
    ArchitectureGraph, Assignee, CheckpointReason, Complexity, ComplexityAssessment, ComplexityTier, DaemonHealth,
    DaemonStatus, ImpactArea, ListPage, ListPageRequest, LogEntry, LogLevel, OrchestratorProject, OrchestratorWorkflow,
    PhaseDecision, PhaseDecisionVerdict, PhaseEvidence, PhaseEvidenceKind, Priority, ProjectConcurrencyLimits, SkipReason,
    ProjectConfig, ProjectCreateInput, ProjectMetadata, ProjectModelPreferences, ProjectType, RequirementQuery,
    RequirementQuerySort, RequirementRange, RiskLevel, Scope, SubjectDispatch, SubjectRef, TaskDensity, TaskQuery,
    TaskQuerySort, TaskStatus, TaskType, VisionDocument, VisionDraftInput, WorkflowCheckpoint,
    WorkflowCheckpointMetadata, WorkflowDecisionAction, WorkflowDecisionRecord, WorkflowDecisionRisk,
    WorkflowDecisionSource, WorkflowFilter, WorkflowMachineEvent, WorkflowMachineState, WorkflowPhaseExecution,
    WorkflowPhaseStatus, WorkflowQuery, WorkflowQuerySort, WorkflowRunInput, WorkflowStatus, WorkflowSubject,
    DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT, SUBJECT_KIND_CUSTOM, SUBJECT_KIND_REQUIREMENT, SUBJECT_KIND_TASK,
};

pub use protocol::orchestrator::{
    is_frontend_related_content, ChecklistItem, CodebaseInsight, DependencyType, DispatchHistoryEntry,
    HandoffTargetRole, OrchestratorTask, RequirementComment, RequirementFilter, RequirementItem, RequirementLinks,
    RequirementStatus, RequirementsDraftInput, RequirementsDraftResult, RequirementsExecutionInput,
    RequirementsExecutionResult, RequirementsRefineInput, ResourceRequirements, TaskCreateInput, TaskDependency,
    TaskFilter, TaskMetadata, TaskPriorityDistribution, TaskPriorityPolicyReport, TaskPriorityRebalanceChange,
    TaskPriorityRebalanceOptions, TaskPriorityRebalancePlan, TaskStatistics, TaskUpdateInput, WorkflowMetadata,
    MAX_DISPATCH_HISTORY_ENTRIES,
};

pub use protocol::RequirementPriority;
pub use protocol::RequirementType;
pub use protocol::{ClassifiedError, ErrorKind};

pub fn not_found(message: impl Into<String>) -> anyhow::Error {
    ClassifiedError::new(ErrorKind::NotFound, message).into()
}

pub fn invalid_input(message: impl Into<String>) -> anyhow::Error {
    ClassifiedError::new(ErrorKind::InvalidInput, message).into()
}

pub fn conflict(message: impl Into<String>) -> anyhow::Error {
    ClassifiedError::new(ErrorKind::Conflict, message).into()
}

pub trait RequirementPriorityExt {
    #[must_use]
    fn to_task_priority(self) -> Priority;
}

impl RequirementPriorityExt for RequirementPriority {
    fn to_task_priority(self) -> Priority {
        match self {
            RequirementPriority::Must => Priority::High,
            RequirementPriority::Should => Priority::Medium,
            RequirementPriority::Could | RequirementPriority::Wont => Priority::Low,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PhaseDecision, PhaseDecisionVerdict, PhaseEvidence, PhaseEvidenceKind, Priority, RequirementPriority,
        RequirementPriorityExt, TaskStatus, TaskType, WorkflowDecisionRisk,
    };
    use serde_json::json;

    #[test]
    fn requirement_priority_to_task_priority_mapping_is_stable() {
        assert_eq!(RequirementPriority::Must.to_task_priority(), Priority::High);
        assert_eq!(RequirementPriority::Should.to_task_priority(), Priority::Medium);
        assert_eq!(RequirementPriority::Could.to_task_priority(), Priority::Low);
        assert_eq!(RequirementPriority::Wont.to_task_priority(), Priority::Low);
    }

    #[test]
    fn phase_decision_deserializes_with_expected_defaults() {
        let input = json!({
            "kind": "phase_decision",
            "phase_id": "testing",
            "verdict": "advance",
            "confidence": 0.96,
            "risk": "low"
        });

        let parsed: PhaseDecision =
            serde_json::from_value(input).expect("phase decision should parse with optional fields omitted");

        assert_eq!(parsed.kind, "phase_decision");
        assert_eq!(parsed.phase_id, "testing");
        assert_eq!(parsed.verdict, PhaseDecisionVerdict::Advance);
        assert_eq!(parsed.confidence, 0.96);
        assert_eq!(parsed.risk, WorkflowDecisionRisk::Low);
        assert!(parsed.reason.is_empty());
        assert!(parsed.evidence.is_empty());
        assert!(parsed.guardrail_violations.is_empty());
        assert!(parsed.commit_message.is_none());
    }

    #[test]
    fn phase_decision_deserializes_unknown_verdict_with_fallback() {
        let input = json!({
            "kind": "phase_decision",
            "phase_id": "code-review",
            "verdict": "escalate",
            "confidence": 0.51,
            "risk": "medium"
        });

        let parsed: PhaseDecision =
            serde_json::from_value(input).expect("unknown verdict should map to fallback variant");

        assert_eq!(parsed.verdict, PhaseDecisionVerdict::Unknown);
        assert_eq!(parsed.phase_id, "code-review");
        assert_eq!(parsed.risk, WorkflowDecisionRisk::Medium);
    }

    #[test]
    fn phase_decision_serializes_with_evidence_payload() {
        let decision = PhaseDecision {
            kind: "phase_decision".to_string(),
            phase_id: "testing".to_string(),
            verdict: PhaseDecisionVerdict::Advance,
            confidence: 0.99,
            risk: WorkflowDecisionRisk::Low,
            reason: "All required checks passed".to_string(),
            evidence: vec![PhaseEvidence {
                kind: PhaseEvidenceKind::TestsPassed,
                description: "cargo test -p orchestrator-core".to_string(),
                file_path: Some("crates/orchestrator-core/src/types.rs".to_string()),
                value: Some(json!({ "tests": 2 })),
            }],
            guardrail_violations: vec![],
            commit_message: Some("test: validate phase decision contract".to_string()),
            target_phase: None,
            skip_reason: None,
        };

        let serialized = serde_json::to_value(&decision).expect("phase decision should serialize successfully");

        assert_eq!(serialized["kind"], "phase_decision");
        assert_eq!(serialized["verdict"], "advance");
        assert_eq!(serialized["risk"], "low");
        assert_eq!(serialized["evidence"][0]["kind"], "tests_passed");
        assert_eq!(serialized["evidence"][0]["description"], "cargo test -p orchestrator-core");
        assert_eq!(serialized["evidence"][0]["value"]["tests"], 2);
        assert_eq!(serialized["commit_message"], "test: validate phase decision contract");
    }

    #[test]
    fn task_status_deserializes_contract_aliases_and_helpers_stay_consistent() {
        let backlog_alias: TaskStatus = serde_json::from_str("\"todo\"").expect("todo should map to backlog");
        let in_progress_kebab: TaskStatus = serde_json::from_str("\"in-progress\"").expect("kebab case should parse");
        let in_progress_snake: TaskStatus = serde_json::from_str("\"in_progress\"").expect("snake case should parse");
        let on_hold_snake: TaskStatus = serde_json::from_str("\"on_hold\"").expect("on_hold alias should parse");
        let done_alias: TaskStatus = serde_json::from_str("\"completed\"").expect("completed should map to done");

        assert_eq!(backlog_alias, TaskStatus::Backlog);
        assert_eq!(in_progress_kebab, TaskStatus::InProgress);
        assert_eq!(in_progress_snake, TaskStatus::InProgress);
        assert_eq!(on_hold_snake, TaskStatus::OnHold);
        assert_eq!(done_alias, TaskStatus::Done);

        assert!(TaskStatus::InProgress.is_active());
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Blocked.is_blocked());
        assert!(TaskStatus::OnHold.is_blocked());
    }

    #[test]
    fn task_type_as_str_matches_canonical_serialization_and_aliases() {
        let variants = [
            TaskType::Feature,
            TaskType::Bugfix,
            TaskType::Hotfix,
            TaskType::Refactor,
            TaskType::Docs,
            TaskType::Test,
            TaskType::Chore,
            TaskType::Experiment,
        ];

        for task_type in variants {
            let serialized = serde_json::to_string(&task_type).expect("task type should serialize to canonical string");
            assert_eq!(serialized, format!("\"{}\"", task_type.as_str()));
        }

        let bug_alias: TaskType = serde_json::from_str("\"bug\"").expect("bug alias should parse");
        let docs_alias: TaskType = serde_json::from_str("\"documentation\"").expect("documentation alias should parse");
        let tests_alias: TaskType = serde_json::from_str("\"tests\"").expect("tests alias should parse");

        assert_eq!(bug_alias, TaskType::Bugfix);
        assert_eq!(docs_alias, TaskType::Docs);
        assert_eq!(tests_alias, TaskType::Test);
    }
}
