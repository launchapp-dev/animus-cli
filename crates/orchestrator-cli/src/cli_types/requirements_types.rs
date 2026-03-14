use clap::{ArgAction, Args, Subcommand};

use super::{
    IdArgs, INPUT_JSON_PRECEDENCE_HELP, REQUIREMENT_CATEGORY_HELP, REQUIREMENT_PRIORITY_HELP,
    REQUIREMENT_STATUS_HELP, REQUIREMENT_TYPE_HELP,
};

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementsCommand {
    /// Execute requirements into implementation tasks and optional workflows.
    Execute(RequirementsExecuteArgs),
    /// List requirements.
    List,
    /// Get a requirement by id.
    Get(IdArgs),
    /// Create a requirement.
    Create(RequirementCreateArgs),
    /// Update a requirement.
    Update(RequirementUpdateArgs),
    /// Delete a requirement.
    Delete(IdArgs),
    /// View or replace the requirement dependency graph.
    Graph {
        #[command(subcommand)]
        command: RequirementGraphCommand,
    },
    /// Manage requirement mockups and linked assets.
    Mockups {
        #[command(subcommand)]
        command: MockupCommand,
    },
    /// Scan and apply requirement recommendations.
    Recommendations {
        #[command(subcommand)]
        command: RecommendationCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct RequirementsExecuteArgs {
    #[arg(
        long = "id",
        visible_alias = "requirement-id",
        visible_alias = "requirement-ids",
        value_name = "REQ_ID",
        num_args = 1..,
        help = "Requirement identifiers. Repeat or pass multiple values."
    )]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) workflow_ref: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_workflows: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Requirement title.")]
    pub(crate) title: String,
    #[arg(long, value_name = "TEXT", help = "Requirement description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = REQUIREMENT_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "CATEGORY", help = REQUIREMENT_CATEGORY_HELP)]
    pub(crate) category: Option<String>,
    #[arg(long = "type", value_name = "TYPE", help = REQUIREMENT_TYPE_HELP)]
    pub(crate) requirement_type: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Optional source describing where this requirement originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "acceptance-criterion",
        value_name = "TEXT",
        help = "Acceptance criteria text. Repeat to provide multiple criteria."
    )]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementUpdateArgs {
    #[arg(long, value_name = "REQ_ID", help = "Requirement identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TITLE", help = "Updated requirement title.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Updated requirement description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = REQUIREMENT_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "STATUS", help = REQUIREMENT_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "CATEGORY", help = REQUIREMENT_CATEGORY_HELP)]
    pub(crate) category: Option<String>,
    #[arg(long = "type", value_name = "TYPE", help = REQUIREMENT_TYPE_HELP)]
    pub(crate) requirement_type: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Updated source describing where this requirement originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Task ids linked to this requirement. Repeat to add multiple ids."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(
        long = "acceptance-criterion",
        value_name = "TEXT",
        help = "Acceptance criteria text. Repeat to provide multiple criteria."
    )]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all acceptance criteria with the provided --acceptance-criterion values."
    )]
    pub(crate) replace_acceptance_criteria: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementGraphCommand {
    /// Read the requirement graph.
    Get,
    /// Replace the requirement graph with provided JSON.
    Save(RequirementGraphSaveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RequirementGraphSaveArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Complete requirement graph JSON payload to persist."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MockupCommand {
    /// List requirement mockups.
    List,
    /// Create a mockup record.
    Create(MockupCreateArgs),
    /// Link a mockup to requirements or flows.
    Link(MockupLinkArgs),
    /// Get a mockup file by relative path.
    GetFile(MockupFileArgs),
}

#[derive(Debug, Args)]
pub(crate) struct MockupCreateArgs {
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long)]
    pub(crate) mockup_type: Option<String>,
    #[arg(long = "requirement-id")]
    pub(crate) requirement_id: Vec<String>,
    #[arg(long = "flow-id")]
    pub(crate) flow_id: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct MockupLinkArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long = "requirement-id")]
    pub(crate) requirement_id: Vec<String>,
    #[arg(long = "flow-id")]
    pub(crate) flow_id: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct MockupFileArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) relative_path: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecommendationCommand {
    /// Run recommendation scan over current project context.
    Scan(RecommendationScanArgs),
    /// List saved recommendation reports.
    List,
    /// Apply a recommendation report.
    Apply(RecommendationApplyArgs),
    /// Read recommendation config.
    ConfigGet,
    /// Update recommendation config.
    ConfigUpdate(RecommendationConfigUpdateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationScanArgs {
    #[arg(long)]
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationApplyArgs {
    #[arg(long)]
    pub(crate) report_id: String,
    #[arg(long)]
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationConfigUpdateArgs {
    #[arg(long)]
    pub(crate) mode: Option<String>,
    #[arg(long)]
    pub(crate) enabled: Option<bool>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}
