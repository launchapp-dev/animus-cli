//! Shared types for the modular `animus doctor` checks.
//!
//! Each category lives in its own module under `ops_doctor/` and emits one or
//! more [`DiagnosticCheck`] results. The runner aggregates them into a
//! [`DiagnosticReport`] that gets serialized under the `animus.doctor.v1`
//! schema, alongside the legacy `orchestrator_core::DoctorReport` for
//! back-compat.

use std::path::PathBuf;

use serde::Serialize;

/// Status for a single check. Matches the legacy `DoctorCheckStatus` shape so
/// the JSON envelope stays consistent across the old and new surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

impl CheckStatus {
    pub(crate) fn glyph(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "[ok]",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[fail]",
            CheckStatus::Skipped => "[skip]",
        }
    }
}

/// One concrete suggestion the operator can paste back into a shell.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckFix {
    /// Stable id (e.g. `install_provider_plugin`). Tests assert on this.
    pub(crate) id: String,
    /// Human-readable description.
    pub(crate) details: String,
    /// Shell command, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<String>,
    /// Whether `animus doctor --fix` can apply this fix without prompting.
    /// `false` for anything that touches secrets, installs system packages,
    /// or otherwise requires operator consent.
    pub(crate) auto_applicable: bool,
}

impl CheckFix {
    pub(crate) fn command(id: &str, details: &str, command: &str) -> Self {
        Self {
            id: id.to_string(),
            details: details.to_string(),
            command: Some(command.to_string()),
            auto_applicable: false,
        }
    }

    pub(crate) fn manual(id: &str, details: &str) -> Self {
        Self { id: id.to_string(), details: details.to_string(), command: None, auto_applicable: false }
    }

    pub(crate) fn auto(id: &str, details: &str, command: &str) -> Self {
        Self {
            id: id.to_string(),
            details: details.to_string(),
            command: Some(command.to_string()),
            auto_applicable: true,
        }
    }

    pub(crate) fn auto_no_command(id: &str, details: &str) -> Self {
        Self { id: id.to_string(), details: details.to_string(), command: None, auto_applicable: true }
    }
}

/// One diagnostic check emitted by a category module.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticCheck {
    /// Stable id (e.g. `plugin_binary_executable.animus-provider-claude`).
    pub(crate) id: String,
    /// Coarse category label (e.g. `plugins`, `daemon`, `cli_tools`).
    pub(crate) category: &'static str,
    pub(crate) status: CheckStatus,
    pub(crate) title: String,
    pub(crate) details: String,
    /// Optional "current" value the user is seeing (e.g. `not running`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current: Option<String>,
    /// Optional "expected" value (e.g. `running with control.sock present`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) fixes: Vec<CheckFix>,
}

impl DiagnosticCheck {
    pub(crate) fn new(
        id: impl Into<String>,
        category: &'static str,
        status: CheckStatus,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            status,
            title: title.into(),
            details: String::new(),
            current: None,
            expected: None,
            fixes: Vec::new(),
        }
    }

    pub(crate) fn details(mut self, details: impl Into<String>) -> Self {
        self.details = details.into();
        self
    }

    pub(crate) fn current(mut self, value: impl Into<String>) -> Self {
        self.current = Some(value.into());
        self
    }

    pub(crate) fn expected(mut self, value: impl Into<String>) -> Self {
        self.expected = Some(value.into());
        self
    }

    pub(crate) fn fix(mut self, fix: CheckFix) -> Self {
        self.fixes.push(fix);
        self
    }
}

/// Input handed to every check.
#[derive(Debug, Clone)]
pub(crate) struct CheckContext {
    pub(crate) project_root: PathBuf,
    pub(crate) skip_subprocess: bool,
}

/// Outcome of running [`apply_safe_fixes`] for one fix id.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FixOutcome {
    pub(crate) id: String,
    pub(crate) status: &'static str,
    pub(crate) details: String,
}
