import type {
  DaemonHealth,
  DaemonLogEntry,
  DaemonStatusValue,
  JsonRecord,
  MessagePayload,
  PlanningRequirementItem,
  PlanningRequirementsDraftResult,
  PlanningRequirementsRefineResult,
  PlanningVisionDocument,
  PlanningVisionRefineResult,
  ProjectDetail,
  ProjectRequirementDetailPayload,
  ProjectRequirementSummary,
  ProjectRequirementsByIdPayload,
  ProjectSummary,
  RequirementPriorityValue,
  RequirementStatusValue,
  ProjectTasksPayload,
  ProjectWorkflowsPayload,
  RequirementSummary,
  RequirementTypeValue,
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
  normalizeRequirementPriority,
  normalizeRequirementStatus,
  normalizeRequirementType,
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

function decodeOptionalRecordField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<JsonRecord | undefined> {
  const value = record[key];
  if (value === undefined) {
    return decodeOk(undefined);
  }

  const decodedRecord = decodeRecord(value, `${context}.${key}`);
  if (!decodedRecord.ok) {
    return decodedRecord;
  }

  return decodeOk(decodedRecord.data);
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

function decodeStringArray(
  value: unknown,
  context: string,
): DecodeResult<string[]> {
  const array = decodeArray(value, context);
  if (!array.ok) {
    return array;
  }

  const decoded: string[] = [];
  for (let index = 0; index < array.data.length; index += 1) {
    const item = array.data[index];
    if (typeof item !== "string") {
      return decodeError(`${context}[${index}] must be a string`);
    }
    decoded.push(item);
  }

  return decodeOk(decoded);
}

function decodeOptionalStringArrayField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<string[] | undefined> {
  const value = record[key];
  if (value === undefined) {
    return decodeOk(undefined);
  }

  return decodeStringArray(value, `${context}.${key}`);
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

function decodeRequirementPriorityField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<RequirementPriorityValue> {
  const value = decodeStringField(record, key, context);
  if (!value.ok) {
    return value;
  }

  return decodeOk(normalizeRequirementPriority(value.data));
}

function decodeRequirementStatusField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<RequirementStatusValue> {
  const value = decodeStringField(record, key, context);
  if (!value.ok) {
    return value;
  }

  return decodeOk(normalizeRequirementStatus(value.data));
}

function decodeOptionalRequirementTypeField(
  record: JsonRecord,
  key: string,
  context: string,
): DecodeResult<RequirementTypeValue | undefined> {
  const value = decodeOptionalStringField(record, key, context);
  if (!value.ok) {
    return value;
  }

  if (value.data === undefined) {
    return decodeOk(undefined);
  }

  return decodeOk(normalizeRequirementType(value.data));
}

function decodePlanningRequirementItem(
  value: unknown,
  context: string,
): DecodeResult<PlanningRequirementItem> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  const title = decodeStringField(record.data, "title", context);
  if (!title.ok) {
    return title;
  }

  const description = decodeStringField(record.data, "description", context);
  if (!description.ok) {
    return description;
  }

  const body = decodeOptionalStringField(record.data, "body", context);
  if (!body.ok) {
    return body;
  }

  const category = decodeOptionalStringField(record.data, "category", context);
  if (!category.ok) {
    return category;
  }

  const requirementType = decodeOptionalRequirementTypeField(
    record.data,
    "requirement_type",
    context,
  );
  if (!requirementType.ok) {
    return requirementType;
  }

  const acceptanceCriteria = decodeOptionalStringArrayField(
    record.data,
    "acceptance_criteria",
    context,
  );
  if (!acceptanceCriteria.ok) {
    return acceptanceCriteria;
  }

  const priority = decodeRequirementPriorityField(record.data, "priority", context);
  if (!priority.ok) {
    return priority;
  }

  const status = decodeRequirementStatusField(record.data, "status", context);
  if (!status.ok) {
    return status;
  }

  const source = decodeStringField(record.data, "source", context);
  if (!source.ok) {
    return source;
  }

  const tags = decodeOptionalStringArrayField(record.data, "tags", context);
  if (!tags.ok) {
    return tags;
  }

  const linkedTaskIds = decodeOptionalStringArrayField(record.data, "linked_task_ids", context);
  if (!linkedTaskIds.ok) {
    return linkedTaskIds;
  }

  const relativePath = decodeOptionalStringField(record.data, "relative_path", context);
  if (!relativePath.ok) {
    return relativePath;
  }

  const createdAt = decodeStringField(record.data, "created_at", context);
  if (!createdAt.ok) {
    return createdAt;
  }

  const updatedAt = decodeStringField(record.data, "updated_at", context);
  if (!updatedAt.ok) {
    return updatedAt;
  }

  return decodeOk({
    ...record.data,
    id: id.data,
    title: title.data,
    description: description.data,
    body: body.data,
    category: category.data,
    requirement_type: requirementType.data,
    acceptance_criteria: acceptanceCriteria.data ?? [],
    priority: priority.data,
    status: status.data,
    source: source.data,
    tags: tags.data ?? [],
    linked_task_ids: linkedTaskIds.data ?? [],
    relative_path: relativePath.data,
    created_at: createdAt.data,
    updated_at: updatedAt.data,
  });
}

function decodePlanningVisionDocument(
  value: unknown,
  context: string,
): DecodeResult<PlanningVisionDocument> {
  const record = decodeRecord(value, context);
  if (!record.ok) {
    return record;
  }

  const id = decodeStringField(record.data, "id", context);
  if (!id.ok) {
    return id;
  }

  const projectRoot = decodeStringField(record.data, "project_root", context);
  if (!projectRoot.ok) {
    return projectRoot;
  }

  const markdown = decodeStringField(record.data, "markdown", context);
  if (!markdown.ok) {
    return markdown;
  }

  const problemStatement = decodeStringField(record.data, "problem_statement", context);
  if (!problemStatement.ok) {
    return problemStatement;
  }

  const targetUsers = decodeOptionalStringArrayField(record.data, "target_users", context);
  if (!targetUsers.ok) {
    return targetUsers;
  }

  const goals = decodeOptionalStringArrayField(record.data, "goals", context);
  if (!goals.ok) {
    return goals;
  }

  const constraints = decodeOptionalStringArrayField(record.data, "constraints", context);
  if (!constraints.ok) {
    return constraints;
  }

  const valueProposition = decodeOptionalStringField(record.data, "value_proposition", context);
  if (!valueProposition.ok) {
    return valueProposition;
  }

  const createdAt = decodeStringField(record.data, "created_at", context);
  if (!createdAt.ok) {
    return createdAt;
  }

  const updatedAt = decodeStringField(record.data, "updated_at", context);
  if (!updatedAt.ok) {
    return updatedAt;
  }

  return decodeOk({
    ...record.data,
    id: id.data,
    project_root: projectRoot.data,
    markdown: markdown.data,
    problem_statement: problemStatement.data,
    target_users: targetUsers.data ?? [],
    goals: goals.data ?? [],
    constraints: constraints.data ?? [],
    value_proposition: valueProposition.data,
    created_at: createdAt.data,
    updated_at: updatedAt.data,
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

export function decodeVisionDocumentNullable(
  value: unknown,
): DecodeResult<PlanningVisionDocument | null> {
  if (value === null) {
    return decodeOk(null);
  }

  return decodePlanningVisionDocument(value, "planning_vision");
}

export function decodeVisionDocument(
  value: unknown,
): DecodeResult<PlanningVisionDocument> {
  return decodePlanningVisionDocument(value, "planning_vision");
}

export function decodeVisionRefineResult(
  value: unknown,
): DecodeResult<PlanningVisionRefineResult> {
  const record = decodeRecord(value, "planning_vision_refine");
  if (!record.ok) {
    return record;
  }

  const updatedVision = decodePlanningVisionDocument(
    record.data["updated_vision"],
    "planning_vision_refine.updated_vision",
  );
  if (!updatedVision.ok) {
    return updatedVision;
  }

  const refinement = decodeRecord(record.data["refinement"], "planning_vision_refine.refinement");
  if (!refinement.ok) {
    return refinement;
  }

  const mode = decodeStringField(
    refinement.data,
    "mode",
    "planning_vision_refine.refinement",
  );
  if (!mode.ok) {
    return mode;
  }

  const focus = decodeOptionalStringField(
    refinement.data,
    "focus",
    "planning_vision_refine.refinement",
  );
  if (!focus.ok) {
    return focus;
  }

  const rationale = decodeOptionalStringField(
    refinement.data,
    "rationale",
    "planning_vision_refine.refinement",
  );
  if (!rationale.ok) {
    return rationale;
  }

  const changes = decodeOptionalRecordField(
    refinement.data,
    "changes",
    "planning_vision_refine.refinement",
  );
  if (!changes.ok) {
    return changes;
  }

  return decodeOk({
    ...record.data,
    updated_vision: updatedVision.data,
    refinement: {
      ...refinement.data,
      mode: mode.data,
      focus: focus.data,
      rationale: rationale.data,
      changes: changes.data,
    },
  });
}

export function decodePlanningRequirementsList(
  value: unknown,
): DecodeResult<PlanningRequirementItem[]> {
  const requirements = decodeArray(value, "planning_requirements");
  if (!requirements.ok) {
    return requirements;
  }

  const decoded: PlanningRequirementItem[] = [];
  for (let index = 0; index < requirements.data.length; index += 1) {
    const requirement = decodePlanningRequirementItem(
      requirements.data[index],
      `planning_requirements[${index}]`,
    );
    if (!requirement.ok) {
      return requirement;
    }
    decoded.push(requirement.data);
  }

  return decodeOk(decoded);
}

export function decodePlanningRequirementDetail(
  value: unknown,
): DecodeResult<PlanningRequirementItem> {
  return decodePlanningRequirementItem(value, "planning_requirement_detail");
}

export function decodePlanningRequirementsDraftResult(
  value: unknown,
): DecodeResult<PlanningRequirementsDraftResult> {
  const record = decodeRecord(value, "planning_requirements_draft");
  if (!record.ok) {
    return record;
  }

  const requirements = decodePlanningRequirementsList(record.data["requirements"]);
  if (!requirements.ok) {
    return decodeError(`planning_requirements_draft.${requirements.message}`);
  }

  const appendedCount = decodeOptionalNumberField(
    record.data,
    "appended_count",
    "planning_requirements_draft",
  );
  if (!appendedCount.ok) {
    return appendedCount;
  }

  if (appendedCount.data === undefined) {
    return decodeError("planning_requirements_draft.appended_count must be a number");
  }

  return decodeOk({
    ...record.data,
    requirements: requirements.data,
    appended_count: appendedCount.data,
  });
}

export function decodePlanningRequirementsRefineResult(
  value: unknown,
): DecodeResult<PlanningRequirementsRefineResult> {
  const record = decodeRecord(value, "planning_requirements_refine");
  if (!record.ok) {
    return record;
  }

  const requirements = decodePlanningRequirementsList(record.data["requirements"]);
  if (!requirements.ok) {
    return decodeError(`planning_requirements_refine.${requirements.message}`);
  }

  const updatedIds = decodeStringArray(
    record.data["updated_ids"],
    "planning_requirements_refine.updated_ids",
  );
  if (!updatedIds.ok) {
    return updatedIds;
  }

  const requestedIds = decodeStringArray(
    record.data["requested_ids"],
    "planning_requirements_refine.requested_ids",
  );
  if (!requestedIds.ok) {
    return requestedIds;
  }

  const scope = decodeStringField(record.data, "scope", "planning_requirements_refine");
  if (!scope.ok) {
    return scope;
  }

  const focus = decodeOptionalStringField(record.data, "focus", "planning_requirements_refine");
  if (!focus.ok) {
    return focus;
  }

  if (scope.data !== "selected" && scope.data !== "all") {
    return decodeError("planning_requirements_refine.scope must be selected or all");
  }

  return decodeOk({
    ...record.data,
    requirements: requirements.data,
    updated_ids: updatedIds.data,
    requested_ids: requestedIds.data,
    scope: scope.data,
    focus: focus.data,
  });
}
