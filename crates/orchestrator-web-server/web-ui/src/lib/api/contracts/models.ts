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
};

export type TaskDetail = JsonRecord & {
  id: string;
  status?: TaskStatusValue;
  type?: TaskTypeValue;
};

export type WorkflowSummary = JsonRecord & {
  id: string;
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
