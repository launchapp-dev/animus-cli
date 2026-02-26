export type JsonRecord = Record<string, unknown>;

export type DaemonStatusValue =
  | "starting"
  | "running"
  | "paused"
  | "stopping"
  | "stopped"
  | "crashed"
  | "unknown";

export type TaskStatusValue =
  | "backlog"
  | "ready"
  | "in-progress"
  | "blocked"
  | "on-hold"
  | "done"
  | "cancelled"
  | "unknown";

export type TaskTypeValue =
  | "feature"
  | "bugfix"
  | "hotfix"
  | "refactor"
  | "docs"
  | "test"
  | "chore"
  | "experiment"
  | "unknown";

export type PriorityValue = "critical" | "high" | "medium" | "low" | "unknown";

export type WorkflowStatusValue =
  | "pending"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled"
  | "unknown";

export type MessagePayload = JsonRecord & {
  message: string;
};

export type SystemInfo = JsonRecord & {
  platform: string;
  arch: string;
  version: string;
  daemon_status: DaemonStatusValue;
  daemon_running?: boolean;
  project_root?: string;
};

export type DaemonHealth = JsonRecord & {
  healthy: boolean;
  status: DaemonStatusValue;
};

export type DaemonLogEntry = unknown;

export type ProjectSummary = JsonRecord & {
  id: string;
  name: string;
  archived?: boolean;
};

export type ProjectDetail = JsonRecord & {
  id: string;
  name: string;
};

export type TaskSummary = JsonRecord & {
  id: string;
  status: TaskStatusValue;
  type: TaskTypeValue;
  title?: string;
  description?: string;
  priority?: PriorityValue;
  updated_at?: string;
  metadata?: JsonRecord & {
    updated_at?: string;
  };
  checklist?: JsonRecord[];
  dependencies?: JsonRecord[];
};

export type TaskDetail = JsonRecord & {
  id: string;
  status?: TaskStatusValue;
  type?: TaskTypeValue;
  title?: string;
  description?: string;
  priority?: PriorityValue;
  updated_at?: string;
  metadata?: JsonRecord & {
    updated_at?: string;
  };
  checklist?: JsonRecord[];
  dependencies?: JsonRecord[];
};

export type WorkflowSummary = JsonRecord & {
  id: string;
  task_id?: string;
  pipeline_id?: string | null;
  status?: WorkflowStatusValue;
  current_phase?: string | null;
  current_phase_index?: number;
  started_at?: string;
  completed_at?: string | null;
};

export type ProjectTasksPayload = JsonRecord & {
  project: ProjectDetail;
  tasks: TaskSummary[];
};

export type ProjectWorkflowsPayload = JsonRecord & {
  project: ProjectDetail;
  workflows: WorkflowSummary[];
};

export type RequirementSummary = JsonRecord & {
  id: string;
};

export type ProjectRequirementSummary = JsonRecord & {
  project_id: string;
  project_name: string;
};

export type ProjectRequirementsByIdPayload = JsonRecord & {
  project_id: string;
  project_name: string;
  requirements: RequirementSummary[];
};

export type ProjectRequirementDetailPayload = JsonRecord & {
  project_id: string;
  project_name: string;
  requirement: RequirementSummary;
};

export type TaskStatsPayload = JsonRecord & {
  total?: number;
  in_progress?: number;
  blocked?: number;
  completed?: number;
  by_status?: Record<string, number>;
  by_priority?: Record<string, number>;
  by_type?: Record<string, number>;
};

export type WorkflowDecision = JsonRecord;
export type WorkflowCheckpoint = JsonRecord;
export type WorkflowCheckpointDetail = JsonRecord;

export type ReviewHandoffResponse = JsonRecord & {
  status: string;
};

export type RequirementPriorityValue = "must" | "should" | "could" | "wont" | "unknown";

export type RequirementStatusValue =
  | "draft"
  | "refined"
  | "planned"
  | "in-progress"
  | "done"
  | "po-review"
  | "em-review"
  | "needs-rework"
  | "approved"
  | "implemented"
  | "deprecated"
  | "unknown";

export type RequirementTypeValue =
  | "product"
  | "functional"
  | "non-functional"
  | "technical"
  | "other"
  | "unknown";

export type PlanningVisionDraftInput = {
  project_name?: string;
  problem_statement: string;
  target_users: string[];
  goals: string[];
  constraints: string[];
  value_proposition?: string;
};

export type PlanningVisionRefineInput = {
  focus?: string;
};

export type PlanningVisionDocument = JsonRecord & {
  id: string;
  project_root: string;
  markdown: string;
  problem_statement: string;
  target_users: string[];
  goals: string[];
  constraints: string[];
  value_proposition?: string;
  created_at: string;
  updated_at: string;
};

export type PlanningVisionRefineResult = JsonRecord & {
  updated_vision: PlanningVisionDocument;
  refinement: JsonRecord & {
    mode: string;
    focus?: string;
    rationale?: string;
    changes?: JsonRecord;
  };
};

export type PlanningRequirementItem = JsonRecord & {
  id: string;
  title: string;
  description: string;
  body?: string;
  category?: string;
  requirement_type?: RequirementTypeValue;
  acceptance_criteria: string[];
  priority: RequirementPriorityValue;
  status: RequirementStatusValue;
  source: string;
  tags: string[];
  linked_task_ids: string[];
  relative_path?: string;
  created_at: string;
  updated_at: string;
};

export type PlanningRequirementCreateInput = {
  id?: string;
  title: string;
  description?: string;
  body?: string;
  category?: string;
  requirement_type?: RequirementTypeValue | "non_functional";
  acceptance_criteria?: string[];
  priority?: "must" | "should" | "could" | "wont" | "won't";
  status?:
    | "draft"
    | "refined"
    | "planned"
    | "in-progress"
    | "in_progress"
    | "done"
    | "po-review"
    | "em-review"
    | "needs-rework"
    | "approved"
    | "implemented"
    | "deprecated";
  source?: string;
  tags?: string[];
  linked_task_ids?: string[];
  relative_path?: string;
};

export type PlanningRequirementUpdateInput = Partial<PlanningRequirementCreateInput>;

export type TaskUpdateInput = {
  title?: string;
  description?: string;
  priority?: "critical" | "high" | "medium" | "low";
  status?:
    | "todo"
    | "backlog"
    | "ready"
    | "in-progress"
    | "in_progress"
    | "blocked"
    | "on-hold"
    | "on_hold"
    | "done"
    | "cancelled";
  assignee?: string | null;
  tags?: string[];
  updated_by?: string;
  deadline?: string | null;
  linked_architecture_entities?: string[];
};

export type TaskStatusInput = {
  status:
    | "todo"
    | "backlog"
    | "ready"
    | "in-progress"
    | "in_progress"
    | "blocked"
    | "on-hold"
    | "on_hold"
    | "done"
    | "cancelled";
};

export type TaskChecklistAddInput = {
  description: string;
  updated_by?: string;
};

export type TaskChecklistUpdateInput = {
  completed: boolean;
  updated_by?: string;
};

export type TaskDependencyAddInput = {
  dependency_id: string;
  dependency_type: "blocks-by" | "blocked-by" | "related-to";
  updated_by?: string;
};

export type TaskDependencyRemoveInput = {
  updated_by?: string;
};

export type WorkflowRunInput = {
  task_id: string;
  pipeline_id?: string;
};

export type PlanningRequirementsDraftInput = {
  include_codebase_scan?: boolean;
  append_only?: boolean;
  max_requirements?: number;
};

export type PlanningRequirementsDraftResult = JsonRecord & {
  requirements: PlanningRequirementItem[];
  appended_count: number;
};

export type PlanningRequirementsRefineInput = {
  requirement_ids?: string[];
  focus?: string;
};

export type PlanningRequirementsRefineResult = JsonRecord & {
  requirements: PlanningRequirementItem[];
  updated_ids: string[];
  requested_ids: string[];
  scope: "selected" | "all";
  focus?: string;
};
