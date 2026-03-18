import { FormEvent, useCallback, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  TasksDocument,
  TaskDetailDocument,
  CreateTaskDocument,
  UpdateTaskDocument,
  UpdateTaskStatusDocument,
  DeleteTaskDocument,
  AssignAgentDocument,
  AssignHumanDocument,
  ChecklistAddDocument,
  ChecklistUpdateDocument,
  DependencyAddDocument,
  DependencyRemoveDocument,
  RunWorkflowDocument,
} from "@/lib/graphql/generated/graphql";
import { toast } from "sonner";
import { statusColor, priorityColor, PageLoading, PageError, SectionHeading, Markdown } from "./shared";

export function TasksPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const statusFilter = searchParams.get("status") ?? "";
  const searchQuery = searchParams.get("search") ?? "";
  const [page, setPage] = useState(0);
  const [pageSize] = useState(25);
  const [sortBy, setSortBy] = useState("id");
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [bulkStatus, setBulkStatus] = useState("");

  const [result] = useQuery({
    query: TasksDocument,
    variables: { status: statusFilter || undefined, search: searchQuery || undefined },
  });
  const [, updateStatus] = useMutation(UpdateTaskStatusDocument);
  const [, runWorkflow] = useMutation(RunWorkflowDocument);
  const { data, fetching, error } = result;

  const tasks = data?.tasks ?? [];
  const stats = data?.taskStats;
  const byStatus: Record<string, number> = stats?.byStatus ? JSON.parse(stats.byStatus) : {};

  const sortedTasks = useMemo(() => {
    const sorted = [...tasks].sort((a, b) => {
      const aVal = (a as Record<string, unknown>)[sortBy] ?? "";
      const bVal = (b as Record<string, unknown>)[sortBy] ?? "";
      const cmp = String(aVal).localeCompare(String(bVal));
      return sortDir === "asc" ? cmp : -cmp;
    });
    return sorted;
  }, [tasks, sortBy, sortDir]);

  const totalPages = Math.max(1, Math.ceil(sortedTasks.length / pageSize));
  const paginatedTasks = useMemo(
    () => sortedTasks.slice(page * pageSize, (page + 1) * pageSize),
    [sortedTasks, page, pageSize],
  );

  const toggleSort = (col: string) => {
    if (sortBy === col) setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    else { setSortBy(col); setSortDir("asc"); }
  };

  const sortArrow = (col: string) => (sortBy === col ? (sortDir === "asc" ? " \u2191" : " \u2193") : "");

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const toggleAll = () => {
    if (selectedIds.size === paginatedTasks.length) setSelectedIds(new Set());
    else setSelectedIds(new Set(paginatedTasks.map((t) => t.id)));
  };

  const applyBulkStatus = async () => {
    if (!bulkStatus || selectedIds.size === 0) return;
    for (const id of selectedIds) {
      const { error: err } = await updateStatus({ id, status: bulkStatus });
      if (err) { toast.error(`Failed ${id}: ${err.message}`); return; }
    }
    toast.success(`Updated ${selectedIds.size} tasks to ${bulkStatus}`);
    setSelectedIds(new Set());
    setBulkStatus("");
  };

  const dispatchBulkWorkflows = async () => {
    if (selectedIds.size === 0) return;
    for (const id of selectedIds) {
      const { error: err } = await runWorkflow({ taskId: id });
      if (err) { toast.error(`Failed ${id}: ${err.message}`); return; }
    }
    toast.success(`Dispatched workflows for ${selectedIds.size} tasks`);
    setSelectedIds(new Set());
  };

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Tasks</h1>
          <Link to="/tasks/new"><Button size="sm">Create Task</Button></Link>
        </div>
        <span className="text-sm text-muted-foreground">{tasks.length} tasks</span>
      </div>

      <div className="grid grid-cols-3 md:grid-cols-6 gap-2">
        {["backlog", "ready", "in-progress", "blocked", "done", "cancelled"].map((s) => (
          <button
            key={s}
            type="button"
            onClick={() => {
              const next = new URLSearchParams(searchParams);
              if (statusFilter === s) next.delete("status");
              else next.set("status", s);
              setSearchParams(next);
              setPage(0);
            }}
            className={`rounded-md border px-2 py-1 text-xs text-center transition-colors ${
              statusFilter === s ? "bg-accent text-accent-foreground" : "hover:bg-accent/50"
            }`}
          >
            {s} ({byStatus[s] ?? 0})
          </button>
        ))}
      </div>

      <Input
        placeholder="Search tasks..."
        value={searchQuery}
        onChange={(e) => {
          const next = new URLSearchParams(searchParams);
          if (e.target.value) next.set("search", e.target.value);
          else next.delete("search");
          setSearchParams(next);
          setPage(0);
        }}
        className="max-w-sm"
      />

      {selectedIds.size > 0 && (
        <div className="border border-primary/20 bg-primary/5 rounded-md px-3 py-2 flex items-center gap-3">
          <span className="text-xs text-muted-foreground">{selectedIds.size} selected</span>
          <select
            value={bulkStatus}
            onChange={(e) => setBulkStatus(e.target.value)}
            className="h-6 rounded-md border border-input bg-background px-2 text-xs"
          >
            <option value="">Set status...</option>
            {["backlog", "ready", "in-progress", "blocked", "done", "cancelled"].map((s) => (
              <option key={s} value={s}>{s}</option>
            ))}
          </select>
          <Button size="sm" variant="outline" className="h-6 text-xs" onClick={applyBulkStatus} disabled={!bulkStatus}>Apply</Button>
          <Button size="sm" variant="outline" className="h-6 text-xs" onClick={dispatchBulkWorkflows}>Run Workflows</Button>
          <Button size="sm" variant="ghost" className="h-6 text-xs" onClick={() => setSelectedIds(new Set())}>Clear</Button>
        </div>
      )}

      {tasks.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No tasks match filters.</p>
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">
                  <input type="checkbox" className="h-4 w-4" checked={selectedIds.size === paginatedTasks.length && paginatedTasks.length > 0} onChange={toggleAll} />
                </TableHead>
                <TableHead className="w-28 cursor-pointer select-none" onClick={() => toggleSort("id")}>ID{sortArrow("id")}</TableHead>
                <TableHead className="cursor-pointer select-none" onClick={() => toggleSort("title")}>Title{sortArrow("title")}</TableHead>
                <TableHead className="cursor-pointer select-none" onClick={() => toggleSort("statusRaw")}>Status{sortArrow("statusRaw")}</TableHead>
                <TableHead className="cursor-pointer select-none" onClick={() => toggleSort("priorityRaw")}>Priority{sortArrow("priorityRaw")}</TableHead>
                <TableHead className="cursor-pointer select-none" onClick={() => toggleSort("taskTypeRaw")}>Type{sortArrow("taskTypeRaw")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {paginatedTasks.map((t) => (
                <TableRow key={t.id}>
                  <TableCell>
                    <input type="checkbox" className="h-4 w-4" checked={selectedIds.has(t.id)} onChange={() => toggleSelect(t.id)} />
                  </TableCell>
                  <TableCell>
                    <Link to={`/tasks/${t.id}`} className="font-mono text-xs underline">{t.id}</Link>
                  </TableCell>
                  <TableCell className="font-medium">{t.title}</TableCell>
                  <TableCell><Badge variant={statusColor(t.statusRaw ?? "")}>{t.statusRaw}</Badge></TableCell>
                  <TableCell><Badge variant={priorityColor(t.priorityRaw ?? "")}>{t.priorityRaw}</Badge></TableCell>
                  <TableCell className="text-xs text-muted-foreground">{t.taskTypeRaw}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Card>
      )}

      {sortedTasks.length > pageSize && (
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Page {page + 1} of {totalPages}</span>
          <div className="flex gap-1">
            <Button size="sm" variant="outline" className="h-6" disabled={page === 0} onClick={() => setPage((p) => p - 1)}>Prev</Button>
            <Button size="sm" variant="outline" className="h-6" disabled={page >= totalPages - 1} onClick={() => setPage((p) => p + 1)}>Next</Button>
          </div>
        </div>
      )}
    </div>
  );
}

export function TaskCreatePage() {
  const navigate = useNavigate();
  const [, createTask] = useMutation(CreateTaskDocument);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("medium");
  const [taskType, setTaskType] = useState("feature");
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim()) { setErrorMsg("Title is required."); return; }
    setSubmitting(true);
    setErrorMsg(null);
    const result = await createTask({
      title: title.trim(),
      description: description.trim() || null,
      priority,
      taskType,
    });
    setSubmitting(false);
    if (result.error) {
      setErrorMsg(result.error.message);
    } else {
      navigate(`/tasks/${result.data?.createTask?.id}`, { replace: true });
    }
  };

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-semibold tracking-tight">Create Task</h1>
      <Card>
        <CardContent className="pt-6">
          <form onSubmit={onSubmit} className="space-y-4">
            <div>
              <label className="text-sm font-medium">Title</label>
              <Input required value={title} onChange={(e) => setTitle(e.target.value)} className="mt-1" />
            </div>
            <div>
              <label className="text-sm font-medium">Description</label>
              <Textarea rows={4} value={description} onChange={(e) => setDescription(e.target.value)} className="mt-1" />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="text-sm font-medium">Priority</label>
                <select value={priority} onChange={(e) => setPriority(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["critical", "high", "medium", "low"].map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-sm font-medium">Type</label>
                <select value={taskType} onChange={(e) => setTaskType(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["feature", "bug", "chore", "refactor", "test", "docs"].map((t) => <option key={t} value={t}>{t}</option>)}
                </select>
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Button type="submit" disabled={submitting}>{submitting ? "Creating..." : "Create Task"}</Button>
              <Link to="/tasks"><Button variant="outline" type="button">Cancel</Button></Link>
            </div>
          </form>
        </CardContent>
      </Card>
      {errorMsg && <Alert variant="destructive"><AlertDescription>{errorMsg}</AlertDescription></Alert>}
    </div>
  );
}

export function TaskDetailPage() {
  const navigate = useNavigate();
  const { taskId } = useParams();
  const [result, reexecute] = useQuery({ query: TaskDetailDocument, variables: { id: taskId! } });
  const [, updateStatus] = useMutation(UpdateTaskStatusDocument);
  const [, updateTask] = useMutation(UpdateTaskDocument);
  const [, deleteTask] = useMutation(DeleteTaskDocument);
  const [, assignAgent] = useMutation(AssignAgentDocument);
  const [, assignHuman] = useMutation(AssignHumanDocument);
  const [, checklistAdd] = useMutation(ChecklistAddDocument);
  const [, checklistUpdate] = useMutation(ChecklistUpdateDocument);
  const [, depAdd] = useMutation(DependencyAddDocument);
  const [, depRemove] = useMutation(DependencyRemoveDocument);

  const [targetStatus, setTargetStatus] = useState("");
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState("");
  const [editDesc, setEditDesc] = useState("");
  const [editPriority, setEditPriority] = useState("");
  const [editType, setEditType] = useState("");
  const [editRisk, setEditRisk] = useState("");
  const [editScope, setEditScope] = useState("");
  const [editComplexity, setEditComplexity] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [newChecklistItem, setNewChecklistItem] = useState("");
  const [newDepId, setNewDepId] = useState("");
  const [assignMode, setAssignMode] = useState<"" | "agent" | "human">("");
  const [assignRole, setAssignRole] = useState("default");
  const [assignModel, setAssignModel] = useState("");
  const [assignName, setAssignName] = useState("");

  const { data, fetching, error } = result;

  const reload = useCallback(() => reexecute({ requestPolicy: "network-only" }), [reexecute]);

  const showFeedback = (kind: "ok" | "error", message: string) => setFeedback({ kind, message });

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const task = data?.task;
  if (!task) return <PageError message={`Task ${taskId} not found.`} />;

  const startEdit = () => {
    setEditTitle(task.title);
    setEditDesc(task.description ?? "");
    setEditPriority(task.priorityRaw ?? "");
    setEditType(task.taskTypeRaw ?? "");
    setEditRisk(task.risk ?? "");
    setEditScope(task.scope ?? "");
    setEditComplexity(task.complexity ?? "");
    setEditing(true);
  };

  const saveEdit = async () => {
    const { error: err } = await updateTask({
      id: taskId!,
      title: editTitle.trim() || null,
      description: editDesc.trim() || null,
      taskType: editType || null,
      priority: editPriority || null,
      risk: editRisk || null,
      scope: editScope || null,
      complexity: editComplexity || null,
    });
    if (err) showFeedback("error", err.message);
    else { showFeedback("ok", "Task updated."); setEditing(false); reload(); }
  };

  const applyStatus = async () => {
    if (!targetStatus) return;
    const { error: err } = await updateStatus({ id: taskId!, status: targetStatus });
    if (err) showFeedback("error", err.message);
    else { showFeedback("ok", `Status updated to ${targetStatus}.`); reload(); }
  };

  const onDelete = async () => {
    const { error: err } = await deleteTask({ id: taskId! });
    if (err) showFeedback("error", err.message);
    else navigate("/tasks", { replace: true });
  };

  const onChecklistToggle = async (itemId: string, completed: boolean) => {
    const { error: err } = await checklistUpdate({ id: taskId!, itemId, completed: !completed });
    if (err) showFeedback("error", err.message);
    else reload();
  };

  const onChecklistAdd = async () => {
    if (!newChecklistItem.trim()) return;
    const { error: err } = await checklistAdd({ id: taskId!, description: newChecklistItem.trim() });
    if (err) showFeedback("error", err.message);
    else { setNewChecklistItem(""); reload(); }
  };

  const onDepAdd = async () => {
    if (!newDepId.trim()) return;
    const { error: err } = await depAdd({ id: taskId!, dependsOn: newDepId.trim() });
    if (err) showFeedback("error", err.message);
    else { setNewDepId(""); reload(); }
  };

  const onDepRemove = async (depTaskId: string) => {
    const { error: err } = await depRemove({ id: taskId!, dependsOn: depTaskId });
    if (err) showFeedback("error", err.message);
    else reload();
  };

  const onAssign = async () => {
    if (assignMode === "agent") {
      const { error: err } = await assignAgent({ id: taskId!, role: assignRole || null, model: assignModel || null });
      if (err) showFeedback("error", err.message);
      else { showFeedback("ok", "Assigned to agent."); setAssignMode(""); reload(); }
    } else if (assignMode === "human") {
      if (!assignName.trim()) return;
      const { error: err } = await assignHuman({ id: taskId!, name: assignName.trim() });
      if (err) showFeedback("error", err.message);
      else { showFeedback("ok", `Assigned to ${assignName}.`); setAssignMode(""); reload(); }
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between">
        <div>
          <p className="text-[11px] text-muted-foreground/50 font-mono tracking-wide">{task.id}</p>
          <h1 className="text-2xl font-semibold tracking-tight">{task.title}</h1>
          <div className="flex gap-2 mt-2">
            <Badge variant={statusColor(task.statusRaw ?? "")}>{task.statusRaw}</Badge>
            <Badge variant={priorityColor(task.priorityRaw ?? "")}>{task.priorityRaw}</Badge>
            <Badge variant="outline">{task.taskTypeRaw}</Badge>
          </div>
        </div>
        <div className="flex gap-2">
          <Link to={`/tasks/${taskId}/output`}><Button size="sm" variant="outline">Output</Button></Link>
          <Button size="sm" variant="outline" onClick={startEdit}>Edit</Button>
          {confirmDelete ? (
            <>
              <Button size="sm" variant="destructive" onClick={onDelete}>Confirm Delete</Button>
              <Button size="sm" variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
            </>
          ) : (
            <Button size="sm" variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={() => setConfirmDelete(true)}>Delete</Button>
          )}
        </div>
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {editing && (
        <Card className="border-primary/20 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Edit Task</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3 space-y-3">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</label>
              <Input value={editTitle} onChange={(e) => setEditTitle(e.target.value)} className="mt-1" />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
              <Textarea rows={3} value={editDesc} onChange={(e) => setEditDesc(e.target.value)} className="mt-1" />
            </div>
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Priority</label>
                <select value={editPriority} onChange={(e) => setEditPriority(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["critical", "high", "medium", "low"].map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</label>
                <select value={editType} onChange={(e) => setEditType(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["feature", "bug", "chore", "refactor", "test", "docs"].map((t) => <option key={t} value={t}>{t}</option>)}
                </select>
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Risk</label>
                <select value={editRisk} onChange={(e) => setEditRisk(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["low", "medium", "high"].map((r) => <option key={r} value={r}>{r}</option>)}
                </select>
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Scope</label>
                <select value={editScope} onChange={(e) => setEditScope(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["small", "medium", "large"].map((s) => <option key={s} value={s}>{s}</option>)}
                </select>
              </div>
            </div>
            <div className="flex gap-2">
              <Button size="sm" onClick={saveEdit}>Save</Button>
              <Button size="sm" variant="outline" onClick={() => setEditing(false)}>Cancel</Button>
            </div>
          </CardContent>
        </Card>
      )}

      {task.description && !editing && (
        <Card className="border-border/40 bg-card/60">
          <CardContent className="pt-4 pb-3 px-4"><Markdown content={task.description} /></CardContent>
        </Card>
      )}

      <div className="grid md:grid-cols-[1fr_auto] gap-4 items-start">
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Status Transition</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            <div className="flex items-center gap-2">
              <select
                value={targetStatus}
                onChange={(e) => setTargetStatus(e.target.value)}
                className="h-9 flex-1 rounded-md border border-input bg-background px-3 text-sm"
              >
                <option value="">Select status...</option>
                {["backlog", "ready", "in-progress", "blocked", "on-hold", "done", "cancelled"].map((s) => (
                  <option key={s} value={s}>{s}</option>
                ))}
              </select>
              <Button size="sm" onClick={applyStatus} disabled={!targetStatus || targetStatus === task.statusRaw}>
                Apply
              </Button>
            </div>
          </CardContent>
        </Card>

        <Card className="border-border/40 bg-card/60 min-w-[180px]">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Details</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3 space-y-1.5">
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground/60">Risk</span>
              <Badge variant="outline" className="text-[10px]">{task.risk}</Badge>
            </div>
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground/60">Scope</span>
              <Badge variant="outline" className="text-[10px]">{task.scope}</Badge>
            </div>
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground/60">Complexity</span>
              <Badge variant="outline" className="text-[10px]">{task.complexity}</Badge>
            </div>
            {(task.tags ?? []).length > 0 && (
              <div className="flex gap-1 flex-wrap pt-1">
                {task.tags!.map((t) => <Badge key={t} variant="outline" className="text-[10px]">{t}</Badge>)}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <SectionHeading>Work</SectionHeading>

      <div className="grid md:grid-cols-2 gap-4">
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <div className="flex items-center justify-between">
              <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Assignment</CardTitle>
              {assignMode === "" && (
                <div className="flex gap-1">
                  <Button size="sm" variant="outline" className="h-5 text-[10px] px-1.5" onClick={() => setAssignMode("agent")}>Agent</Button>
                  <Button size="sm" variant="outline" className="h-5 text-[10px] px-1.5" onClick={() => setAssignMode("human")}>Human</Button>
                </div>
              )}
            </div>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            {assignMode === "agent" && (
              <div className="flex items-end gap-2">
                <div>
                  <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Role</label>
                  <Input value={assignRole} onChange={(e) => setAssignRole(e.target.value)} className="mt-1 h-8 w-32 text-xs" />
                </div>
                <div>
                  <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Model</label>
                  <Input value={assignModel} onChange={(e) => setAssignModel(e.target.value)} placeholder="e.g. claude-sonnet-4-6" className="mt-1 h-8 w-48 text-xs" />
                </div>
                <Button size="sm" className="h-8" onClick={onAssign}>Assign</Button>
                <Button size="sm" variant="outline" className="h-8" onClick={() => setAssignMode("")}>Cancel</Button>
              </div>
            )}
            {assignMode === "human" && (
              <div className="flex items-end gap-2">
                <div>
                  <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
                  <Input value={assignName} onChange={(e) => setAssignName(e.target.value)} className="mt-1 h-8 w-48 text-xs" />
                </div>
                <Button size="sm" className="h-8" onClick={onAssign}>Assign</Button>
                <Button size="sm" variant="outline" className="h-8" onClick={() => setAssignMode("")}>Cancel</Button>
              </div>
            )}
            {assignMode === "" && (
              <p className="text-xs text-muted-foreground/50">Unassigned</p>
            )}
          </CardContent>
        </Card>

        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Checklist</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3 space-y-2">
            {(task.checklist ?? []).length > 0 && (
              <ul className="space-y-1">
                {task.checklist!.map((item) => (
                  <li key={item.id} className="flex items-center gap-2 text-sm">
                    <button
                      type="button"
                      onClick={() => onChecklistToggle(item.id, item.completed)}
                      className="shrink-0 text-lg leading-none hover:opacity-70"
                      aria-label={item.completed ? `Uncheck: ${item.description}` : `Check: ${item.description}`}
                    >
                      {item.completed ? <span className="text-[var(--ao-success)]">&#x2611;</span> : <span className="text-muted-foreground">&#x2610;</span>}
                    </button>
                    <span className={item.completed ? "line-through text-muted-foreground" : ""}>{item.description}</span>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex gap-2">
              <Input
                value={newChecklistItem}
                onChange={(e) => setNewChecklistItem(e.target.value)}
                placeholder="Add checklist item..."
                className="h-8 text-sm"
                onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), onChecklistAdd())}
              />
              <Button size="sm" variant="outline" className="h-8" onClick={onChecklistAdd}>Add</Button>
            </div>
          </CardContent>
        </Card>
      </div>

      <SectionHeading>Relationships</SectionHeading>

      <div className="grid md:grid-cols-2 gap-4">
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Dependencies</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3 space-y-2">
            {(task.dependencies ?? []).length > 0 && (
              <ul className="space-y-1">
                {task.dependencies!.map((dep) => (
                  <li key={dep.taskId} className="flex items-center gap-2 text-sm">
                    <Link to={`/tasks/${dep.taskId}`} className="font-mono text-xs text-primary/80 hover:text-primary transition-colors">{dep.taskId}</Link>
                    <span className="text-muted-foreground/50 text-xs">{dep.type}</span>
                    <Button size="sm" variant="ghost" className="h-5 px-1 text-[10px] text-destructive/60 hover:text-destructive" onClick={() => onDepRemove(dep.taskId)}>remove</Button>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex gap-2">
              <Input
                value={newDepId}
                onChange={(e) => setNewDepId(e.target.value)}
                placeholder="TASK-XXX"
                className="h-8 w-40 text-sm font-mono"
                onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), onDepAdd())}
              />
              <Button size="sm" variant="outline" className="h-8" onClick={onDepAdd}>Add</Button>
            </div>
          </CardContent>
        </Card>

        {(task.linkedRequirementIds ?? []).length > 0 && (
          <Card className="border-border/40 bg-card/60">
            <CardHeader className="pb-2 pt-3 px-4">
              <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Linked Requirements</CardTitle>
            </CardHeader>
            <CardContent className="px-4 pb-3">
              <div className="flex gap-2 flex-wrap">
                {task.linkedRequirementIds!.map((id) => (
                  <Link key={id} to={`/planning/requirements/${id}`}>
                    <Badge variant="outline" className="font-mono text-[10px] hover:bg-accent/50 transition-colors cursor-pointer">{id}</Badge>
                  </Link>
                ))}
              </div>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  );
}
