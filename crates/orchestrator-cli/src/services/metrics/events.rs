//! Closed enums for opt-in metrics event names and tags.
//!
//! Every event name and every tag value is a compile-time enum. No
//! user-supplied string can reach the payload — that is the load-bearing
//! privacy invariant. See `docs/reference/configuration.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventName {
    WorkflowStarted,
    WorkflowCompleted,
    PluginInstalled,
    DaemonStarted,
    ErrorHit,
    CliInvoked,
    UpdateApplied,
}

impl EventName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowStarted => "workflow_started",
            Self::WorkflowCompleted => "workflow_completed",
            Self::PluginInstalled => "plugin_installed",
            Self::DaemonStarted => "daemon_started",
            Self::ErrorHit => "error_hit",
            Self::CliInvoked => "cli_invoked",
            Self::UpdateApplied => "update_applied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Task,
    Requirement,
    Custom,
}

impl WorkflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Requirement => "requirement",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcome {
    Success,
    Failure,
    Cancelled,
}

impl WorkflowOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    SubjectBackend,
    Provider,
    Transport,
    WebUi,
    Trigger,
    LogStorage,
    Queue,
    Notifier,
    WorkflowRunner,
    AgentRunner,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubjectBackend => "subject_backend",
            Self::Provider => "provider",
            Self::Transport => "transport",
            Self::WebUi => "web_ui",
            Self::Trigger => "trigger",
            Self::LogStorage => "log_storage",
            Self::Queue => "queue",
            Self::Notifier => "notifier",
            Self::WorkflowRunner => "workflow_runner",
            Self::AgentRunner => "agent_runner",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    ParseError,
    PreflightFailed,
    PluginCrash,
    NetworkError,
    Other,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::PreflightFailed => "preflight_failed",
            Self::PluginCrash => "plugin_crash",
            Self::NetworkError => "network_error",
            Self::Other => "other",
        }
    }
}

/// Closed enum of root-level command names. Matches the top-level
/// `Command` variants in `cli_types::root_types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandGroup {
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
    Metrics,
}

impl CommandGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Daemon => "daemon",
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Queue => "queue",
            Self::Workflow => "workflow",
            Self::History => "history",
            Self::Git => "git",
            Self::Skill => "skill",
            Self::Model => "model",
            Self::Pack => "pack",
            Self::Plugin => "plugin",
            Self::Runner => "runner",
            Self::Status => "status",
            Self::Output => "output",
            Self::Mcp => "mcp",
            Self::Web => "web",
            Self::Init => "init",
            Self::Doctor => "doctor",
            Self::Trigger => "trigger",
            Self::Logs => "logs",
            Self::Subject => "subject",
            Self::Flavor => "flavor",
            Self::Metrics => "metrics",
        }
    }
}

/// A single event emission. Tags carry bounded enums only — strings
/// never reach the payload from user input.
#[derive(Debug, Clone)]
pub enum Event {
    WorkflowStarted { kind: WorkflowKind },
    WorkflowCompleted { outcome: WorkflowOutcome },
    PluginInstalled { kind: PluginKind },
    DaemonStarted,
    ErrorHit { class: ErrorClass },
    CliInvoked { group: CommandGroup },
    UpdateApplied,
}

impl Event {
    pub fn name(&self) -> EventName {
        match self {
            Self::WorkflowStarted { .. } => EventName::WorkflowStarted,
            Self::WorkflowCompleted { .. } => EventName::WorkflowCompleted,
            Self::PluginInstalled { .. } => EventName::PluginInstalled,
            Self::DaemonStarted => EventName::DaemonStarted,
            Self::ErrorHit { .. } => EventName::ErrorHit,
            Self::CliInvoked { .. } => EventName::CliInvoked,
            Self::UpdateApplied => EventName::UpdateApplied,
        }
    }

    /// Returns the tag key/value pair for this event, or `None` for tagless events.
    pub fn tag(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::WorkflowStarted { kind } => Some(("workflow_kind", kind.as_str())),
            Self::WorkflowCompleted { outcome } => Some(("outcome", outcome.as_str())),
            Self::PluginInstalled { kind } => Some(("plugin_kind", kind.as_str())),
            Self::ErrorHit { class } => Some(("error_class", class.as_str())),
            Self::CliInvoked { group } => Some(("command_group", group.as_str())),
            Self::DaemonStarted | Self::UpdateApplied => None,
        }
    }
}
