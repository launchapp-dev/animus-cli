import type { Task, Requirement, Project } from "./types";

const BASE = import.meta.env.VITE_API_URL || "";

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    credentials: "include",
    headers: { "Content-Type": "application/json", ...options?.headers },
    ...options,
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(body.error || `${res.status} ${res.statusText}`);
  }
  return res.json();
}

export const api = {
  projects: {
    list: () => apiFetch<{ projects: Project[] }>("/api/projects"),
    get: (id: string) => apiFetch<{ project: Project }>(`/api/projects/${id}`),
    create: (data: { name: string; organizationId: string; repoOriginUrl: string }) =>
      apiFetch<{ project: Project }>("/api/projects", { method: "POST", body: JSON.stringify(data) }),
    delete: (id: string) => apiFetch(`/api/projects/${id}`, { method: "DELETE" }),
  },
  tasks: {
    list: (projectId: string, params?: Record<string, string>) => {
      const qs = params ? "?" + new URLSearchParams(params).toString() : "";
      return apiFetch<{ tasks: Task[]; total: number; limit: number; offset: number }>(
        `/api/projects/${projectId}/tasks${qs}`
      );
    },
    get: (projectId: string, taskId: string) =>
      apiFetch<{ task: Task }>(`/api/projects/${projectId}/tasks/${taskId}`),
  },
  requirements: {
    list: (projectId: string, params?: Record<string, string>) => {
      const qs = params ? "?" + new URLSearchParams(params).toString() : "";
      return apiFetch<{ requirements: Requirement[]; total: number; limit: number; offset: number }>(
        `/api/projects/${projectId}/requirements${qs}`
      );
    },
    get: (projectId: string, reqId: string) =>
      apiFetch<{ requirement: Requirement }>(`/api/projects/${projectId}/requirements/${reqId}`),
  },
  metrics: {
    get: (projectId: string) =>
      apiFetch<ProjectMetrics>(`/api/projects/${projectId}/metrics`),
  },
};

export interface MetricBucket {
  name: string;
  value: number;
}

export interface TimelineBucket {
  week: string;
  created: number;
  completed: number;
}

export interface ProjectMetrics {
  tasks: {
    total: number;
    by_status: MetricBucket[];
    by_priority: MetricBucket[];
    by_type: MetricBucket[];
  };
  requirements: {
    total: number;
    by_status: MetricBucket[];
    by_priority: MetricBucket[];
  };
  timeline: TimelineBucket[];
}
