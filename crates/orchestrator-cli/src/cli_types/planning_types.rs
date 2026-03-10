use clap::Subcommand;

use super::{
    IdArgs, RequirementsDraftArgs, RequirementsExecuteArgs, RequirementsRefineArgs, VisionCommand,
};

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningCommand {
    /// Planning facade for vision commands.
    Vision {
        #[command(subcommand)]
        command: VisionCommand,
    },
    /// Planning facade for requirements commands.
    Requirements {
        #[command(subcommand)]
        command: PlanningRequirementsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningRequirementsCommand {
    /// Draft requirements from current project context.
    Draft(RequirementsDraftArgs),
    /// Refine existing requirements.
    Refine(RequirementsRefineArgs),
    /// Execute requirements into tasks and optional workflows.
    Execute(RequirementsExecuteArgs),
    /// List requirements.
    List,
    /// Get a requirement by id.
    Get(IdArgs),
}
