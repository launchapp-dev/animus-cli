import { FormEvent, ReactNode, useCallback, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQuery, useMutation } from "urql";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Separator } from "@/components/ui/separator";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { useDaemonEvents } from "../lib/events/use-daemon-events";

const DASHBOARD_QUERY = `
  query Dashboard {
    taskStats { total byStatus byPriority }
    daemonHealth { healthy status runnerConnected activeDaemons: activeAgents daemonPid }
    agentRuns { runId taskId taskTitle workflowId phaseId status }
    systemInfo { platform version daemonStatus projectRoot }
  }
`;

const TASKS_QUERY = `
  query Tasks($status: String, $search: String) {
    tasks(status: $status, search: $search) {
      id title status statusRaw priority priorityRaw taskType taskTypeRaw
      tags linkedRequirementIds
    }
    taskStats { total byStatus byPriority }
  }
`;

const TASKS_PRIORITIZED_QUERY = `
  query TasksPrioritized {
    tasksPrioritized {
      id title status statusRaw priority priorityRaw taskType taskTypeRaw tags
    }
  }
`;

const TASK_DETAIL_QUERY = `
  query TaskDetail($id: ID!) {
    task(id: $id) {
      id title description status statusRaw priority priorityRaw
      taskType taskTypeRaw risk scope complexity tags
      linkedRequirementIds
      checklist { id description completed }
      dependencies { taskId type }
    }
  }
`;

const WORKFLOWS_QUERY = `
  query Workflows($status: String) {
    workflows(status: $status) {
      id taskId workflowRef status statusRaw currentPhase totalReworks
      phases { phaseId status startedAt completedAt attempt errorMessage }
    }
  }
`;

const WORKFLOW_DETAIL_QUERY = `
  query WorkflowDetail($id: ID!) {
    workflow(id: $id) {
      id taskId workflowRef status statusRaw currentPhase totalReworks
      phases { phaseId status startedAt completedAt attempt errorMessage }
      decisions { timestamp phaseId source decision targetPhase reason confidence risk }
    }
    workflowCheckpoints(workflowId: $id) { id phase timestamp data }
  }
`;

const QUEUE_QUERY = `
  query Queue {
    queue { taskId title priority status waitTime position }
    queueStats { depth readyCount heldCount avgWait throughput }
  }
`;

const DAEMON_QUERY = `
  query Daemon {
    daemonStatus { healthy status statusRaw runnerConnected activeAgents maxAgents projectRoot }
    daemonHealth { healthy status runnerConnected runnerPid activeAgents daemonPid }
    agentRuns { runId taskId taskTitle workflowId phaseId status }
    daemonLogs(limit: 50) { timestamp level message }
  }
`;

const PROJECTS_QUERY = `
  query Projects {
    projects { id name path description archived }
    projectsActive { id name path }
  }
`;

const PROJECT_DETAIL_QUERY = `
  query ProjectDetail($id: ID!) {
    project(id: $id) { id name path description type techStack archived }
  }
`;

const CREATE_PROJECT = `mutation CreateProject($name: String!, $path: String!, $description: String, $projectType: String) {
  createProject(name: $name, path: $path, description: $description, projectType: $projectType) { id name }
}`;
const UPDATE_PROJECT = `mutation UpdateProject($id: ID!, $name: String, $description: String, $projectType: String) {
  updateProject(id: $id, name: $name, description: $description, projectType: $projectType) { id name }
}`;
const DELETE_PROJECT = `mutation DeleteProject($id: ID!) { deleteProject(id: $id) }`;
const LOAD_PROJECT = `mutation LoadProject($id: ID!) { loadProject(id: $id) { id name } }`;
const ARCHIVE_PROJECT = `mutation ArchiveProject($id: ID!) { archiveProject(id: $id) { id name archived } }`;

const REQUIREMENT_DETAIL_QUERY = `
  query RequirementDetail($id: ID!) {
    requirement(id: $id) {
      id title description priority priorityRaw status statusRaw
      requirementType tags linkedTaskIds
    }
  }
`;

const VISION_QUERY = `
  query Vision {
    vision { title summary goals targetAudience successCriteria constraints raw }
  }
`;

const UPDATE_TASK_STATUS = `
  mutation UpdateTaskStatus($id: ID!, $status: String!) {
    updateTaskStatus(id: $id, status: $status) { id status statusRaw }
  }
`;

const RUN_WORKFLOW = `
  mutation RunWorkflow($taskId: String!, $workflowRef: String) {
    runWorkflow(taskId: $taskId, workflowRef: $workflowRef) { id taskId status statusRaw }
  }
`;

const PAUSE_WORKFLOW = `mutation PauseWorkflow($id: ID!) { pauseWorkflow(id: $id) { id status } }`;
const RESUME_WORKFLOW = `mutation ResumeWorkflow($id: ID!) { resumeWorkflow(id: $id) { id status } }`;
const CANCEL_WORKFLOW = `mutation CancelWorkflow($id: ID!) { cancelWorkflow(id: $id) { id status } }`;

const DAEMON_START = `mutation { daemonStart }`;
const DAEMON_STOP = `mutation { daemonStop }`;
const DAEMON_PAUSE = `mutation { daemonPause }`;
const DAEMON_RESUME = `mutation { daemonResume }`;
const DAEMON_CLEAR_LOGS = `mutation { daemonClearLogs }`;

const QUEUE_HOLD = `mutation QueueHold($taskId: String!, $reason: String) { queueHold(taskId: $taskId, reason: $reason) }`;
const QUEUE_RELEASE = `mutation QueueRelease($taskId: String!) { queueRelease(taskId: $taskId) }`;
const QUEUE_REORDER = `mutation QueueReorder($taskIds: [String!]!) { queueReorder(taskIds: $taskIds) }`;

const REVIEW_HANDOFF = `
  mutation ReviewHandoff($targetRole: String!, $question: String!, $context: String) {
    reviewHandoff(targetRole: $targetRole, question: $question, context: $context)
  }
`;

const CREATE_TASK = `
  mutation CreateTask($title: String!, $description: String, $taskType: String, $priority: String) {
    createTask(title: $title, description: $description, taskType: $taskType, priority: $priority) {
      id title status statusRaw
    }
  }
`;

const UPDATE_TASK = `
  mutation UpdateTask($id: ID!, $title: String, $description: String, $taskType: String, $priority: String, $risk: String, $scope: String, $complexity: String) {
    updateTask(id: $id, title: $title, description: $description, taskType: $taskType, priority: $priority, risk: $risk, scope: $scope, complexity: $complexity) {
      id title status statusRaw
    }
  }
`;

const DELETE_TASK = `mutation DeleteTask($id: ID!) { deleteTask(id: $id) }`;

const ASSIGN_AGENT = `
  mutation AssignAgent($id: ID!, $role: String, $model: String) {
    assignAgent(id: $id, role: $role, model: $model) { id }
  }
`;

const ASSIGN_HUMAN = `
  mutation AssignHuman($id: ID!, $name: String!) {
    assignHuman(id: $id, name: $name) { id }
  }
`;

const CHECKLIST_ADD = `
  mutation ChecklistAdd($id: ID!, $description: String!) {
    checklistAdd(id: $id, description: $description) {
      id checklist { id description completed }
    }
  }
`;

const CHECKLIST_UPDATE = `
  mutation ChecklistUpdate($id: ID!, $itemId: String!, $completed: Boolean, $description: String) {
    checklistUpdate(id: $id, itemId: $itemId, completed: $completed, description: $description) {
      id checklist { id description completed }
    }
  }
`;

const DEPENDENCY_ADD = `
  mutation DependencyAdd($id: ID!, $dependsOn: String!, $dependencyType: String) {
    dependencyAdd(id: $id, dependsOn: $dependsOn, dependencyType: $dependencyType) {
      id dependencies { taskId type }
    }
  }
`;

const DEPENDENCY_REMOVE = `
  mutation DependencyRemove($id: ID!, $dependsOn: String!) {
    dependencyRemove(id: $id, dependsOn: $dependsOn) {
      id dependencies { taskId type }
    }
  }
`;

function statusColor(status: string): "default" | "secondary" | "destructive" | "outline" {
  const s = status.toLowerCase().replace(/[_\s]/g, "-");
  if (["done", "completed", "approved", "implemented"].includes(s)) return "default";
  if (["in-progress", "running", "inprogress"].includes(s)) return "secondary";
  if (["blocked", "failed", "cancelled", "crashed"].includes(s)) return "destructive";
  return "outline";
}

function priorityColor(p: string): "default" | "secondary" | "destructive" | "outline" {
  const v = (p || "").toLowerCase();
  if (v === "critical") return "destructive";
  if (v === "high") return "secondary";
  return "outline";
}

function PageLoading() {
  return (
    <div className="space-y-4">
      <Skeleton className="h-8 w-48" />
      <Skeleton className="h-32 w-full" />
      <Skeleton className="h-32 w-full" />
    </div>
  );
}

function PageError({ message }: { message: string }) {
  return (
    <Alert variant="destructive">
      <AlertTitle>Error</AlertTitle>
      <AlertDescription>{message}</AlertDescription>
    </Alert>
  );
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

export function DashboardPage() {
  const [result] = useQuery({ query: DASHBOARD_QUERY });
  const { data, fetching, error } = result;

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const stats = data?.taskStats;
  const health = data?.daemonHealth;
  const agents = data?.agentRuns ?? [];
  const sys = data?.systemInfo;

  const byStatus: Record<string, number> = stats?.byStatus ? JSON.parse(stats.byStatus) : {};

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-semibold tracking-tight">Dashboard</h1>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <StatCard label="Total Tasks" value={stats?.total ?? 0} />
        <StatCard label="In Progress" value={byStatus["in-progress"] ?? 0} />
        <StatCard label="Ready" value={byStatus["ready"] ?? 0} />
        <StatCard label="Blocked" value={byStatus["blocked"] ?? 0} />
      </div>

      <div className="grid md:grid-cols-2 gap-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium">Daemon</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 text-sm">
            <div className="flex items-center gap-2">
              <Badge variant={health?.healthy ? "default" : "destructive"}>
                {health?.status ?? "unknown"}
              </Badge>
              {health?.runnerConnected && <span className="text-muted-foreground">runner connected</span>}
            </div>
            <p className="text-muted-foreground">Active agents: {health?.activeDaemons ?? 0}</p>
            {health?.daemonPid && <p className="text-muted-foreground">PID: {health.daemonPid}</p>}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium">System</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 text-sm text-muted-foreground">
            {sys?.version && <p>Version: {sys.version}</p>}
            {sys?.platform && <p>Platform: {sys.platform}</p>}
            {sys?.projectRoot && <p className="truncate">Root: {sys.projectRoot}</p>}
          </CardContent>
        </Card>
      </div>

      {agents.length > 0 && (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium">Active Agents</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Run</TableHead>
                  <TableHead>Task</TableHead>
                  <TableHead>Phase</TableHead>
                  <TableHead>Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {agents.map((a: any) => (
                  <TableRow key={a.runId}>
                    <TableCell className="font-mono text-xs">{a.runId}</TableCell>
                    <TableCell>
                      {a.taskId ? <Link to={`/tasks/${a.taskId}`} className="underline">{a.taskTitle ?? a.taskId}</Link> : "-"}
                    </TableCell>
                    <TableCell className="font-mono text-xs">{a.phaseId ?? "-"}</TableCell>
                    <TableCell><Badge variant={statusColor(a.status)}>{a.status}</Badge></TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: number | string }) {
  return (
    <Card>
      <CardContent className="pt-4">
        <p className="text-xs text-muted-foreground">{label}</p>
        <p className="text-2xl font-bold">{value}</p>
      </CardContent>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

export function TasksPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const statusFilter = searchParams.get("status") ?? "";
  const searchQuery = searchParams.get("search") ?? "";

  const [result] = useQuery({
    query: TASKS_QUERY,
    variables: { status: statusFilter || undefined, search: searchQuery || undefined },
  });
  const { data, fetching, error } = result;

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const tasks = data?.tasks ?? [];
  const stats = data?.taskStats;
  const byStatus: Record<string, number> = stats?.byStatus ? JSON.parse(stats.byStatus) : {};

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
        }}
        className="max-w-sm"
      />

      {tasks.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No tasks match filters.</p>
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-28">ID</TableHead>
                <TableHead>Title</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Priority</TableHead>
                <TableHead>Type</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {tasks.map((t: any) => (
                <TableRow key={t.id}>
                  <TableCell>
                    <Link to={`/tasks/${t.id}`} className="font-mono text-xs underline">{t.id}</Link>
                  </TableCell>
                  <TableCell className="font-medium">{t.title}</TableCell>
                  <TableCell><Badge variant={statusColor(t.statusRaw)}>{t.statusRaw}</Badge></TableCell>
                  <TableCell><Badge variant={priorityColor(t.priorityRaw)}>{t.priorityRaw}</Badge></TableCell>
                  <TableCell className="text-xs text-muted-foreground">{t.taskTypeRaw}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Card>
      )}
    </div>
  );
}

export function TaskCreatePage() {
  const navigate = useNavigate();
  const [, createTask] = useMutation(CREATE_TASK);
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
      navigate(`/tasks/${result.data.createTask.id}`, { replace: true });
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
  const [result, reexecute] = useQuery({ query: TASK_DETAIL_QUERY, variables: { id: taskId } });
  const [, updateStatus] = useMutation(UPDATE_TASK_STATUS);
  const [, updateTask] = useMutation(UPDATE_TASK);
  const [, deleteTask] = useMutation(DELETE_TASK);
  const [, assignAgent] = useMutation(ASSIGN_AGENT);
  const [, assignHuman] = useMutation(ASSIGN_HUMAN);
  const [, checklistAdd] = useMutation(CHECKLIST_ADD);
  const [, checklistUpdate] = useMutation(CHECKLIST_UPDATE);
  const [, depAdd] = useMutation(DEPENDENCY_ADD);
  const [, depRemove] = useMutation(DEPENDENCY_REMOVE);

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
    setEditPriority(task.priorityRaw);
    setEditType(task.taskTypeRaw);
    setEditRisk(task.risk ?? "");
    setEditScope(task.scope ?? "");
    setEditComplexity(task.complexity ?? "");
    setEditing(true);
  };

  const saveEdit = async () => {
    const { error: err } = await updateTask({
      id: taskId,
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
    const { error: err } = await updateStatus({ id: taskId, status: targetStatus });
    if (err) showFeedback("error", err.message);
    else { showFeedback("ok", `Status updated to ${targetStatus}.`); reload(); }
  };

  const onDelete = async () => {
    const { error: err } = await deleteTask({ id: taskId });
    if (err) showFeedback("error", err.message);
    else navigate("/tasks", { replace: true });
  };

  const onChecklistToggle = async (itemId: string, completed: boolean) => {
    const { error: err } = await checklistUpdate({ id: taskId, itemId, completed: !completed });
    if (err) showFeedback("error", err.message);
    else reload();
  };

  const onChecklistAdd = async () => {
    if (!newChecklistItem.trim()) return;
    const { error: err } = await checklistAdd({ id: taskId, description: newChecklistItem.trim() });
    if (err) showFeedback("error", err.message);
    else { setNewChecklistItem(""); reload(); }
  };

  const onDepAdd = async () => {
    if (!newDepId.trim()) return;
    const { error: err } = await depAdd({ id: taskId, dependsOn: newDepId.trim() });
    if (err) showFeedback("error", err.message);
    else { setNewDepId(""); reload(); }
  };

  const onDepRemove = async (depTaskId: string) => {
    const { error: err } = await depRemove({ id: taskId, dependsOn: depTaskId });
    if (err) showFeedback("error", err.message);
    else reload();
  };

  const onAssign = async () => {
    if (assignMode === "agent") {
      const { error: err } = await assignAgent({ id: taskId, role: assignRole || null, model: assignModel || null });
      if (err) showFeedback("error", err.message);
      else { showFeedback("ok", "Assigned to agent."); setAssignMode(""); reload(); }
    } else if (assignMode === "human") {
      if (!assignName.trim()) return;
      const { error: err } = await assignHuman({ id: taskId, name: assignName.trim() });
      if (err) showFeedback("error", err.message);
      else { showFeedback("ok", `Assigned to ${assignName}.`); setAssignMode(""); reload(); }
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between">
        <div>
          <p className="text-sm text-muted-foreground font-mono">{task.id}</p>
          <h1 className="text-2xl font-semibold tracking-tight">{task.title}</h1>
          <div className="flex gap-2 mt-2">
            <Badge variant={statusColor(task.statusRaw)}>{task.statusRaw}</Badge>
            <Badge variant={priorityColor(task.priorityRaw)}>{task.priorityRaw}</Badge>
            <Badge variant="outline">{task.taskTypeRaw}</Badge>
          </div>
        </div>
        <div className="flex gap-2">
          <Button size="sm" variant="outline" onClick={startEdit}>Edit</Button>
          {confirmDelete ? (
            <>
              <Button size="sm" variant="destructive" onClick={onDelete}>Confirm Delete</Button>
              <Button size="sm" variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
            </>
          ) : (
            <Button size="sm" variant="destructive" onClick={() => setConfirmDelete(true)}>Delete</Button>
          )}
        </div>
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {editing && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Edit Task</CardTitle></CardHeader>
          <CardContent className="space-y-3">
            <div>
              <label className="text-sm font-medium">Title</label>
              <Input value={editTitle} onChange={(e) => setEditTitle(e.target.value)} className="mt-1" />
            </div>
            <div>
              <label className="text-sm font-medium">Description</label>
              <Textarea rows={3} value={editDesc} onChange={(e) => setEditDesc(e.target.value)} className="mt-1" />
            </div>
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <div>
                <label className="text-xs font-medium">Priority</label>
                <select value={editPriority} onChange={(e) => setEditPriority(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["critical", "high", "medium", "low"].map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-xs font-medium">Type</label>
                <select value={editType} onChange={(e) => setEditType(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["feature", "bug", "chore", "refactor", "test", "docs"].map((t) => <option key={t} value={t}>{t}</option>)}
                </select>
              </div>
              <div>
                <label className="text-xs font-medium">Risk</label>
                <select value={editRisk} onChange={(e) => setEditRisk(e.target.value)} className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                  {["low", "medium", "high"].map((r) => <option key={r} value={r}>{r}</option>)}
                </select>
              </div>
              <div>
                <label className="text-xs font-medium">Scope</label>
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
        <Card>
          <CardContent className="pt-4 text-sm whitespace-pre-wrap">{task.description}</CardContent>
        </Card>
      )}

      <div className="grid md:grid-cols-2 gap-4">
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Status Transition</CardTitle></CardHeader>
          <CardContent className="space-y-2">
            <select
              value={targetStatus}
              onChange={(e) => setTargetStatus(e.target.value)}
              className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="">Select status...</option>
              {["backlog", "ready", "in-progress", "blocked", "on-hold", "done", "cancelled"].map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
            <Button size="sm" onClick={applyStatus} disabled={!targetStatus || targetStatus === task.statusRaw}>
              Apply
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Details</CardTitle></CardHeader>
          <CardContent className="text-sm space-y-1">
            <p>Risk: <Badge variant="outline">{task.risk}</Badge></p>
            <p>Scope: <Badge variant="outline">{task.scope}</Badge></p>
            <p>Complexity: <Badge variant="outline">{task.complexity}</Badge></p>
            {task.tags.length > 0 && (
              <div className="flex gap-1 flex-wrap pt-1">
                {task.tags.map((t: string) => <Badge key={t} variant="outline" className="text-xs">{t}</Badge>)}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-sm font-medium">Assignment</CardTitle>
            {assignMode === "" && (
              <div className="flex gap-1">
                <Button size="sm" variant="outline" className="h-6 text-xs" onClick={() => setAssignMode("agent")}>Assign Agent</Button>
                <Button size="sm" variant="outline" className="h-6 text-xs" onClick={() => setAssignMode("human")}>Assign Human</Button>
              </div>
            )}
          </div>
        </CardHeader>
        <CardContent>
          {assignMode === "agent" && (
            <div className="flex items-end gap-2">
              <div>
                <label className="text-xs font-medium">Role</label>
                <Input value={assignRole} onChange={(e) => setAssignRole(e.target.value)} className="mt-1 h-8 w-32 text-xs" />
              </div>
              <div>
                <label className="text-xs font-medium">Model</label>
                <Input value={assignModel} onChange={(e) => setAssignModel(e.target.value)} placeholder="e.g. claude-sonnet-4-6" className="mt-1 h-8 w-48 text-xs" />
              </div>
              <Button size="sm" className="h-8" onClick={onAssign}>Assign</Button>
              <Button size="sm" variant="outline" className="h-8" onClick={() => setAssignMode("")}>Cancel</Button>
            </div>
          )}
          {assignMode === "human" && (
            <div className="flex items-end gap-2">
              <div>
                <label className="text-xs font-medium">Name</label>
                <Input value={assignName} onChange={(e) => setAssignName(e.target.value)} className="mt-1 h-8 w-48 text-xs" />
              </div>
              <Button size="sm" className="h-8" onClick={onAssign}>Assign</Button>
              <Button size="sm" variant="outline" className="h-8" onClick={() => setAssignMode("")}>Cancel</Button>
            </div>
          )}
          {assignMode === "" && (
            <p className="text-sm text-muted-foreground">
              {task.assignee ? `Assigned: ${JSON.stringify(task.assignee)}` : "Unassigned"}
            </p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Checklist</CardTitle></CardHeader>
        <CardContent className="space-y-2">
          {task.checklist.length > 0 && (
            <ul className="space-y-1">
              {task.checklist.map((item: any) => (
                <li key={item.id} className="flex items-center gap-2 text-sm">
                  <button
                    type="button"
                    onClick={() => onChecklistToggle(item.id, item.completed)}
                    className="shrink-0 text-lg leading-none hover:opacity-70"
                  >
                    {item.completed ? <span className="text-green-600">&#x2611;</span> : <span className="text-muted-foreground">&#x2610;</span>}
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

      <Card>
        <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Dependencies</CardTitle></CardHeader>
        <CardContent className="space-y-2">
          {task.dependencies.length > 0 && (
            <ul className="space-y-1">
              {task.dependencies.map((dep: any) => (
                <li key={dep.taskId} className="flex items-center gap-2 text-sm">
                  <Link to={`/tasks/${dep.taskId}`} className="font-mono underline">{dep.taskId}</Link>
                  <span className="text-muted-foreground">{dep.type}</span>
                  <Button size="sm" variant="ghost" className="h-5 px-1 text-xs text-destructive" onClick={() => onDepRemove(dep.taskId)}>remove</Button>
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
            <Button size="sm" variant="outline" className="h-8" onClick={onDepAdd}>Add Dependency</Button>
          </div>
        </CardContent>
      </Card>

      {task.linkedRequirementIds.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Linked Requirements</CardTitle></CardHeader>
          <CardContent>
            <div className="flex gap-2 flex-wrap">
              {task.linkedRequirementIds.map((id: string) => (
                <Link key={id} to={`/planning/requirements/${id}`}>
                  <Badge variant="outline" className="font-mono">{id}</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Workflows
// ---------------------------------------------------------------------------

export function WorkflowsPage() {
  const [statusFilter, setStatusFilter] = useState<string>("");
  const [result, reexecute] = useQuery({
    query: WORKFLOWS_QUERY,
    variables: { status: statusFilter || undefined },
  });
  const [, runWf] = useMutation(RUN_WORKFLOW);
  const [, pauseWf] = useMutation(PAUSE_WORKFLOW);
  const [, resumeWf] = useMutation(RESUME_WORKFLOW);
  const [, cancelWf] = useMutation(CANCEL_WORKFLOW);
  const [runTaskId, setRunTaskId] = useState("");
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const workflows = data?.workflows ?? [];

  const onRun = async (e: FormEvent) => {
    e.preventDefault();
    if (!runTaskId.trim()) return;
    const { error: err } = await runWf({ taskId: runTaskId.trim() });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `Workflow started for ${runTaskId}.` });
      setRunTaskId("");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onAction = async (id: string, action: "pause" | "resume" | "cancel") => {
    const fn = action === "pause" ? pauseWf : action === "resume" ? resumeWf : cancelWf;
    const { error: err } = await fn({ id });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `${action} applied to ${id}.` });
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const counts = useMemo(() => {
    const c = { running: 0, paused: 0, completed: 0, failed: 0 };
    for (const w of workflows) {
      const s = (w.statusRaw || "").toLowerCase();
      if (s === "running") c.running++;
      else if (s === "paused") c.paused++;
      else if (s === "completed") c.completed++;
      else if (s === "failed") c.failed++;
    }
    return c;
  }, [workflows]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold tracking-tight">Workflows</h1>
        <span className="text-sm text-muted-foreground">{workflows.length} total</span>
      </div>

      <div className="grid grid-cols-4 gap-2">
        <StatCard label="Running" value={counts.running} />
        <StatCard label="Paused" value={counts.paused} />
        <StatCard label="Completed" value={counts.completed} />
        <StatCard label="Failed" value={counts.failed} />
      </div>

      <div className="flex gap-2 items-end">
        <form onSubmit={onRun} className="flex gap-2 items-end">
          <Input
            placeholder="Task ID (e.g. TASK-014)"
            value={runTaskId}
            onChange={(e) => setRunTaskId(e.target.value)}
            className="w-48"
          />
          <Button type="submit" size="sm">Run Workflow</Button>
        </form>
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      <Tabs defaultValue="all" onValueChange={(v) => setStatusFilter(v === "all" ? "" : v)}>
        <TabsList>
          <TabsTrigger value="all">All</TabsTrigger>
          <TabsTrigger value="running">Running</TabsTrigger>
          <TabsTrigger value="paused">Paused</TabsTrigger>
          <TabsTrigger value="completed">Completed</TabsTrigger>
          <TabsTrigger value="failed">Failed</TabsTrigger>
        </TabsList>
      </Tabs>

      {workflows.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No workflows found.</p>
      ) : (
        <div className="space-y-3">
          {workflows.map((wf: any) => (
            <Card key={wf.id}>
              <CardContent className="pt-4">
                <div className="flex items-start justify-between">
                  <div>
                    <Link to={`/workflows/${wf.id}`} className="font-mono text-sm underline">{wf.id}</Link>
                    <p className="text-xs text-muted-foreground mt-0.5">
                      Task: <Link to={`/tasks/${wf.taskId}`} className="underline">{wf.taskId}</Link>
                      {wf.currentPhase && <> &middot; Phase: {wf.currentPhase}</>}
                      {wf.totalReworks > 0 && <> &middot; {wf.totalReworks} reworks</>}
                    </p>
                  </div>
                  <Badge variant={statusColor(wf.statusRaw)}>{wf.statusRaw}</Badge>
                </div>

                {wf.phases?.length > 0 && (
                  <div className="flex gap-1 mt-2 flex-wrap">
                    {wf.phases.map((p: any) => (
                      <span
                        key={p.phaseId}
                        className={`inline-block rounded px-1.5 py-0.5 text-[10px] font-mono ${
                          p.status === "completed" ? "bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200" :
                          p.status === "running" ? "bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200" :
                          p.status === "failed" ? "bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200" :
                          "bg-muted text-muted-foreground"
                        }`}
                      >
                        {p.phaseId}
                      </span>
                    ))}
                  </div>
                )}

                <div className="flex gap-1 mt-2">
                  <Button size="sm" variant="outline" onClick={() => onAction(wf.id, "pause")} disabled={wf.statusRaw !== "running"}>Pause</Button>
                  <Button size="sm" variant="outline" onClick={() => onAction(wf.id, "resume")} disabled={wf.statusRaw !== "paused"}>Resume</Button>
                  <Button size="sm" variant="destructive" onClick={() => onAction(wf.id, "cancel")} disabled={["completed", "failed", "cancelled"].includes(wf.statusRaw)}>Cancel</Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}

export function WorkflowDetailPage() {
  const { workflowId } = useParams();
  const [result, reexecute] = useQuery({ query: WORKFLOW_DETAIL_QUERY, variables: { id: workflowId } });
  const [, pauseWf] = useMutation(PAUSE_WORKFLOW);
  const [, resumeWf] = useMutation(RESUME_WORKFLOW);
  const [, cancelWf] = useMutation(CANCEL_WORKFLOW);
  const [wfMessage, setWfMessage] = useState<string | null>(null);
  const [wfOperating, setWfOperating] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState(false);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const wf = data?.workflow;
  if (!wf) return <PageError message={`Workflow ${workflowId} not found.`} />;

  const checkpoints = data?.workflowCheckpoints ?? [];
  const decisions = wf.decisions ?? [];

  const wfAction = async (label: string, fn: () => Promise<any>) => {
    setWfOperating(true);
    setWfMessage(null);
    const res = await fn();
    setWfOperating(false);
    if (res.error) {
      setWfMessage(`Error: ${res.error.message}`);
    } else {
      setWfMessage(`${label} successful.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const isRunning = wf.statusRaw === "running";
  const isPaused = wf.statusRaw === "paused";
  const isTerminal = ["completed", "failed", "cancelled"].includes(wf.statusRaw);

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between">
        <div>
          <p className="text-sm text-muted-foreground font-mono">{wf.id}</p>
          <h1 className="text-2xl font-semibold tracking-tight">
            Workflow for <Link to={`/tasks/${wf.taskId}`} className="underline">{wf.taskId}</Link>
          </h1>
          <div className="flex gap-2 mt-2">
            <Badge variant={statusColor(wf.statusRaw)}>{wf.statusRaw}</Badge>
            {wf.workflowRef && <Badge variant="outline">{wf.workflowRef}</Badge>}
            {wf.totalReworks > 0 && <Badge variant="outline">{wf.totalReworks} reworks</Badge>}
          </div>
        </div>
        {!isTerminal && (
          <div className="flex items-center gap-2">
            {isRunning && (
              <Button variant="secondary" disabled={wfOperating} onClick={() => wfAction("Pause", () => pauseWf({ id: workflowId }))}>
                Pause
              </Button>
            )}
            {isPaused && (
              <Button variant="secondary" disabled={wfOperating} onClick={() => wfAction("Resume", () => resumeWf({ id: workflowId }))}>
                Resume
              </Button>
            )}
            {confirmCancel ? (
              <>
                <Button variant="destructive" disabled={wfOperating} onClick={() => { setConfirmCancel(false); wfAction("Cancel", () => cancelWf({ id: workflowId })); }}>
                  Confirm Cancel
                </Button>
                <Button variant="outline" onClick={() => setConfirmCancel(false)}>Back</Button>
              </>
            ) : (
              <Button variant="destructive" disabled={wfOperating} onClick={() => setConfirmCancel(true)}>
                Cancel Workflow
              </Button>
            )}
          </div>
        )}
      </div>

      {wfMessage && (
        <Alert variant={wfMessage.startsWith("Error") ? "destructive" : "default"}>
          <AlertDescription>{wfMessage}</AlertDescription>
        </Alert>
      )}

      <Card>
        <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Phase Timeline</CardTitle></CardHeader>
        <CardContent>
          <div className="space-y-2">
            {wf.phases.map((p: any, i: number) => (
              <div key={p.phaseId} className="flex items-start gap-3">
                <div className="flex flex-col items-center">
                  <div className={`h-3 w-3 rounded-full ${
                    p.status === "completed" ? "bg-green-500" :
                    p.status === "running" ? "bg-blue-500 animate-pulse" :
                    p.status === "failed" ? "bg-red-500" :
                    "bg-muted-foreground/30"
                  }`} />
                  {i < wf.phases.length - 1 && <div className="w-px h-6 bg-border" />}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-sm">{p.phaseId}</span>
                    <Badge variant={statusColor(p.status)} className="text-[10px]">{p.status}</Badge>
                    {p.attempt > 1 && <span className="text-xs text-muted-foreground">attempt {p.attempt}</span>}
                  </div>
                  {p.errorMessage && <p className="text-xs text-destructive mt-0.5">{p.errorMessage}</p>}
                  {(p.startedAt || p.completedAt) && (
                    <p className="text-xs text-muted-foreground">
                      {p.startedAt && <>Started: {p.startedAt}</>}
                      {p.completedAt && <> &middot; Completed: {p.completedAt}</>}
                    </p>
                  )}
                </div>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>

      {decisions.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Decisions</CardTitle></CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Phase</TableHead>
                  <TableHead>Decision</TableHead>
                  <TableHead>Target</TableHead>
                  <TableHead>Confidence</TableHead>
                  <TableHead>Source</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {decisions.map((d: any, i: number) => (
                  <TableRow key={i}>
                    <TableCell className="font-mono text-xs">{d.phaseId}</TableCell>
                    <TableCell>{d.decision}</TableCell>
                    <TableCell className="font-mono text-xs">{d.targetPhase ?? "-"}</TableCell>
                    <TableCell>{(d.confidence * 100).toFixed(0)}%</TableCell>
                    <TableCell className="text-xs text-muted-foreground">{d.source}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      )}

      {checkpoints.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Checkpoints</CardTitle></CardHeader>
          <CardContent>
            <ul className="space-y-2">
              {checkpoints.map((cp: any) => (
                <li key={cp.id} className="text-sm">
                  <Link
                    to={`/workflows/${workflowId}/checkpoints/${cp.id}`}
                    className="font-mono underline"
                  >
                    {cp.id}
                  </Link>
                  <span className="text-muted-foreground ml-2">{cp.phase}</span>
                  {cp.timestamp && <span className="text-muted-foreground ml-2">{cp.timestamp}</span>}
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

export function WorkflowCheckpointPage() {
  const { workflowId, checkpoint } = useParams();
  const [result] = useQuery({
    query: WORKFLOW_DETAIL_QUERY,
    variables: { id: workflowId },
  });

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const checkpoints = data?.workflowCheckpoints ?? [];
  const cp = checkpoints.find((c: any) => c.id === checkpoint);

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold tracking-tight">Checkpoint {checkpoint}</h1>
      <p className="text-sm text-muted-foreground">
        Workflow: <Link to={`/workflows/${workflowId}`} className="underline font-mono">{workflowId}</Link>
      </p>
      {cp ? (
        <Card>
          <CardContent className="pt-4">
            <pre className="text-xs overflow-auto">{cp.data ?? "No data"}</pre>
          </CardContent>
        </Card>
      ) : (
        <PageError message={`Checkpoint ${checkpoint} not found.`} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

export function QueuePage() {
  const [result, reexecute] = useQuery({ query: QUEUE_QUERY });
  const [, holdMut] = useMutation(QUEUE_HOLD);
  const [, releaseMut] = useMutation(QUEUE_RELEASE);
  const [, reorderMut] = useMutation(QUEUE_REORDER);
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const entries = data?.queue ?? [];
  const stats = data?.queueStats;

  const onHold = async (taskId: string) => {
    const { error: err } = await holdMut({ taskId });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `Held ${taskId}.` });
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onRelease = async (taskId: string) => {
    const { error: err } = await releaseMut({ taskId });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `Released ${taskId}.` });
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const moveEntry = async (index: number, direction: -1 | 1) => {
    const ids = entries.map((e: any) => e.taskId);
    const newIndex = index + direction;
    if (newIndex < 0 || newIndex >= ids.length) return;
    [ids[index], ids[newIndex]] = [ids[newIndex], ids[index]];
    const { error: err } = await reorderMut({ taskIds: ids });
    if (err) setFeedback({ kind: "error", message: err.message });
    else reexecute({ requestPolicy: "network-only" });
  };

  const sortByPriority = async () => {
    const priorityOrder: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3 };
    const sorted = [...entries].sort((a: any, b: any) => (priorityOrder[a.priority] ?? 9) - (priorityOrder[b.priority] ?? 9));
    const { error: err } = await reorderMut({ taskIds: sorted.map((e: any) => e.taskId) });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: "Queue reordered by priority." });
      reexecute({ requestPolicy: "network-only" });
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold tracking-tight">Queue</h1>
        {entries.length > 1 && <Button size="sm" variant="outline" onClick={sortByPriority}>Sort by Priority</Button>}
      </div>

      <div className="grid grid-cols-2 md:grid-cols-5 gap-2">
        <StatCard label="Depth" value={stats?.depth ?? 0} />
        <StatCard label="Ready" value={stats?.readyCount ?? 0} />
        <StatCard label="Held" value={stats?.heldCount ?? 0} />
        <StatCard label="Avg Wait" value={stats?.avgWait != null ? `${stats.avgWait.toFixed(1)}s` : "-"} />
        <StatCard label="Throughput" value={stats?.throughput != null ? `${stats.throughput.toFixed(1)}/hr` : "-"} />
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {entries.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">Queue is empty.</p>
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">#</TableHead>
                <TableHead>Task</TableHead>
                <TableHead>Title</TableHead>
                <TableHead>Priority</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Wait</TableHead>
                <TableHead>Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.map((entry: any, i: number) => (
                <TableRow key={entry.taskId}>
                  <TableCell className="text-xs text-muted-foreground">{entry.position ?? i + 1}</TableCell>
                  <TableCell>
                    <Link to={`/tasks/${entry.taskId}`} className="font-mono text-xs underline">{entry.taskId}</Link>
                  </TableCell>
                  <TableCell>{entry.title ?? "-"}</TableCell>
                  <TableCell>{entry.priority && <Badge variant={priorityColor(entry.priority)}>{entry.priority}</Badge>}</TableCell>
                  <TableCell>{entry.status && <Badge variant={statusColor(entry.status)}>{entry.status}</Badge>}</TableCell>
                  <TableCell className="text-xs text-muted-foreground">{entry.waitTime != null ? `${entry.waitTime.toFixed(0)}s` : "-"}</TableCell>
                  <TableCell>
                    <div className="flex gap-1">
                      <Button size="sm" variant="ghost" className="h-6 w-6 p-0" onClick={() => moveEntry(i, -1)} disabled={i === 0} aria-label={`Move ${entry.taskId} up`}>↑</Button>
                      <Button size="sm" variant="ghost" className="h-6 w-6 p-0" onClick={() => moveEntry(i, 1)} disabled={i === entries.length - 1} aria-label={`Move ${entry.taskId} down`}>↓</Button>
                      <Button size="sm" variant="outline" className="h-6 text-xs" onClick={() => onHold(entry.taskId)}>Hold</Button>
                      <Button size="sm" variant="outline" className="h-6 text-xs" onClick={() => onRelease(entry.taskId)}>Release</Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

export function DaemonPage() {
  const [result, reexecute] = useQuery({ query: DAEMON_QUERY });
  const [, startMut] = useMutation(DAEMON_START);
  const [, stopMut] = useMutation(DAEMON_STOP);
  const [, pauseMut] = useMutation(DAEMON_PAUSE);
  const [, resumeMut] = useMutation(DAEMON_RESUME);
  const [, clearLogsMut] = useMutation(DAEMON_CLEAR_LOGS);
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const status = data?.daemonStatus;
  const health = data?.daemonHealth;
  const agents = data?.agentRuns ?? [];
  const logs = data?.daemonLogs ?? [];

  const runAction = async (label: string, fn: () => Promise<any>) => {
    const { error: err } = await fn();
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `${label} successful.` });
      reexecute({ requestPolicy: "network-only" });
    }
  };

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold tracking-tight">Daemon</h1>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      <div className="grid md:grid-cols-2 gap-4">
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Status</CardTitle></CardHeader>
          <CardContent className="space-y-2">
            <div className="flex items-center gap-2">
              <Badge variant={status?.healthy ? "default" : "destructive"}>{status?.statusRaw ?? "unknown"}</Badge>
              {status?.runnerConnected && <span className="text-xs text-muted-foreground">runner connected</span>}
            </div>
            <p className="text-sm text-muted-foreground">Active agents: {status?.activeAgents ?? 0}{status?.maxAgents ? ` / ${status.maxAgents}` : ""}</p>
            {status?.projectRoot && <p className="text-xs text-muted-foreground truncate">Root: {status.projectRoot}</p>}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Controls</CardTitle></CardHeader>
          <CardContent>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" onClick={() => runAction("Start", () => startMut({}))}>Start</Button>
              <Button size="sm" variant="outline" onClick={() => runAction("Pause", () => pauseMut({}))}>Pause</Button>
              <Button size="sm" variant="outline" onClick={() => runAction("Resume", () => resumeMut({}))}>Resume</Button>
              <Button size="sm" variant="destructive" onClick={() => runAction("Stop", () => stopMut({}))}>Stop</Button>
              <Button size="sm" variant="outline" onClick={() => runAction("Clear Logs", () => clearLogsMut({}))}>Clear Logs</Button>
            </div>
          </CardContent>
        </Card>
      </div>

      {agents.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Active Agents</CardTitle></CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Run</TableHead>
                  <TableHead>Task</TableHead>
                  <TableHead>Phase</TableHead>
                  <TableHead>Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {agents.map((a: any) => (
                  <TableRow key={a.runId}>
                    <TableCell className="font-mono text-xs">{a.runId}</TableCell>
                    <TableCell>{a.taskId ? <Link to={`/tasks/${a.taskId}`} className="underline">{a.taskTitle ?? a.taskId}</Link> : "-"}</TableCell>
                    <TableCell className="font-mono text-xs">{a.phaseId ?? "-"}</TableCell>
                    <TableCell><Badge variant={statusColor(a.status)}>{a.status}</Badge></TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      )}

      {logs.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Logs</CardTitle></CardHeader>
          <CardContent>
            <div className="max-h-80 overflow-y-auto font-mono text-xs space-y-0.5">
              {logs.map((log: any, i: number) => (
                <div key={i} className="flex gap-2">
                  <span className="text-muted-foreground shrink-0">{log.timestamp ?? ""}</span>
                  <span className={log.level === "ERROR" ? "text-destructive" : "text-foreground"}>{log.message ?? ""}</span>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

export function ProjectsPage() {
  const [result, reexecute] = useQuery({ query: PROJECTS_QUERY });
  const [, createProject] = useMutation(CREATE_PROJECT);
  const { data, fetching, error } = result;
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [description, setDescription] = useState("");
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const projects = data?.projects ?? [];
  const active = data?.projectsActive ?? [];

  const onCreateProject = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !path.trim()) return;
    const { error: err } = await createProject({ name: name.trim(), path: path.trim(), description: description.trim() || undefined });
    if (err) { setFeedback({ kind: "error", message: err.message }); return; }
    setFeedback({ kind: "ok", message: `Project "${name.trim()}" created.` });
    setName(""); setPath(""); setDescription(""); setShowCreate(false);
    reexecute({ requestPolicy: "network-only" });
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold tracking-tight">Projects</h1>
        <Button size="sm" onClick={() => setShowCreate(!showCreate)}>{showCreate ? "Cancel" : "New Project"}</Button>
      </div>
      {active.length > 0 && (
        <p className="text-sm text-muted-foreground">Active: {active.map((p: any) => p.name).join(", ")}</p>
      )}

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {showCreate && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Create Project</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={onCreateProject} className="space-y-3">
              <Input placeholder="Project name" value={name} onChange={(e) => setName(e.target.value)} required />
              <Input placeholder="Project path (e.g. /path/to/project)" value={path} onChange={(e) => setPath(e.target.value)} required />
              <Input placeholder="Description (optional)" value={description} onChange={(e) => setDescription(e.target.value)} />
              <Button type="submit" size="sm">Create</Button>
            </form>
          </CardContent>
        </Card>
      )}

      {projects.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No projects found.</p>
      ) : (
        <div className="grid md:grid-cols-2 lg:grid-cols-3 gap-4">
          {projects.map((p: any) => (
            <Link key={p.id} to={`/projects/${p.id}`}>
              <Card className="hover:border-foreground/20 transition-colors">
                <CardContent className="pt-4">
                  <p className="font-medium">{p.name}</p>
                  {p.path && <p className="text-xs text-muted-foreground truncate">{p.path}</p>}
                  {p.description && <p className="text-sm text-muted-foreground mt-1 line-clamp-2">{p.description}</p>}
                  {p.archived && <Badge variant="outline" className="mt-1">archived</Badge>}
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}

export function ProjectDetailPage() {
  const { projectId } = useParams();
  const navigate = useNavigate();
  const [result, reexecute] = useQuery({ query: PROJECT_DETAIL_QUERY, variables: { id: projectId } });
  const [, updateProject] = useMutation(UPDATE_PROJECT);
  const [, deleteProject] = useMutation(DELETE_PROJECT);
  const [, loadProject] = useMutation(LOAD_PROJECT);
  const [, archiveProject] = useMutation(ARCHIVE_PROJECT);
  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editType, setEditType] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);
  const [initialized, setInitialized] = useState(false);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const project = data?.project;
  if (!project) return <PageError message={`Project ${projectId} not found.`} />;

  if (project && !initialized) {
    setEditName(project.name ?? "");
    setEditDescription(project.description ?? "");
    setEditType(project.type ?? "");
    setInitialized(true);
  }

  const onSave = async (e: FormEvent) => {
    e.preventDefault();
    const { error: err } = await updateProject({
      id: projectId,
      name: editName.trim() || undefined,
      description: editDescription.trim() || undefined,
      projectType: editType.trim() || undefined,
    });
    if (err) { setFeedback({ kind: "error", message: err.message }); return; }
    setFeedback({ kind: "ok", message: "Project updated." });
    setEditing(false);
    setInitialized(false);
    reexecute({ requestPolicy: "network-only" });
  };

  const onDelete = async () => {
    const { error: err } = await deleteProject({ id: projectId });
    if (err) { setFeedback({ kind: "error", message: err.message }); return; }
    navigate("/projects", { replace: true });
  };

  const onLoad = async () => {
    const { error: err } = await loadProject({ id: projectId });
    if (err) setFeedback({ kind: "error", message: err.message });
    else setFeedback({ kind: "ok", message: "Project set as active." });
  };

  const onArchive = async () => {
    const { error: err } = await archiveProject({ id: projectId });
    if (err) { setFeedback({ kind: "error", message: err.message }); return; }
    setFeedback({ kind: "ok", message: "Project archived." });
    setInitialized(false);
    reexecute({ requestPolicy: "network-only" });
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{project.name}</h1>
          {project.path && <p className="text-sm text-muted-foreground">{project.path}</p>}
        </div>
        <div className="flex items-center gap-2">
          {project.type && <Badge variant="outline">{project.type}</Badge>}
          {project.archived && <Badge variant="destructive">archived</Badge>}
        </div>
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {project.description && !editing && <p className="text-sm">{project.description}</p>}

      {(project.techStack ?? []).length > 0 && (
        <div className="flex gap-2 flex-wrap">
          {project.techStack.map((t: string) => <Badge key={t} variant="outline">{t}</Badge>)}
        </div>
      )}

      <div className="flex gap-2 flex-wrap">
        <Button size="sm" variant="outline" onClick={() => setEditing(!editing)}>{editing ? "Cancel Edit" : "Edit"}</Button>
        <Button size="sm" variant="outline" onClick={onLoad}>Set Active</Button>
        {!project.archived && <Button size="sm" variant="outline" onClick={onArchive}>Archive</Button>}
        {confirmDelete ? (
          <>
            <Button size="sm" variant="destructive" onClick={onDelete}>Confirm Delete</Button>
            <Button size="sm" variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
          </>
        ) : (
          <Button size="sm" variant="destructive" onClick={() => setConfirmDelete(true)}>Delete</Button>
        )}
      </div>

      {editing && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Edit Project</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={onSave} className="space-y-3">
              <div>
                <label className="text-sm font-medium">Name</label>
                <Input value={editName} onChange={(e) => setEditName(e.target.value)} />
              </div>
              <div>
                <label className="text-sm font-medium">Description</label>
                <Textarea value={editDescription} onChange={(e) => setEditDescription(e.target.value)} rows={3} />
              </div>
              <div>
                <label className="text-sm font-medium">Type</label>
                <Input value={editType} onChange={(e) => setEditType(e.target.value)} placeholder="e.g., rust, node, python" />
              </div>
              <Button type="submit" size="sm">Save Changes</Button>
            </form>
          </CardContent>
        </Card>
      )}

      <Link to="/projects"><Button variant="outline" size="sm">Back to Projects</Button></Link>
    </div>
  );
}

export function RequirementDetailPage() {
  const params = useParams();
  const requirementId = params.requirementId ?? params.projectId ?? "";
  const [result] = useQuery({ query: REQUIREMENT_DETAIL_QUERY, variables: { id: requirementId } });

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const req = data?.requirement;
  if (!req) return <PageError message={`Requirement ${requirementId} not found.`} />;

  return (
    <div className="space-y-4">
      <div>
        <p className="text-sm text-muted-foreground font-mono">{req.id}</p>
        <h1 className="text-2xl font-semibold tracking-tight">{req.title}</h1>
        <div className="flex gap-2 mt-2">
          <Badge variant={statusColor(req.statusRaw)}>{req.statusRaw}</Badge>
          <Badge variant={priorityColor(req.priorityRaw)}>{req.priorityRaw}</Badge>
          {req.requirementType && <Badge variant="outline">{req.requirementType}</Badge>}
        </div>
      </div>
      {req.description && (
        <Card>
          <CardContent className="pt-4 text-sm whitespace-pre-wrap">{req.description}</CardContent>
        </Card>
      )}
      {req.linkedTaskIds?.length > 0 && (
        <Card>
          <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Linked Tasks</CardTitle></CardHeader>
          <CardContent>
            <div className="flex gap-2 flex-wrap">
              {req.linkedTaskIds.map((id: string) => (
                <Link key={id} to={`/tasks/${id}`}><Badge variant="outline" className="font-mono">{id}</Badge></Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

export function EventsPage() {
  const { connectionState, events } = useDaemonEvents();

  const mostRecent = useMemo(() => [...events].reverse().slice(0, 50), [events]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold tracking-tight">Events</h1>
        <Badge variant={connectionState === "open" ? "default" : "outline"}>{connectionState}</Badge>
      </div>
      {mostRecent.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No events received yet.</p>
      ) : (
        <Card>
          <CardContent className="pt-4">
            <div className="max-h-[600px] overflow-y-auto space-y-2">
              {mostRecent.map((evt: any, i: number) => (
                <div key={evt.id ?? i} className="border-b border-border pb-2 last:border-0">
                  <div className="flex items-center gap-2">
                    <Badge variant="outline" className="text-[10px]">{evt.event_type ?? "event"}</Badge>
                    <span className="text-xs text-muted-foreground">{evt.timestamp ?? ""}</span>
                  </div>
                  <pre className="text-xs mt-1 overflow-x-auto">{JSON.stringify(evt.data ?? evt, null, 2)}</pre>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Review Handoff
// ---------------------------------------------------------------------------

export function ReviewHandoffPage() {
  const [, handoff] = useMutation(REVIEW_HANDOFF);
  const [targetRole, setTargetRole] = useState("em");
  const [question, setQuestion] = useState("");
  const [context, setContext] = useState("");
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!question.trim()) return;
    const { error } = await handoff({
      targetRole,
      question: question.trim(),
      context: context.trim() || undefined,
    });
    if (error) setFeedback({ kind: "error", message: error.message });
    else {
      setFeedback({ kind: "ok", message: "Review handoff submitted." });
      setQuestion("");
      setContext("");
    }
  };

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold tracking-tight">Review Handoff</h1>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      <Card>
        <CardContent className="pt-4">
          <form onSubmit={onSubmit} className="space-y-4">
            <div>
              <label className="text-sm font-medium">Target Role</label>
              <select
                value={targetRole}
                onChange={(e) => setTargetRole(e.target.value)}
                className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                <option value="em">em</option>
                <option value="reviewer">reviewer</option>
                <option value="qa">qa</option>
              </select>
            </div>
            <div>
              <label className="text-sm font-medium">Question</label>
              <Textarea
                value={question}
                onChange={(e) => setQuestion(e.target.value)}
                rows={3}
                required
                className="mt-1"
              />
            </div>
            <div>
              <label className="text-sm font-medium">Context (optional)</label>
              <Textarea
                value={context}
                onChange={(e) => setContext(e.target.value)}
                rows={3}
                className="mt-1"
              />
            </div>
            <Button type="submit">Submit Handoff</Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Not Found
// ---------------------------------------------------------------------------

export function NotFoundPage() {
  return (
    <div className="space-y-4 py-12 text-center">
      <h1 className="text-4xl font-bold">404</h1>
      <p className="text-muted-foreground">The requested page does not exist.</p>
      <Link to="/dashboard">
        <Button variant="outline">Go to Dashboard</Button>
      </Link>
    </div>
  );
}
