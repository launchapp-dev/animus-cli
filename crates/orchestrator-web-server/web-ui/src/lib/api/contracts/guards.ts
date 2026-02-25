import type {
  DaemonHealth,
  DaemonLogEntry,
  DaemonStatusValue,
  JsonRecord,
  MessagePayload,
  ProjectDetail,
  ProjectRequirementDetailPayload,
  ProjectRequirementSummary,
  ProjectRequirementsByIdPayload,
  ProjectSummary,
  ProjectTasksPayload,
  ProjectWorkflowsPayload,
  RequirementSummary,
  ReviewHandoffResponse,
  SystemInfo,
  TaskDetail,
  TaskStatsPayload,
  TaskSummary,
  WorkflowCheckpoint,
  WorkflowCheckpointDetail,
  WorkflowDecision,
  WorkflowSummary,
} from "./models";
import {
  normalizeDaemonStatus,
  normalizeTaskStatus,
  normalizeTaskType,
} from "./normalize";

export type DecodeResult<TData> =
  | {
      ok: true;
      data: TData;
    }
  | {
      ok: false;
      message: string;
    };

export type PayloadDecoder<TData> = (payload: unknown) => DecodeResult<TData>;

function decodeOk<TData>(data: TData): DecodeResult<TData> {
  return { ok: true, data };
}

function decodeError(message: string): DecodeResult<never> {
  return { ok: false, message };
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function decodeRecord(value: unknown, context: string): DecodeResult<JsonRecord> {
  if (!isRecord(value)) {
    return decodeError(`${context} must be an object`);
  }

  return decodeOk(value);
}

function decodeStringField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<string> {
  const value = record[key];
  if (typeof value !== "string") {
    return decodeError(`${context}.${key} must be a string`);
  }

  return decodeOk(value);
}

function decodeOptionalStringField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<string | undefined> {
  const value = record[key];
  if (value === undefined) {
    return decodeOk(undefined);
  }

  if (typeof value !== "string") {
    return decodeError(`${context}.${key} must be a string when present`);
  }

  return decodeOk(value);
}

function decodeOptionalBooleanField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<boolean | undefined> {
  const value = record[key];
  if (value === undefined) {
    return decodeOk(undefined);
  }

  if (typeof value !== "boolean") {
    return decodeError(`${context}.${key} must be a boolean when present`);
  }

  return decodeOk(value);
}

function decodeOptionalNumberField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<number | undefined> {
  const value = record[key];
  if (value === undefined) {
    return decodeOk(undefined);
  }

  if (typeof value !== "number") {
    return decodeError(`${context}.${key} must be a number when present`);
  }

  return decodeOk(value);
}

function decodeArray(
  value: unknown,
  context: string,
): DecodeResult<unknown[]> {
  if (!Array.isArray(value)) {
    return decodeError(`${context} must be an array`);
  }

  return decodeOk(value);
}

function decodeNumericRecord(
  value: unknown,
  context: string,
): DecodeResult<Record<string, number>> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const entries: Record<string, number> = {};
  for (const [key, entryValue] of Object.entries(record.data)) {
    if (typeof entryValue !== "number") {
      return decodeError(`${context}.${key} must be a number`);
    }
    entries[key] = entryValue;
  }

  return decodeOk(entries);
}

function decodeProjectSummaryItem(
  value: unknown,
  context: string,
): DecodeResult<ProjectSummary> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  const name = decodeStringField(record.data, "name", context);
  if (!name.ok) {
    return name;
  }

  return decodeOk({
    ...record.data,
    id: id.data,
    name: name.data,
  });
}

function decodeProjectIdentity(
  value: unknown,
  context: string,
): DecodeResult<ProjectDetail> {
  const project = decodeProjectSummaryItem(value, context);
  if (!project.ok) {
    return project;
  }

  return decodeOk({
    ...project.data,
    id: project.data.id,
    name: project.data.name,
  });
}

function decodeTaskItem(
  value: unknown,
  context: string,
  options: { requireStatus: boolean; requireType: boolean },
): DecodeResult<JsonRecord> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  const decoded: JsonRecord = {
    ...record.data,
    id: id.data,
  };

  if (options.requireStatus || "status" in record.data) {
    const status = decodeStringField(record.data, "status", context);
    if (!status.ok) {
      return status;
    }
    decoded["status"] = normalizeTaskStatus(status.data);
  }

  if (options.requireType || "type" in record.data) {
    const taskType = decodeStringField(record.data, "type", context);
    if (!taskType.ok) {
      return taskType;
    }
    decoded["type"] = normalizeTaskType(taskType.data);
  }

  return decodeOk(decoded);
}

function decodeWorkflowItem(
  value: unknown,
  context: string,
): DecodeResult<WorkflowSummary> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  return decodeOk({
    ...record.data,
    id: id.data,
  });
}

function decodeRequirementSummary(
  value: unknown,
  context: string,
): DecodeResult<RequirementSummary> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  return decodeOk({
    ...record.data,
    id: id.data,
  });
}

export function decodeSystemInfo(value: unknown): DecodeResult<SystemInfo> {
  const record = decodeRecord(value, "system_info");
  if (!record.ok) {
    return record;
  }

  const platform = decodeStringField(record.data, "platform", "system_info");
  if (!platform.ok) {
    return platform;
  }

  const arch = decodeStringField(record.data, "arch", "system_info");
  if (!arch.ok) {
    return arch;
  }

  const version = decodeStringField(record.data, "version", "system_info");
  if (!version.ok) {
    return version;
  }

  const daemonStatus = decodeStringField(record.data, "daemon_status", "system_info");
  if (!daemonStatus.ok) {
    return daemonStatus;
  }

  const daemonRunning = decodeOptionalBooleanField(record.data, "daemon_running", "system_info");
  if (!daemonRunning.ok) {
    return daemonRunning;
  }

  const projectRoot = decodeOptionalStringField(record.data, "project_root", "system_info");
  if (!projectRoot.ok) {
    return projectRoot;
  }

  return decodeOk({
    ...record.data,
    platform: platform.data,
    arch: arch.data,
    version: version.data,
    daemon_status: normalizeDaemonStatus(daemonStatus.data),
    daemon_running: daemonRunning.data,
    project_root: projectRoot.data,
  });
}

export function decodeDaemonStatus(value: unknown): DecodeResult<DaemonStatusValue> {
  if (typeof value !== "string") {
    return decodeError("daemon_status must be a string");
  }

  return decodeOk(normalizeDaemonStatus(value));
}

export function decodeDaemonHealth(value: unknown): DecodeResult<DaemonHealth> {
  const record = decodeRecord(value, "daemon_health");
  if (!record.ok) {
    return record;
  }

  const healthy = decodeOptionalBooleanField(record.data, "healthy", "daemon_health");
  if (!healthy.ok) {
    return healthy;
  }

  if (healthy.data === undefined) {
    return decodeError("daemon_health.healthy must be a boolean");
  }

  const status = decodeStringField(record.data, "status", "daemon_health");
  if (!status.ok) {
    return status;
  }

  return decodeOk({
    ...record.data,
    healthy: healthy.data,
    status: normalizeDaemonStatus(status.data),
  });
}

export function decodeDaemonLogs(value: unknown): DecodeResult<DaemonLogEntry[]> {
  const logs = decodeArray(value, "daemon_logs");
  if (!logs.ok) {
    return logs;
  }

  return decodeOk(logs.data);
}

export function decodeMessagePayload(value: unknown): DecodeResult<MessagePayload> {
  const record = decodeRecord(value, "message_response");
  if (!record.ok) {
    return record;
  }

  const message = decodeStringField(record.data, "message", "message_response");
  if (!message.ok) {
    return message;
  }

  return decodeOk({
    ...record.data,
    message: message.data,
  });
}

export function decodeProjectsList(value: unknown): DecodeResult<ProjectSummary[]> {
  const projects = decodeArray(value, "projects");
  if (!projects.ok) {
    return projects;
  }

  const decoded: ProjectSummary[] = [];
  for (let index = 0; index < projects.data.length; index += 1) {
    const project = decodeProjectSummaryItem(projects.data[index], `projects[${index}]`);
    if (!project.ok) {
      return project;
    }
    decoded.push(project.data);
  }

  return decodeOk(decoded);
}

export function decodeProjectsActive(value: unknown): DecodeResult<ProjectSummary | null> {
  if (value === null) {
    return decodeOk(null);
  }

  return decodeProjectSummaryItem(value, "projects_active");
}

export function decodeProjectDetail(value: unknown): DecodeResult<ProjectDetail> {
  return decodeProjectIdentity(value, "project");
}

export function decodeProjectTasksPayload(
  value: unknown,
): DecodeResult<ProjectTasksPayload> {
  const record = decodeRecord(value, "project_tasks");
  if (!record.ok) {
    return record;
  }

  const project = decodeProjectIdentity(record.data["project"], "project_tasks.project");
  if (!project.ok) {
    return project;
  }

  const tasks = decodeArray(record.data["tasks"], "project_tasks.tasks");
  if (!tasks.ok) {
    return tasks;
  }

  const decodedTasks: TaskSummary[] = [];
  for (let index = 0; index < tasks.data.length; index += 1) {
    const task = decodeTaskItem(tasks.data[index], `project_tasks.tasks[${index}]`, {
      requireStatus: true,
      requireType: true,
    });
    if (!task.ok) {
      return task;
    }
    decodedTasks.push(task.data as TaskSummary);
  }

  return decodeOk({
    ...record.data,
    project: project.data,
    tasks: decodedTasks,
  });
}

export function decodeProjectWorkflowsPayload(
  value: unknown,
): DecodeResult<ProjectWorkflowsPayload> {
  const record = decodeRecord(value, "project_workflows");
  if (!record.ok) {
    return record;
  }

  const project = decodeProjectIdentity(record.data["project"], "project_workflows.project");
  if (!project.ok) {
    return project;
  }

  const workflows = decodeArray(record.data["workflows"], "project_workflows.workflows");
  if (!workflows.ok) {
    return workflows;
  }

  const decodedWorkflows: WorkflowSummary[] = [];
  for (let index = 0; index < workflows.data.length; index += 1) {
    const workflow = decodeWorkflowItem(
      workflows.data[index],
      `project_workflows.workflows[${index}]`,
    );
    if (!workflow.ok) {
      return workflow;
    }
    decodedWorkflows.push(workflow.data);
  }

  return decodeOk({
    ...record.data,
    project: project.data,
    workflows: decodedWorkflows,
  });
}

export function decodeProjectRequirementsSummary(
  value: unknown,
): DecodeResult<ProjectRequirementSummary[]> {
  const rows = decodeArray(value, "project_requirements_summary");
  if (!rows.ok) {
    return rows;
  }

  const decoded: ProjectRequirementSummary[] = [];
  for (let index = 0; index < rows.data.length; index += 1) {
    const rowRecord = decodeRecord(rows.data[index], `project_requirements_summary[${index}]`);
    if (!rowRecord.ok) {
      return rowRecord;
    }

    const projectId = decodeStringField(
      rowRecord.data,
      "project_id",
      `project_requirements_summary[${index}]`,
    );
    if (!projectId.ok) {
      return projectId;
    }

    const projectName = decodeStringField(
      rowRecord.data,
      "project_name",
      `project_requirements_summary[${index}]`,
    );
    if (!projectName.ok) {
      return projectName;
    }

    decoded.push({
      ...rowRecord.data,
      project_id: projectId.data,
      project_name: projectName.data,
    });
  }

  return decodeOk(decoded);
}

export function decodeProjectRequirementsById(
  value: unknown,
): DecodeResult<ProjectRequirementsByIdPayload> {
  const record = decodeRecord(value, "project_requirements_by_id");
  if (!record.ok) {
    return record;
  }

  const projectId = decodeStringField(record.data, "project_id", "project_requirements_by_id");
  if (!projectId.ok) {
    return projectId;
  }

  const projectName = decodeStringField(record.data, "project_name", "project_requirements_by_id");
  if (!projectName.ok) {
    return projectName;
  }

  const requirements = decodeArray(record.data["requirements"], "project_requirements_by_id.requirements");
  if (!requirements.ok) {
    return requirements;
  }

  const decodedRequirements: RequirementSummary[] = [];
  for (let index = 0; index < requirements.data.length; index += 1) {
    const requirement = decodeRequirementSummary(
      requirements.data[index],
      `project_requirements_by_id.requirements[${index}]`,
    );
    if (!requirement.ok) {
      return requirement;
    }
    decodedRequirements.push(requirement.data);
  }

  return decodeOk({
    ...record.data,
    project_id: projectId.data,
    project_name: projectName.data,
    requirements: decodedRequirements,
  });
}

export function decodeProjectRequirementDetail(
  value: unknown,
): DecodeResult<ProjectRequirementDetailPayload> {
  const record = decodeRecord(value, "project_requirement_detail");
  if (!record.ok) {
    return record;
  }

  const projectId = decodeStringField(record.data, "project_id", "project_requirement_detail");
  if (!projectId.ok) {
    return projectId;
  }

  const projectName = decodeStringField(record.data, "project_name", "project_requirement_detail");
  if (!projectName.ok) {
    return projectName;
  }

  const requirement = decodeRequirementSummary(
    record.data["requirement"],
    "project_requirement_detail.requirement",
  );
  if (!requirement.ok) {
    return requirement;
  }

  return decodeOk({
    ...record.data,
    project_id: projectId.data,
    project_name: projectName.data,
    requirement: requirement.data,
  });
}

export function decodeTasksList(value: unknown): DecodeResult<TaskSummary[]> {
  const tasks = decodeArray(value, "tasks");
  if (!tasks.ok) {
    return tasks;
  }

  const decodedTasks: TaskSummary[] = [];
  for (let index = 0; index < tasks.data.length; index += 1) {
    const task = decodeTaskItem(tasks.data[index], `tasks[${index}]`, {
      requireStatus: true,
      requireType: true,
    });
    if (!task.ok) {
      return task;
    }
    decodedTasks.push(task.data as TaskSummary);
  }

  return decodeOk(decodedTasks);
}

export function decodeTaskStats(value: unknown): DecodeResult<TaskStatsPayload> {
  const record = decodeRecord(value, "task_stats");
  if (!record.ok) {
    return record;
  }

  const total = decodeOptionalNumberField(record.data, "total", "task_stats");
  if (!total.ok) {
    return total;
  }

  const inProgress = decodeOptionalNumberField(record.data, "in_progress", "task_stats");
  if (!inProgress.ok) {
    return inProgress;
  }

  const blocked = decodeOptionalNumberField(record.data, "blocked", "task_stats");
  if (!blocked.ok) {
    return blocked;
  }

  const completed = decodeOptionalNumberField(record.data, "completed", "task_stats");
  if (!completed.ok) {
    return completed;
  }

  const byStatusRaw = record.data["by_status"];
  const byPriorityRaw = record.data["by_priority"];
  const byTypeRaw = record.data["by_type"];

  const byStatus =
    byStatusRaw === undefined ? decodeOk(undefined) : decodeNumericRecord(byStatusRaw, "task_stats.by_status");
  if (!byStatus.ok) {
    return byStatus;
  }

  const byPriority =
    byPriorityRaw === undefined
      ? decodeOk(undefined)
      : decodeNumericRecord(byPriorityRaw, "task_stats.by_priority");
  if (!byPriority.ok) {
    return byPriority;
  }

  const byType =
    byTypeRaw === undefined ? decodeOk(undefined) : decodeNumericRecord(byTypeRaw, "task_stats.by_type");
  if (!byType.ok) {
    return byType;
  }

  return decodeOk({
    ...record.data,
    total: total.data,
    in_progress: inProgress.data,
    blocked: blocked.data,
    completed: completed.data,
    by_status: byStatus.data,
    by_priority: byPriority.data,
    by_type: byType.data,
  });
}

export function decodeTaskDetail(value: unknown): DecodeResult<TaskDetail> {
  const task = decodeTaskItem(value, "task_detail", {
    requireStatus: false,
    requireType: false,
  });
  if (!task.ok) {
    return task;
  }

  return decodeOk(task.data as TaskDetail);
}

export function decodeWorkflowsList(value: unknown): DecodeResult<WorkflowSummary[]> {
  const workflows = decodeArray(value, "workflows");
  if (!workflows.ok) {
    return workflows;
  }

  const decodedWorkflows: WorkflowSummary[] = [];
  for (let index = 0; index < workflows.data.length; index += 1) {
    const workflow = decodeWorkflowItem(workflows.data[index], `workflows[${index}]`);
    if (!workflow.ok) {
      return workflow;
    }
    decodedWorkflows.push(workflow.data);
  }

  return decodeOk(decodedWorkflows);
}

export function decodeWorkflowDetail(value: unknown): DecodeResult<WorkflowSummary> {
  return decodeWorkflowItem(value, "workflow_detail");
}

export function decodeWorkflowDecisions(value: unknown): DecodeResult<WorkflowDecision[]> {
  const decisions = decodeArray(value, "workflow_decisions");
  if (!decisions.ok) {
    return decisions;
  }

  const decodedDecisions: WorkflowDecision[] = [];
  for (let index = 0; index < decisions.data.length; index += 1) {
    const decision = decodeRecord(decisions.data[index], `workflow_decisions[${index}]`);
    if (!decision.ok) {
      return decision;
    }
    decodedDecisions.push(decision.data);
  }

  return decodeOk(decodedDecisions);
}

export function decodeWorkflowCheckpoints(
  value: unknown,
): DecodeResult<WorkflowCheckpoint[]> {
  const checkpoints = decodeArray(value, "workflow_checkpoints");
  if (!checkpoints.ok) {
    return checkpoints;
  }

  const decodedCheckpoints: WorkflowCheckpoint[] = [];
  for (let index = 0; index < checkpoints.data.length; index += 1) {
    const checkpoint = decodeRecord(checkpoints.data[index], `workflow_checkpoints[${index}]`);
    if (!checkpoint.ok) {
      return checkpoint;
    }
    decodedCheckpoints.push(checkpoint.data);
  }

  return decodeOk(decodedCheckpoints);
}

export function decodeWorkflowCheckpointDetail(
  value: unknown,
): DecodeResult<WorkflowCheckpointDetail> {
  const checkpoint = decodeRecord(value, "workflow_checkpoint");
  if (!checkpoint.ok) {
    return checkpoint;
  }

  return decodeOk(checkpoint.data);
}

export function decodeReviewHandoffResponse(
  value: unknown,
): DecodeResult<ReviewHandoffResponse> {
  const record = decodeRecord(value, "review_handoff");
  if (!record.ok) {
    return record;
  }

  const status = decodeStringField(record.data, "status", "review_handoff");
  if (!status.ok) {
    return status;
  }

  return decodeOk({
    ...record.data,
    status: status.data,
  });
}
