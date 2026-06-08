use serde::{Deserialize, Serialize};

/// Closed event surface. Every variant is a counter. Adding a new event
/// requires adding it here and updating the matching tag type — this is
/// the privacy boundary: callers cannot inject free-form event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventName {
    WorkflowStarted,
    WorkflowCompleted,
    PluginInstalled,
    DaemonStarted,
    ErrorHit,
    CliInvoked,
    UpdateApplied,
}

impl EventName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EventName::WorkflowStarted => "workflow_started",
            EventName::WorkflowCompleted => "workflow_completed",
            EventName::PluginInstalled => "plugin_installed",
            EventName::DaemonStarted => "daemon_started",
            EventName::ErrorHit => "error_hit",
            EventName::CliInvoked => "cli_invoked",
            EventName::UpdateApplied => "update_applied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowKind {
    Task,
    Requirement,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowOutcome {
    Success,
    Failure,
    Cancelled,
}

/// Plugin role taxonomy. Mirrors the role names used in the plugin protocol;
/// rendered into payloads under the `plugin_kind` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PluginRole {
    SubjectBackend,
    Provider,
    Transport,
    WebUi,
    Trigger,
    LogStorage,
    Queue,
    Notifier,
    WorkflowRunner,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorClass {
    ParseError,
    PreflightFailed,
    PluginCrash,
    NetworkError,
    Other,
}

/// Closed enum of root-level command groups. Anything not in this list
/// renders as `other` — callers cannot smuggle dynamic strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandGroup {
    Version,
    Daemon,
    Agent,
    Project,
    Queue,
    Workflow,
    History,
    Git,
    Skill,
    Model,
    Pack,
    Plugin,
    Runner,
    Status,
    Output,
    Mcp,
    Web,
    Init,
    Doctor,
    Trigger,
    Logs,
    Subject,
    Flavor,
    SelfUpdate,
    Metrics,
    Cost,
    Events,
    Other,
}

/// Closed tag union — one variant per event name. Forces serde to never
/// emit a user-supplied string as a tag value: every tag inhabitant is a
/// compile-time enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "name", content = "tags")]
pub(crate) enum EventTags {
    WorkflowStarted { workflow_kind: WorkflowKind },
    WorkflowCompleted { outcome: WorkflowOutcome },
    PluginInstalled { plugin_kind: PluginRole },
    DaemonStarted {},
    ErrorHit { error_class: ErrorClass },
    CliInvoked { command_group: CommandGroup },
    UpdateApplied {},
}

impl EventTags {
    pub(crate) fn event_name(&self) -> EventName {
        match self {
            EventTags::WorkflowStarted { .. } => EventName::WorkflowStarted,
            EventTags::WorkflowCompleted { .. } => EventName::WorkflowCompleted,
            EventTags::PluginInstalled { .. } => EventName::PluginInstalled,
            EventTags::DaemonStarted { .. } => EventName::DaemonStarted,
            EventTags::ErrorHit { .. } => EventName::ErrorHit,
            EventTags::CliInvoked { .. } => EventName::CliInvoked,
            EventTags::UpdateApplied { .. } => EventName::UpdateApplied,
        }
    }
}

/// On-disk JSONL entry. One per emitted event. The batcher folds entries
/// with the same `(name, tags)` shape into counter buckets at send time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    /// RFC 3339 timestamp when the event was recorded.
    pub recorded_at: String,
    /// Bounded-enum tag payload.
    #[serde(flatten)]
    pub tags: EventTags,
}
