import { useState, useMemo } from "react";
import { useParams, Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import type { Task, Requirement } from "@/lib/types";
import { ProjectDataTab } from "./project-data";

function statusVariant(status: string) {
  if (["done", "implemented", "approved"].includes(status)) return "success" as const;
  if (["in-progress"].includes(status)) return "default" as const;
  if (["blocked", "cancelled", "deprecated"].includes(status)) return "destructive" as const;
  if (["on-hold", "needs-rework"].includes(status)) return "warning" as const;
  return "secondary" as const;
}

function priorityVariant(priority: string) {
  if (priority === "critical") return "destructive" as const;
  if (priority === "high" || priority === "must") return "warning" as const;
  return "secondary" as const;
}

function StatCard({ label, value, sub }: { label: string; value: string | number; sub?: string }) {
  return (
    <Card>
      <CardContent className="p-4">
        <div className="text-2xl font-bold">{value}</div>
        <div className="text-sm text-muted-foreground">{label}</div>
        {sub && <div className="text-xs text-muted-foreground mt-1">{sub}</div>}
      </CardContent>
    </Card>
  );
}

export function ProjectPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const [tab, setTab] = useState<"tasks" | "requirements" | "data">("tasks");
  const [statusFilter, setStatusFilter] = useState("");
  const [priorityFilter, setPriorityFilter] = useState("");
  const [search, setSearch] = useState("");
  const [offset, setOffset] = useState(0);
  const limit = 50;

  const { data: projectData } = useQuery({
    queryKey: ["project", projectId],
    queryFn: () => api.projects.get(projectId!),
    enabled: !!projectId,
  });

  const { data: allTasksData } = useQuery({
    queryKey: ["tasks-all", projectId],
    queryFn: () => api.tasks.list(projectId!, { limit: "1" }),
    enabled: !!projectId,
  });

  const { data: allReqsData } = useQuery({
    queryKey: ["reqs-all", projectId],
    queryFn: () => api.requirements.list(projectId!, { limit: "1" }),
    enabled: !!projectId,
  });

  const taskParams: Record<string, string> = { limit: String(limit), offset: String(offset) };
  if (statusFilter) taskParams.status = statusFilter;
  if (priorityFilter) taskParams.priority = priorityFilter;

  const { data: tasksData, isLoading: tasksLoading } = useQuery({
    queryKey: ["tasks", projectId, taskParams],
    queryFn: () => api.tasks.list(projectId!, taskParams),
    enabled: !!projectId && tab === "tasks",
  });

  const reqParams: Record<string, string> = { limit: String(limit), offset: String(offset) };
  if (statusFilter) reqParams.status = statusFilter;
  if (priorityFilter) reqParams.priority = priorityFilter;

  const { data: reqsData, isLoading: reqsLoading } = useQuery({
    queryKey: ["requirements", projectId, reqParams],
    queryFn: () => api.requirements.list(projectId!, reqParams),
    enabled: !!projectId && tab === "requirements",
  });

  const { data: doneTasksData } = useQuery({
    queryKey: ["tasks-done", projectId],
    queryFn: () => api.tasks.list(projectId!, { status: "done", limit: "1" }),
    enabled: !!projectId,
  });

  const { data: inProgressData } = useQuery({
    queryKey: ["tasks-inprogress", projectId],
    queryFn: () => api.tasks.list(projectId!, { status: "in-progress", limit: "1" }),
    enabled: !!projectId,
  });

  const { data: blockedData } = useQuery({
    queryKey: ["tasks-blocked", projectId],
    queryFn: () => api.tasks.list(projectId!, { status: "blocked", limit: "1" }),
    enabled: !!projectId,
  });

  const project = projectData?.project;
  const tasks = tasksData?.tasks ?? [];
  const reqs = reqsData?.requirements ?? [];
  const total = tab === "tasks" ? (tasksData?.total ?? 0) : (reqsData?.total ?? 0);
  const totalTasks = allTasksData?.total ?? 0;
  const totalReqs = allReqsData?.total ?? 0;
  const doneTasks = doneTasksData?.total ?? 0;
  const inProgressTasks = inProgressData?.total ?? 0;
  const blockedTasks = blockedData?.total ?? 0;

  const filteredTasks = search
    ? tasks.filter((t) => t.title.toLowerCase().includes(search.toLowerCase()) || t.id.toLowerCase().includes(search.toLowerCase()))
    : tasks;

  const filteredReqs = search
    ? reqs.filter((r) => r.title.toLowerCase().includes(search.toLowerCase()) || r.id.toLowerCase().includes(search.toLowerCase()))
    : reqs;

  function resetFilters() {
    setStatusFilter("");
    setPriorityFilter("");
    setSearch("");
    setOffset(0);
  }

  return (
    <div className="space-y-4">
      {project && (
        <div>
          <h1 className="text-2xl font-bold">{project.name}</h1>
          <p className="text-sm text-muted-foreground">{project.repoOriginUrl}</p>
        </div>
      )}

      <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
        <StatCard label="Total Tasks" value={totalTasks} />
        <StatCard label="In Progress" value={inProgressTasks} />
        <StatCard label="Done" value={doneTasks} sub={totalTasks > 0 ? `${Math.round((doneTasks / totalTasks) * 100)}%` : ""} />
        <StatCard label="Blocked" value={blockedTasks} />
        <StatCard label="Requirements" value={totalReqs} />
      </div>

      <div className="flex gap-2 border-b">
        <button
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${tab === "tasks" ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground"}`}
          onClick={() => { setTab("tasks"); resetFilters(); }}
        >
          Tasks ({totalTasks})
        </button>
        <button
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${tab === "requirements" ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground"}`}
          onClick={() => { setTab("requirements"); resetFilters(); }}
        >
          Requirements ({totalReqs})
        </button>
        <button
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${tab === "data" ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground"}`}
          onClick={() => setTab("data")}
        >
          Data
        </button>
      </div>

      {tab !== "data" && <div className="flex gap-2 flex-wrap">
        <Input placeholder="Search..." value={search} onChange={(e) => setSearch(e.target.value)} className="w-60" />
        <select
          className="h-10 rounded-md border border-input bg-background px-3 text-sm"
          value={statusFilter}
          onChange={(e) => { setStatusFilter(e.target.value); setOffset(0); }}
        >
          <option value="">All statuses</option>
          {tab === "tasks"
            ? ["backlog", "ready", "in-progress", "blocked", "on-hold", "done", "cancelled"].map((s) => <option key={s} value={s}>{s}</option>)
            : ["draft", "refined", "planned", "in-progress", "done", "approved", "implemented", "deprecated"].map((s) => <option key={s} value={s}>{s}</option>)
          }
        </select>
        <select
          className="h-10 rounded-md border border-input bg-background px-3 text-sm"
          value={priorityFilter}
          onChange={(e) => { setPriorityFilter(e.target.value); setOffset(0); }}
        >
          <option value="">All priorities</option>
          {tab === "tasks"
            ? ["critical", "high", "medium", "low"].map((p) => <option key={p} value={p}>{p}</option>)
            : ["must", "should", "could", "wont"].map((p) => <option key={p} value={p}>{p}</option>)
          }
        </select>
      </div>}

      {tab === "data" ? (
        <ProjectDataTab projectId={projectId!} />
      ) : (tasksLoading || reqsLoading) ? (
        <div className="text-muted-foreground py-8 text-center">Loading...</div>
      ) : tab === "tasks" ? (
        <div className="border rounded-lg overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/50">
                <th className="text-left p-3 font-medium">ID</th>
                <th className="text-left p-3 font-medium">Title</th>
                <th className="text-left p-3 font-medium">Status</th>
                <th className="text-left p-3 font-medium">Priority</th>
                <th className="text-left p-3 font-medium">Type</th>
              </tr>
            </thead>
            <tbody>
              {filteredTasks.map((t) => (
                <tr key={t.id} className="border-b hover:bg-muted/30 cursor-pointer">
                  <td className="p-3 font-mono text-xs">
                    <Link to={`/projects/${projectId}/tasks/${t.id}`} className="text-primary hover:underline">{t.id}</Link>
                  </td>
                  <td className="p-3">
                    <Link to={`/projects/${projectId}/tasks/${t.id}`} className="hover:text-primary">{t.title}</Link>
                  </td>
                  <td className="p-3"><Badge variant={statusVariant(t.status)}>{t.status}</Badge></td>
                  <td className="p-3"><Badge variant={priorityVariant(t.priority)}>{t.priority}</Badge></td>
                  <td className="p-3"><Badge variant="outline">{t.type}</Badge></td>
                </tr>
              ))}
              {filteredTasks.length === 0 && (
                <tr><td colSpan={5} className="p-8 text-center text-muted-foreground">No tasks found</td></tr>
              )}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="border rounded-lg overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/50">
                <th className="text-left p-3 font-medium">ID</th>
                <th className="text-left p-3 font-medium">Title</th>
                <th className="text-left p-3 font-medium">Status</th>
                <th className="text-left p-3 font-medium">Priority</th>
                <th className="text-left p-3 font-medium">Category</th>
              </tr>
            </thead>
            <tbody>
              {filteredReqs.map((r) => (
                <tr key={r.id} className="border-b hover:bg-muted/30 cursor-pointer">
                  <td className="p-3 font-mono text-xs">
                    <Link to={`/projects/${projectId}/requirements/${r.id}`} className="text-primary hover:underline">{r.id}</Link>
                  </td>
                  <td className="p-3">
                    <Link to={`/projects/${projectId}/requirements/${r.id}`} className="hover:text-primary">{r.title}</Link>
                  </td>
                  <td className="p-3"><Badge variant={statusVariant(r.status)}>{r.status}</Badge></td>
                  <td className="p-3"><Badge variant={priorityVariant(r.priority)}>{r.priority}</Badge></td>
                  <td className="p-3">{r.category || "—"}</td>
                </tr>
              ))}
              {filteredReqs.length === 0 && (
                <tr><td colSpan={5} className="p-8 text-center text-muted-foreground">No requirements found</td></tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      {total > limit && (
        <div className="flex justify-between items-center">
          <span className="text-sm text-muted-foreground">
            Showing {offset + 1}–{Math.min(offset + limit, total)} of {total}
          </span>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - limit))}>Previous</Button>
            <Button variant="outline" size="sm" disabled={offset + limit >= total} onClick={() => setOffset(offset + limit)}>Next</Button>
          </div>
        </div>
      )}
    </div>
  );
}
