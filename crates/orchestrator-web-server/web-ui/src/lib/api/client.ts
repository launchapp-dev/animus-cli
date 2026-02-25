import {
  decodeDaemonHealth,
  decodeDaemonLogs,
  decodeDaemonStatus,
  decodeMessagePayload,
  decodeProjectDetail,
  decodeProjectRequirementDetail,
  decodeProjectRequirementsById,
  decodeProjectRequirementsSummary,
  decodeProjectsActive,
  decodeProjectsList,
  decodeProjectTasksPayload,
  decodeProjectWorkflowsPayload,
  decodeReviewHandoffResponse,
  decodeSystemInfo,
  decodeTaskDetail,
  decodeTaskStats,
  decodeTasksList,
  decodeWorkflowCheckpointDetail,
  decodeWorkflowCheckpoints,
  decodeWorkflowDecisions,
  decodeWorkflowDetail,
  decodeWorkflowsList,
} from "./contracts/guards";
import type { PayloadDecoder } from "./contracts/guards";
import type {
  DaemonHealth,
  DaemonLogEntry,
  DaemonStatusValue,
  MessagePayload,
  ProjectDetail,
  ProjectRequirementDetailPayload,
  ProjectRequirementSummary,
  ProjectRequirementsByIdPayload,
  ProjectSummary,
  ProjectTasksPayload,
  ProjectWorkflowsPayload,
  ReviewHandoffResponse,
  SystemInfo,
  TaskDetail,
  TaskStatsPayload,
  TaskSummary,
  WorkflowCheckpoint,
  WorkflowCheckpointDetail,
  WorkflowDecision,
  WorkflowSummary,
} from "./contracts/models";
import { ApiError, ApiResult, parseAoEnvelope } from "./envelope";
import {
  AO_CORRELATION_HEADER,
  generateCorrelationId,
  recordTelemetryEvent,
  resolveCorrelationId,
  sanitizeHeadersForTelemetry,
  sanitizePathForTelemetry,
  sanitizeRequestBodyForTelemetry,
  sanitizeResponseBodyForTelemetry,
} from "../telemetry";
import type {
  TelemetryEvent,
  TelemetryRequestSummary,
  TelemetryResponseSummary,
} from "../telemetry";

export type RequestJsonValue =
  | null
  | boolean
  | number
  | string
  | RequestJsonValue[]
  | { [key: string]: RequestJsonValue };

export type RequestAoOptions = {
  actionName?: string;
  correlationId?: string;
};

const JSON_HEADERS = {
  Accept: "application/json",
  "Content-Type": "application/json",
};

export async function requestAo<TData>(
  path: string,
  init: RequestInit = {},
  decoder?: PayloadDecoder<TData>,
  options: RequestAoOptions = {},
): Promise<ApiResult<TData>> {
  const method = normalizeMethod(init.method);
  const { path: requestPath, query } = sanitizePathForTelemetry(path);
  const requestHeaders = createRequestHeaders(init.headers);

  const requestCorrelationId =
    resolveCorrelationId(options.correlationId, requestHeaders.get(AO_CORRELATION_HEADER)) ??
    generateCorrelationId();
  requestHeaders.set(AO_CORRELATION_HEADER, requestCorrelationId);

  const requestBody = sanitizeRequestBodyForTelemetry(init.body);
  const requestSummary: TelemetryRequestSummary = {
    headers: sanitizeHeadersForTelemetry(requestHeaders),
    query,
    ...(requestBody !== undefined ? { body: requestBody } : {}),
  };

  const actionName = options.actionName ?? `${method} ${requestPath}`;
  const startedAt = Date.now();
  emitTelemetrySafe({
    eventType: "request_start",
    timestamp: new Date(startedAt).toISOString(),
    correlationId: requestCorrelationId,
    method,
    path: requestPath,
    action: actionName,
    request: requestSummary,
  });

  const requestInit: RequestInit = {
    ...init,
    method,
    headers: requestHeaders,
  };

  try {
    const response = await fetch(path, requestInit);
    const canonicalCorrelationId =
      resolveCorrelationId(response.headers.get(AO_CORRELATION_HEADER), requestCorrelationId) ??
      requestCorrelationId;
    const httpStatus = normalizeHttpStatus(response.status);
    const responseHeaders = sanitizeHeadersForTelemetry(response.headers);

    let payload: unknown;
    try {
      payload = await response.json();
    } catch (error) {
      const apiError = createApiError({
        code: "invalid_json",
        message:
          error instanceof Error
            ? `Invalid JSON response for ${path}: ${error.message}`
            : `Invalid JSON response for ${path}.`,
        exitCode: 1,
        correlationId: canonicalCorrelationId,
        httpStatus,
        requestPath,
        method,
      });

      emitFailureTelemetry({
        startedAt,
        correlationId: canonicalCorrelationId,
        method,
        requestPath,
        actionName,
        requestSummary,
        responseSummary: { headers: responseHeaders },
        error: apiError,
        httpStatus,
      });

      return apiError;
    }

    const responseSummary = buildResponseSummary(responseHeaders, payload);
    const envelope = parseAoEnvelope<unknown>(payload);
    if (envelope.kind === "error") {
      const apiError = createApiError({
        code: envelope.code,
        message: envelope.message,
        exitCode: envelope.exitCode,
        correlationId: canonicalCorrelationId,
        httpStatus,
        requestPath,
        method,
      });

      emitFailureTelemetry({
        startedAt,
        correlationId: canonicalCorrelationId,
        method,
        requestPath,
        actionName,
        requestSummary,
        responseSummary,
        error: apiError,
        httpStatus,
      });

      return apiError;
    }

    if (!decoder) {
      emitTelemetrySafe({
        eventType: "request_success",
        timestamp: new Date().toISOString(),
        correlationId: canonicalCorrelationId,
        method,
        path: requestPath,
        action: actionName,
        durationMs: elapsedSince(startedAt),
        httpStatus,
        request: requestSummary,
        response: responseSummary,
      });

      return {
        kind: "ok",
        data: envelope.data as TData,
      };
    }

    const decoded = decoder(envelope.data);
    if (!decoded.ok) {
      const apiError = createApiError({
        code: "invalid_payload",
        message: `Invalid payload for ${path}: ${decoded.message}`,
        exitCode: 1,
        correlationId: canonicalCorrelationId,
        httpStatus,
        requestPath,
        method,
      });

      emitFailureTelemetry({
        startedAt,
        correlationId: canonicalCorrelationId,
        method,
        requestPath,
        actionName,
        requestSummary,
        responseSummary,
        error: apiError,
        httpStatus,
      });

      return apiError;
    }

    emitTelemetrySafe({
      eventType: "request_success",
      timestamp: new Date().toISOString(),
      correlationId: canonicalCorrelationId,
      method,
      path: requestPath,
      action: actionName,
      durationMs: elapsedSince(startedAt),
      httpStatus,
      request: requestSummary,
      response: responseSummary,
    });

    return {
      kind: "ok",
      data: decoded.data,
    };
  } catch (error) {
    const apiError = createApiError({
      code: "network_error",
      message: error instanceof Error ? error.message : "Request failed.",
      exitCode: 5,
      correlationId: requestCorrelationId,
      requestPath,
      method,
    });

    emitFailureTelemetry({
      startedAt,
      correlationId: requestCorrelationId,
      method,
      requestPath,
      actionName,
      requestSummary,
      error: apiError,
    });

    return apiError;
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

function getAo<TData>(
  path: string,
  decoder: PayloadDecoder<TData>,
): Promise<ApiResult<TData>> {
  return requestAo(path, {}, decoder);
}

export const api = {
  systemInfo: () => getAo<SystemInfo>("/api/v1/system/info", decodeSystemInfo),
  daemonStatus: () => getAo<DaemonStatusValue>("/api/v1/daemon/status", decodeDaemonStatus),
  daemonHealth: () => getAo<DaemonHealth>("/api/v1/daemon/health", decodeDaemonHealth),
  daemonLogs: (limit = 100) =>
    getAo<DaemonLogEntry[]>(`/api/v1/daemon/logs?limit=${limit}`, decodeDaemonLogs),
  daemonStart: () => postEmpty("/api/v1/daemon/start", "daemon.start"),
  daemonPause: () => postEmpty("/api/v1/daemon/pause", "daemon.pause"),
  daemonResume: () => postEmpty("/api/v1/daemon/resume", "daemon.resume"),
  daemonStop: () => postEmpty("/api/v1/daemon/stop", "daemon.stop"),
  daemonClearLogs: () =>
    requestAo<MessagePayload>(
      "/api/v1/daemon/logs",
      { method: "DELETE" },
      decodeMessagePayload,
      { actionName: "daemon.clear_logs" },
    ),
  projectsList: () => getAo<ProjectSummary[]>("/api/v1/projects", decodeProjectsList),
  projectsActive: () =>
    getAo<ProjectSummary | null>("/api/v1/projects/active", decodeProjectsActive),
  projectsById: (projectId: string) =>
    getAo<ProjectDetail>(`/api/v1/projects/${projectId}`, decodeProjectDetail),
  projectTasks: (projectId: string) =>
    getAo<ProjectTasksPayload>(`/api/v1/projects/${projectId}/tasks`, decodeProjectTasksPayload),
  projectWorkflows: (projectId: string) =>
    getAo<ProjectWorkflowsPayload>(
      `/api/v1/projects/${projectId}/workflows`,
      decodeProjectWorkflowsPayload,
    ),
  projectsRequirementsSummary: () =>
    getAo<ProjectRequirementSummary[]>(
      "/api/v1/project-requirements",
      decodeProjectRequirementsSummary,
    ),
  projectsRequirementsById: (projectId: string) =>
    getAo<ProjectRequirementsByIdPayload>(
      `/api/v1/project-requirements/${projectId}`,
      decodeProjectRequirementsById,
    ),
  projectRequirementDetail: (projectId: string, requirementId: string) =>
    getAo<ProjectRequirementDetailPayload>(
      `/api/v1/project-requirements/${projectId}/${requirementId}`,
      decodeProjectRequirementDetail,
    ),
  tasksList: () => getAo<TaskSummary[]>("/api/v1/tasks", decodeTasksList),
  tasksStats: () => getAo<TaskStatsPayload>("/api/v1/tasks/stats", decodeTaskStats),
  tasksById: (taskId: string) => getAo<TaskDetail>(`/api/v1/tasks/${taskId}`, decodeTaskDetail),
  workflowsList: () => getAo<WorkflowSummary[]>("/api/v1/workflows", decodeWorkflowsList),
  workflowsById: (workflowId: string) =>
    getAo<WorkflowSummary>(`/api/v1/workflows/${workflowId}`, decodeWorkflowDetail),
  workflowDecisions: (workflowId: string) =>
    getAo<WorkflowDecision[]>(
      `/api/v1/workflows/${workflowId}/decisions`,
      decodeWorkflowDecisions,
    ),
  workflowCheckpoints: (workflowId: string) =>
    getAo<WorkflowCheckpoint[]>(
      `/api/v1/workflows/${workflowId}/checkpoints`,
      decodeWorkflowCheckpoints,
    ),
  workflowCheckpointById: (workflowId: string, checkpoint: string) =>
    getAo<WorkflowCheckpointDetail>(
      `/api/v1/workflows/${workflowId}/checkpoints/${checkpoint}`,
      decodeWorkflowCheckpointDetail,
    ),
  reviewHandoff: (payload: RequestJsonValue) =>
    requestAo<ReviewHandoffResponse>(
      "/api/v1/reviews/handoff",
      {
        method: "POST",
        body: JSON.stringify(payload),
      },
      decodeReviewHandoffResponse,
      { actionName: "reviews.handoff.submit" },
    ),
};

async function postEmpty(
  path: string,
  actionName: string,
): Promise<ApiResult<MessagePayload>> {
  return requestAo(
    path,
    {
      method: "POST",
      body: JSON.stringify({}),
    },
    decodeMessagePayload,
    { actionName },
  );
}

function normalizeMethod(method: string | undefined): string {
  return (method ?? "GET").toUpperCase();
}

function createRequestHeaders(headers?: HeadersInit): Headers {
  const merged = new Headers(JSON_HEADERS);
  if (!headers) {
    return merged;
  }

  const inputHeaders = new Headers(headers);
  for (const [key, value] of inputHeaders.entries()) {
    merged.set(key, value);
  }

  return merged;
}

function createApiError(params: {
  code: string;
  message: string;
  exitCode: number;
  correlationId: string;
  requestPath: string;
  method: string;
  httpStatus?: number;
}): ApiError {
  return {
    kind: "error",
    code: params.code,
    message: params.message,
    exitCode: params.exitCode,
    correlationId: params.correlationId,
    requestPath: params.requestPath,
    method: params.method,
    ...(params.httpStatus !== undefined ? { httpStatus: params.httpStatus } : {}),
  };
}

function buildResponseSummary(
  headers: Record<string, string>,
  body: unknown,
): TelemetryResponseSummary {
  const sanitizedBody = sanitizeResponseBodyForTelemetry(body);
  return {
    headers,
    ...(sanitizedBody !== undefined ? { body: sanitizedBody } : {}),
  };
}

function emitFailureTelemetry(params: {
  startedAt: number;
  correlationId: string;
  method: string;
  requestPath: string;
  actionName: string;
  requestSummary: TelemetryRequestSummary;
  error: ApiError;
  responseSummary?: TelemetryResponseSummary;
  httpStatus?: number;
}): void {
  emitTelemetrySafe({
    eventType: "request_failure",
    timestamp: new Date().toISOString(),
    correlationId: params.correlationId,
    method: params.method,
    path: params.requestPath,
    action: params.actionName,
    durationMs: elapsedSince(params.startedAt),
    httpStatus: params.httpStatus,
    request: params.requestSummary,
    error: {
      code: params.error.code,
      message: params.error.message,
      exitCode: params.error.exitCode,
    },
    ...(params.responseSummary ? { response: params.responseSummary } : {}),
  });
}

function emitTelemetrySafe(event: TelemetryEvent): void {
  try {
    recordTelemetryEvent(event);
  } catch {
    // Telemetry must never break request execution.
  }
}

function elapsedSince(startedAt: number): number {
  return Math.max(Date.now() - startedAt, 0);
}

function normalizeHttpStatus(status: number | undefined): number | undefined {
  if (typeof status !== "number") {
    return undefined;
  }

  if (!Number.isFinite(status) || status <= 0) {
    return undefined;
  }

  return Math.floor(status);
}

export type { ProjectSummary };
