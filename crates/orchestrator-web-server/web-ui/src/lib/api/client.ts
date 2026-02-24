import { ApiError, ApiResult, parseAoEnvelope } from "./envelope";

export type RequestJsonValue =
  | null
  | boolean
  | number
  | string
  | RequestJsonValue[]
  | { [key: string]: RequestJsonValue };

const JSON_HEADERS = {
  Accept: "application/json",
  "Content-Type": "application/json",
};

export async function requestAo<TData>(
  path: string,
  init: RequestInit = {},
): Promise<ApiResult<TData>> {
  const requestInit: RequestInit = {
    ...init,
    headers: {
      ...JSON_HEADERS,
      ...(init.headers ?? {}),
    },
  };

  try {
    const response = await fetch(path, requestInit);
    const payload = (await response.json()) as unknown;
    return parseAoEnvelope<TData>(payload);
  } catch (error) {
    return {
      kind: "error",
      code: "network_error",
      message: error instanceof Error ? error.message : "Request failed.",
      exitCode: 5,
    };
  }
}

export function isApiError<TData>(result: ApiResult<TData>): result is ApiError {
  return result.kind === "error";
}

export function firstApiError(
  ...results: Array<ApiResult<unknown>>
): ApiError | null {
  for (const result of results) {
    if (result.kind === "error") {
      return result;
    }
  }

  return null;
}

export const api = {
  systemInfo: () => requestAo<unknown>("/api/v1/system/info"),
  daemonStatus: () => requestAo<unknown>("/api/v1/daemon/status"),
  daemonHealth: () => requestAo<unknown>("/api/v1/daemon/health"),
  daemonLogs: (limit = 100) => requestAo<unknown>(`/api/v1/daemon/logs?limit=${limit}`),
  daemonStart: () => postEmpty("/api/v1/daemon/start"),
  daemonPause: () => postEmpty("/api/v1/daemon/pause"),
  daemonResume: () => postEmpty("/api/v1/daemon/resume"),
  daemonStop: () => postEmpty("/api/v1/daemon/stop"),
  daemonClearLogs: () => requestAo<unknown>("/api/v1/daemon/logs", { method: "DELETE" }),
  projectsList: () => requestAo<Array<ProjectSummary>>("/api/v1/projects"),
  projectsActive: () => requestAo<ProjectSummary | null>("/api/v1/projects/active"),
  projectsById: (projectId: string) => requestAo<unknown>(`/api/v1/projects/${projectId}`),
  projectTasks: (projectId: string) => requestAo<unknown>(`/api/v1/projects/${projectId}/tasks`),
  projectWorkflows: (projectId: string) =>
    requestAo<unknown>(`/api/v1/projects/${projectId}/workflows`),
  projectsRequirementsSummary: () => requestAo<unknown>("/api/v1/project-requirements"),
  projectsRequirementsById: (projectId: string) =>
    requestAo<unknown>(`/api/v1/project-requirements/${projectId}`),
  projectRequirementDetail: (projectId: string, requirementId: string) =>
    requestAo<unknown>(`/api/v1/project-requirements/${projectId}/${requirementId}`),
  tasksList: () => requestAo<unknown>("/api/v1/tasks"),
  tasksStats: () => requestAo<unknown>("/api/v1/tasks/stats"),
  tasksById: (taskId: string) => requestAo<unknown>(`/api/v1/tasks/${taskId}`),
  workflowsList: () => requestAo<unknown>("/api/v1/workflows"),
  workflowsById: (workflowId: string) => requestAo<unknown>(`/api/v1/workflows/${workflowId}`),
  workflowDecisions: (workflowId: string) =>
    requestAo<unknown>(`/api/v1/workflows/${workflowId}/decisions`),
  workflowCheckpoints: (workflowId: string) =>
    requestAo<unknown>(`/api/v1/workflows/${workflowId}/checkpoints`),
  workflowCheckpointById: (workflowId: string, checkpoint: string) =>
    requestAo<unknown>(`/api/v1/workflows/${workflowId}/checkpoints/${checkpoint}`),
  reviewHandoff: (payload: RequestJsonValue) =>
    requestAo<unknown>("/api/v1/reviews/handoff", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
};

async function postEmpty(path: string): Promise<ApiResult<unknown>> {
  return requestAo(path, {
    method: "POST",
    body: JSON.stringify({}),
  });
}

export type ProjectSummary = {
  id: string;
  name: string;
  archived?: boolean;
};
