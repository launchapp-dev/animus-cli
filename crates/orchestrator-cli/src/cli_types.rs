use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "ao", about = "Agent Orchestrator CLI", version)]
pub(crate) struct Cli {
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[arg(long, global = true)]
    pub(crate) project_root: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Version,
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Vision {
        #[command(subcommand)]
        command: VisionCommand,
    },
    Requirements {
        #[command(subcommand)]
        command: RequirementsCommand,
    },
    Architecture {
        #[command(subcommand)]
        command: ArchitectureCommand,
    },
    Execute {
        #[command(subcommand)]
        command: ExecuteCommand,
    },
    Planning {
        #[command(subcommand)]
        command: PlanningCommand,
    },
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    Qa {
        #[command(subcommand)]
        command: QaCommand,
    },
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    Errors {
        #[command(subcommand)]
        command: ErrorsCommand,
    },
    TaskControl {
        #[command(subcommand)]
        command: TaskControlCommand,
    },
    Git {
        #[command(subcommand)]
        command: GitCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Runner {
        #[command(subcommand)]
        command: RunnerCommand,
    },
    Output {
        #[command(subcommand)]
        command: OutputCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    Doctor,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    Serve,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WebCommand {
    Serve(WebServeArgs),
    Open(WebOpenArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WebServeArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) host: String,
    #[arg(long, default_value_t = 4173)]
    pub(crate) port: u16,
    #[arg(long, default_value_t = false)]
    pub(crate) open: bool,
    #[arg(long)]
    pub(crate) assets_dir: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) api_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WebOpenArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) host: String,
    #[arg(long, default_value_t = 4173)]
    pub(crate) port: u16,
    #[arg(long, default_value = "/")]
    pub(crate) path: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    Start(DaemonStartArgs),
    Run(DaemonRunArgs),
    Stop,
    Status,
    Health,
    Pause,
    Resume,
    Events(DaemonEventsArgs),
    Logs(LogArgs),
    ClearLogs,
    Agents,
    Config(DaemonConfigArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    Run(AgentRunArgs),
    Control(AgentControlArgs),
    Status(AgentStatusArgs),
    ModelStatus(AgentModelStatusArgs),
    RunnerStatus(AgentRunnerStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentRunArgs {
    #[arg(long)]
    pub(crate) run_id: Option<String>,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) prompt: Option<String>,
    #[arg(long)]
    pub(crate) cwd: Option<String>,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long)]
    pub(crate) context_json: Option<String>,
    #[arg(long)]
    pub(crate) runtime_contract_json: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) detach: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) stream: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) save_jsonl: bool,
    #[arg(long)]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentControlArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long, value_enum)]
    pub(crate) action: AgentControlActionArg,
    #[arg(long, default_value_t = false)]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum AgentControlActionArg {
    Pause,
    Resume,
    Terminate,
}

#[derive(Debug, Args)]
pub(crate) struct AgentStatusArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentModelStatusArgs {
    #[arg(long = "model")]
    pub(crate) models: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentRunnerStatusArgs {
    #[arg(long, default_value_t = false)]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStartArgs {
    #[arg(long)]
    pub(crate) max_agents: Option<usize>,
    #[arg(long, default_value_t = false)]
    pub(crate) skip_runner: bool,
    #[arg(long, value_enum)]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) autonomous: bool,
    #[arg(long, default_value_t = 5)]
    pub(crate) interval_secs: u64,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) include_registry: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) auto_run_ready: bool,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) startup_cleanup: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) resume_interrupted: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) reconcile_stale: bool,
    #[arg(long, default_value_t = 2)]
    pub(crate) max_tasks_per_tick: usize,
    #[arg(long)]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(long)]
    pub(crate) idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonRunArgs {
    #[arg(long, default_value_t = 5)]
    pub(crate) interval_secs: u64,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) include_registry: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) auto_run_ready: bool,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) startup_cleanup: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) resume_interrupted: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) reconcile_stale: bool,
    #[arg(long, default_value_t = 2)]
    pub(crate) max_tasks_per_tick: usize,
    #[arg(long)]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(long)]
    pub(crate) idle_timeout_secs: Option<u64>,
    #[arg(long, default_value_t = false)]
    pub(crate) once: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonConfigArgs {
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_commit_before_merge: Option<bool>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonEventsArgs {
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) follow: bool,
}

#[derive(Debug, Args)]
pub(crate) struct LogArgs {
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum RunnerScopeArg {
    Project,
    Global,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProjectCommand {
    List,
    Active,
    Get(IdArgs),
    Create(ProjectCreateArgs),
    Load(IdArgs),
    Rename(ProjectRenameArgs),
    Archive(IdArgs),
    Remove(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProjectCreateArgs {
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) path: String,
    #[arg(long)]
    pub(crate) project_type: Option<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct IdArgs {
    #[arg(long)]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ProjectRenameArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    List(TaskListArgs),
    Prioritized,
    Next,
    Stats,
    Get(IdArgs),
    Create(TaskCreateArgs),
    Update(TaskUpdateArgs),
    Delete(TaskDeleteArgs),
    Assign(TaskAssignArgs),
    AssignAgent(TaskAssignAgentArgs),
    AssignHuman(TaskAssignHumanArgs),
    ChecklistAdd(TaskChecklistAddArgs),
    ChecklistUpdate(TaskChecklistUpdateArgs),
    DependencyAdd(TaskDependencyAddArgs),
    DependencyRemove(TaskDependencyRemoveArgs),
    Status(TaskStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TaskListArgs {
    #[arg(long)]
    pub(crate) task_type: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    #[arg(long)]
    pub(crate) priority: Option<String>,
    #[arg(long)]
    pub(crate) assignee_type: Option<String>,
    #[arg(long)]
    pub(crate) tag: Vec<String>,
    #[arg(long)]
    pub(crate) linked_requirement: Option<String>,
    #[arg(long)]
    pub(crate) linked_architecture_entity: Option<String>,
    #[arg(long)]
    pub(crate) search: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskCreateArgs {
    #[arg(long)]
    pub(crate) title: String,
    #[arg(long, default_value = "")]
    pub(crate) description: String,
    #[arg(long)]
    pub(crate) task_type: Option<String>,
    #[arg(long)]
    pub(crate) priority: Option<String>,
    #[arg(long = "linked-architecture-entity")]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) title: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long)]
    pub(crate) priority: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    #[arg(long)]
    pub(crate) assignee: Option<String>,
    #[arg(long = "linked-architecture-entity")]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_linked_architecture_entities: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDeleteArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) assignee: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignAgentArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) role: String,
    #[arg(long)]
    pub(crate) model: Option<String>,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignHumanArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) user_id: String,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistAddArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) description: String,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) item_id: String,
    #[arg(long)]
    pub(crate) completed: bool,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyAddArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) dependency_id: String,
    #[arg(long)]
    pub(crate) dependency_type: String,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyRemoveArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) dependency_id: String,
    #[arg(long, default_value = "ao-cli")]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskStatusArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) status: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCommand {
    List,
    Get(IdArgs),
    Decisions(IdArgs),
    Checkpoints {
        #[command(subcommand)]
        command: WorkflowCheckpointCommand,
    },
    Run(WorkflowRunArgs),
    Resume(IdArgs),
    ResumeStatus(IdArgs),
    Pause(WorkflowPauseArgs),
    Cancel(WorkflowCancelArgs),
    Phase {
        #[command(subcommand)]
        command: WorkflowPhaseCommand,
    },
    Phases {
        #[command(subcommand)]
        command: WorkflowPhasesCommand,
    },
    Pipelines {
        #[command(subcommand)]
        command: WorkflowPipelinesCommand,
    },
    Config {
        #[command(subcommand)]
        command: WorkflowConfigCommand,
    },
    StateMachine {
        #[command(subcommand)]
        command: WorkflowStateMachineCommand,
    },
    AgentRuntime {
        #[command(subcommand)]
        command: WorkflowAgentRuntimeCommand,
    },
    UpdatePipeline(WorkflowPipelineUpdateArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhaseCommand {
    Approve(WorkflowPhaseApproveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhasesCommand {
    List,
    Get(WorkflowPhaseGetArgs),
    Upsert(WorkflowPhaseUpsertArgs),
    Remove(WorkflowPhaseRemoveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPipelinesCommand {
    List,
    Upsert(WorkflowPipelineUpsertArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowConfigCommand {
    Get,
    Validate,
    MigrateV2,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowStateMachineCommand {
    Get,
    Validate,
    Set(WorkflowStateMachineSetArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowAgentRuntimeCommand {
    Get,
    Validate,
    Set(WorkflowAgentRuntimeSetArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum VisionCommand {
    Draft(VisionDraftArgs),
    Refine(VisionRefineArgs),
    Get,
}

#[derive(Debug, Args)]
pub(crate) struct VisionDraftArgs {
    #[arg(long)]
    pub(crate) project_name: Option<String>,
    #[arg(long, default_value = "")]
    pub(crate) problem: String,
    #[arg(long)]
    pub(crate) target_user: Vec<String>,
    #[arg(long)]
    pub(crate) goal: Vec<String>,
    #[arg(long)]
    pub(crate) constraint: Vec<String>,
    #[arg(long)]
    pub(crate) value_proposition: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai_complexity: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_fallback: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct VisionRefineArgs {
    #[arg(long)]
    pub(crate) focus: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_fallback: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) preserve_core: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementsCommand {
    Draft(RequirementsDraftArgs),
    List,
    Get(IdArgs),
    Refine(RequirementsRefineArgs),
    Create(RequirementCreateArgs),
    Update(RequirementUpdateArgs),
    Delete(IdArgs),
    Graph {
        #[command(subcommand)]
        command: RequirementGraphCommand,
    },
    Mockups {
        #[command(subcommand)]
        command: MockupCommand,
    },
    Recommendations {
        #[command(subcommand)]
        command: RecommendationCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureCommand {
    Get,
    Set(ArchitectureSetArgs),
    Suggest(ArchitectureSuggestArgs),
    Entity {
        #[command(subcommand)]
        command: ArchitectureEntityCommand,
    },
    Edge {
        #[command(subcommand)]
        command: ArchitectureEdgeCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureSetArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureSuggestArgs {
    #[arg(long)]
    pub(crate) task_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureEntityCommand {
    List,
    Get(IdArgs),
    Create(ArchitectureEntityCreateArgs),
    Update(ArchitectureEntityUpdateArgs),
    Delete(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEntityCreateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) kind: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long = "code-path")]
    pub(crate) code_path: Vec<String>,
    #[arg(long = "tag")]
    pub(crate) tag: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEntityUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: Option<String>,
    #[arg(long)]
    pub(crate) kind: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_description: bool,
    #[arg(long = "code-path")]
    pub(crate) code_path: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_code_paths: bool,
    #[arg(long = "tag")]
    pub(crate) tag: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_tags: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureEdgeCommand {
    List,
    Create(ArchitectureEdgeCreateArgs),
    Delete(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEdgeCreateArgs {
    #[arg(long)]
    pub(crate) id: Option<String>,
    #[arg(long)]
    pub(crate) from: String,
    #[arg(long)]
    pub(crate) to: String,
    #[arg(long)]
    pub(crate) relation: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
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
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long)]
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
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementCreateArgs {
    #[arg(long)]
    pub(crate) title: String,
    #[arg(long, default_value = "")]
    pub(crate) description: String,
    #[arg(long)]
    pub(crate) priority: Option<String>,
    #[arg(long)]
    pub(crate) source: Option<String>,
    #[arg(long = "acceptance-criterion")]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) title: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long)]
    pub(crate) priority: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    #[arg(long)]
    pub(crate) source: Option<String>,
    #[arg(long = "linked-task-id")]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(long = "acceptance-criterion")]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_acceptance_criteria: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementGraphCommand {
    Get,
    Save(RequirementGraphSaveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RequirementGraphSaveArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MockupCommand {
    List,
    Create(MockupCreateArgs),
    Link(MockupLinkArgs),
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
    Scan(RecommendationScanArgs),
    List,
    Apply(RecommendationApplyArgs),
    ConfigGet,
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
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ExecuteCommand {
    Plan(ExecutePlanArgs),
    Run(ExecuteRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ExecutePlanArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ExecuteRunArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningCommand {
    Vision {
        #[command(subcommand)]
        command: PlanningVisionCommand,
    },
    Requirements {
        #[command(subcommand)]
        command: PlanningRequirementsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningVisionCommand {
    Draft(VisionDraftArgs),
    Refine(VisionRefineArgs),
    Get,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningRequirementsCommand {
    Draft(RequirementsDraftArgs),
    List,
    Get(IdArgs),
    Refine(RequirementsRefineArgs),
    Execute(PlanningExecuteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PlanningExecuteArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) start_workflows: bool,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    Entity(ReviewEntityArgs),
    Record(ReviewRecordArgs),
    TaskStatus(TaskIdArgs),
    RequirementStatus(IdArgs),
    Handoff(ReviewHandoffArgs),
    DualApprove(ReviewDualApproveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ReviewEntityArgs {
    #[arg(long)]
    pub(crate) entity_type: String,
    #[arg(long)]
    pub(crate) entity_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewRecordArgs {
    #[arg(long)]
    pub(crate) entity_type: String,
    #[arg(long)]
    pub(crate) entity_id: String,
    #[arg(long)]
    pub(crate) reviewer_role: String,
    #[arg(long)]
    pub(crate) decision: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
    #[arg(long)]
    pub(crate) source: Option<String>,
    #[arg(long)]
    pub(crate) content_hash: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewHandoffArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) target_role: String,
    #[arg(long)]
    pub(crate) question: String,
    #[arg(long)]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewDualApproveArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QaCommand {
    Evaluate(QaEvaluateArgs),
    Get(QaPhaseArgs),
    List(QaWorkflowArgs),
    Approval {
        #[command(subcommand)]
        command: QaApprovalCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct QaEvaluateArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) worktree_path: Option<String>,
    #[arg(long)]
    pub(crate) gates_json: Option<String>,
    #[arg(long)]
    pub(crate) metrics_json: Option<String>,
    #[arg(long)]
    pub(crate) metadata_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct QaPhaseArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct QaWorkflowArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QaApprovalCommand {
    Add(QaApprovalAddArgs),
    List(QaApprovalListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct QaApprovalAddArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
    #[arg(long)]
    pub(crate) gate_id: String,
    #[arg(long)]
    pub(crate) approved_by: String,
    #[arg(long)]
    pub(crate) comment: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct QaApprovalListArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) gate_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HistoryCommand {
    Task(HistoryTaskArgs),
    Get(IdArgs),
    Recent(HistoryRecentArgs),
    Search(HistorySearchArgs),
    Cleanup(HistoryCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HistoryTaskArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryRecentArgs {
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistorySearchArgs {
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) workflow_id: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    #[arg(long)]
    pub(crate) started_after: Option<String>,
    #[arg(long)]
    pub(crate) started_before: Option<String>,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    #[arg(long)]
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryCleanupArgs {
    #[arg(long, default_value_t = 30)]
    pub(crate) days: i64,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ErrorsCommand {
    List(ErrorsListArgs),
    Get(IdArgs),
    Stats,
    Retry(IdArgs),
    Cleanup(ErrorsCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ErrorsListArgs {
    #[arg(long)]
    pub(crate) category: Option<String>,
    #[arg(long)]
    pub(crate) severity: Option<String>,
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct ErrorsCleanupArgs {
    #[arg(long, default_value_t = 30)]
    pub(crate) days: u32,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskControlCommand {
    Pause(TaskIdArgs),
    Resume(TaskIdArgs),
    Cancel(TaskControlCancelArgs),
    SetPriority(TaskControlPriorityArgs),
    SetDeadline(TaskControlDeadlineArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TaskIdArgs {
    #[arg(long)]
    pub(crate) task_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlCancelArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlPriorityArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) priority: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlDeadlineArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) deadline: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitCommand {
    Repo {
        #[command(subcommand)]
        command: GitRepoCommand,
    },
    Branches(GitRepoArgs),
    Status(GitRepoArgs),
    Commit(GitCommitArgs),
    Push(GitPushArgs),
    Pull(GitPullArgs),
    Worktree {
        #[command(subcommand)]
        command: GitWorktreeCommand,
    },
    Confirm {
        #[command(subcommand)]
        command: GitConfirmCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitRepoCommand {
    List,
    Get(GitRepoArgs),
    Init(GitRepoInitArgs),
    Clone(GitRepoCloneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoArgs {
    #[arg(long)]
    pub(crate) repo: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoInitArgs {
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoCloneArgs {
    #[arg(long)]
    pub(crate) url: String,
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitCommitArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) message: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitPushArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
    #[arg(long, default_value = "main")]
    pub(crate) branch: String,
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitPullArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
    #[arg(long, default_value = "main")]
    pub(crate) branch: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitWorktreeCommand {
    Create(GitWorktreeCreateArgs),
    List(GitRepoArgs),
    Get(GitWorktreeGetArgs),
    Remove(GitWorktreeRemoveArgs),
    Pull(GitWorktreePullArgs),
    Push(GitWorktreePushArgs),
    Sync(GitWorktreeSyncArgs),
    SyncStatus(GitWorktreeGetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeCreateArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
    #[arg(long)]
    pub(crate) worktree_path: String,
    #[arg(long)]
    pub(crate) branch: String,
    #[arg(long, default_value_t = false)]
    pub(crate) create_branch: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeGetArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeRemoveArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePullArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePushArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeSyncArgs {
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) worktree_name: String,
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitConfirmCommand {
    Request(GitConfirmRequestArgs),
    Respond(GitConfirmRespondArgs),
    Outcome(GitConfirmOutcomeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRequestArgs {
    #[arg(long)]
    pub(crate) operation_type: String,
    #[arg(long)]
    pub(crate) repo_name: String,
    #[arg(long)]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRespondArgs {
    #[arg(long)]
    pub(crate) request_id: String,
    #[arg(long)]
    pub(crate) approved: bool,
    #[arg(long)]
    pub(crate) comment: Option<String>,
    #[arg(long)]
    pub(crate) user_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmOutcomeArgs {
    #[arg(long)]
    pub(crate) request_id: String,
    #[arg(long)]
    pub(crate) success: bool,
    #[arg(long)]
    pub(crate) message: String,
    #[arg(long)]
    pub(crate) metadata_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelCommand {
    Availability(ModelAvailabilityArgs),
    Status(ModelStatusArgs),
    Validate(ModelValidateArgs),
    Roster {
        #[command(subcommand)]
        command: ModelRosterCommand,
    },
    Eval {
        #[command(subcommand)]
        command: ModelEvalCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ModelAvailabilityArgs {
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelStatusArgs {
    #[arg(long)]
    pub(crate) model_id: String,
    #[arg(long)]
    pub(crate) cli_tool: String,
}

#[derive(Debug, Args)]
pub(crate) struct ModelValidateArgs {
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelRosterCommand {
    Refresh,
    Get,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelEvalCommand {
    Run(ModelEvalRunArgs),
    Report,
}

#[derive(Debug, Args)]
pub(crate) struct ModelEvalRunArgs {
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommand {
    Health,
    Orphans {
        #[command(subcommand)]
        command: RunnerOrphanCommand,
    },
    RestartStats,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerOrphanCommand {
    Detect,
    Cleanup(RunnerOrphanCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunnerOrphanCleanupArgs {
    #[arg(long = "run-id")]
    pub(crate) run_id: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OutputCommand {
    Run(OutputRunArgs),
    Artifacts(OutputArtifactsArgs),
    Download(OutputDownloadArgs),
    Files(OutputFilesArgs),
    Jsonl(OutputJsonlArgs),
    Monitor(OutputMonitorArgs),
    Cli(OutputCliArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OutputRunArgs {
    #[arg(long)]
    pub(crate) run_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputArtifactsArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputDownloadArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
    #[arg(long)]
    pub(crate) artifact_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputFilesArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputJsonlArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long, default_value_t = false)]
    pub(crate) entries: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OutputMonitorArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) phase_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputCliArgs {
    #[arg(long)]
    pub(crate) run_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCheckpointCommand {
    List(IdArgs),
    Get(WorkflowCheckpointGetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointGetArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) checkpoint: usize,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowRunArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPauseArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCancelArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseApproveArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) phase: String,
    #[arg(long)]
    pub(crate) note: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseGetArgs {
    #[arg(long)]
    pub(crate) phase: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseUpsertArgs {
    #[arg(long)]
    pub(crate) phase: String,
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseRemoveArgs {
    #[arg(long)]
    pub(crate) phase: String,
    #[arg(long)]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpsertArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long = "phase")]
    pub(crate) phases: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowStateMachineSetArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowAgentRuntimeSetArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}
