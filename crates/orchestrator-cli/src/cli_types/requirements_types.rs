use clap::{ArgAction, Args, Subcommand};

use super::{
    IdArgs, INPUT_JSON_PRECEDENCE_HELP, REQUIREMENT_CATEGORY_HELP, REQUIREMENT_PRIORITY_HELP,
    REQUIREMENT_STATUS_HELP, REQUIREMENT_TYPE_HELP,
};

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementsCommand {
    /// Draft requirements from project context.
    Draft(RequirementsDraftArgs),
    /// List requirements.
    List(RequirementsListArgs),
    /// Get a requirement by id.
    Get(IdArgs),
    /// Refine existing requirements.
    Refine(RequirementsRefineArgs),
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

#[derive(Debug, Args, Default)]
pub(crate) struct RequirementsListArgs {
    #[arg(long, value_name = "STATUS", help = REQUIREMENT_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = REQUIREMENT_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "TYPE", help = REQUIREMENT_TYPE_HELP)]
    pub(crate) requirement_type: Option<String>,
    #[arg(long, value_name = "CATEGORY", help = REQUIREMENT_CATEGORY_HELP)]
    pub(crate) category: Option<String>,
    #[arg(
        long,
        value_name = "TAG",
        help = "Filter requirements by tag. Repeat to require multiple tags."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Filter requirements by label. Repeat to require multiple labels."
    )]
    pub(crate) label: Vec<String>,
    #[arg(long, value_name = "AREA", help = "Filter requirements by area.")]
    pub(crate) area: Option<String>,
    #[arg(long, value_name = "SOURCE", help = "Filter requirements by source.")]
    pub(crate) source: Option<String>,
    #[arg(
        long = "epic-id",
        value_name = "EPIC_ID",
        help = "Filter requirements linked to an epic id."
    )]
    pub(crate) epic_id: Option<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Filter requirements linked to a task id."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Search requirement id, title, description, body, acceptance criteria, tags, labels, area, and source."
    )]
    pub(crate) search: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementsDraftArgs {
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) include_codebase_scan: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) append_only: bool,
    #[arg(long, default_value_t = 0)]
    pub(crate) max_requirements: usize,
    #[arg(long, default_value = "single-agent")]
    pub(crate) draft_strategy: String,
    #[arg(long, default_value_t = 1)]
    pub(crate) po_parallelism: usize,
    #[arg(long, default_value_t = 2)]
    pub(crate) quality_repair_attempts: usize,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_complexity: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value_t = protocol::default_model_for_tool("codex").expect("default model for codex should be configured").to_string())]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementsRefineArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) focus: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value_t = protocol::default_model_for_tool("codex").expect("default model for codex should be configured").to_string())]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Requirement title.")]
    pub(crate) title: String,
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        help = "Requirement description."
    )]
    pub(crate) description: String,
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
        long = "label",
        value_name = "LABEL",
        help = "Labels to assign to the requirement. Repeat to add multiple values."
    )]
    pub(crate) label: Vec<String>,
    #[arg(long, value_name = "AREA", help = "Area assigned to the requirement.")]
    pub(crate) area: Option<String>,
    #[arg(
        long = "external-ref",
        value_name = "REF",
        help = "External tracking reference for the requirement."
    )]
    pub(crate) external_ref: Option<String>,
    #[arg(
        long = "linked-epic-id",
        value_name = "EPIC_ID",
        help = "Epic ids linked to the requirement. Repeat to add multiple ids."
    )]
    pub(crate) linked_epic_id: Vec<String>,
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
        long = "label",
        value_name = "LABEL",
        help = "Labels to assign to the requirement. Repeat to add multiple values."
    )]
    pub(crate) label: Vec<String>,
    #[arg(
        long,
        value_name = "AREA",
        help = "Updated area value for the requirement."
    )]
    pub(crate) area: Option<String>,
    #[arg(
        long = "external-ref",
        value_name = "REF",
        help = "Updated external tracking reference for the requirement."
    )]
    pub(crate) external_ref: Option<String>,
    #[arg(
        long = "linked-epic-id",
        value_name = "EPIC_ID",
        help = "Epic ids linked to the requirement. Repeat to add multiple ids."
    )]
    pub(crate) linked_epic_id: Vec<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Task ids linked to this requirement. Repeat to add multiple ids."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all linked epic ids with the provided --linked-epic-id values."
    )]
    pub(crate) replace_linked_epic_ids: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all labels with the provided --label values."
    )]
    pub(crate) replace_labels: bool,
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
