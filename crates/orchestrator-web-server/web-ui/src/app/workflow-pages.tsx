import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  WorkflowsDocument,
  WorkflowDetailDocument,
  RunWorkflowDocument,
  PauseWorkflowDocument,
  ResumeWorkflowDocument,
  CancelWorkflowDocument,
  ApprovePhaseDocument,
} from "@/lib/graphql/generated/graphql";
import { statusColor, StatusDot, PageLoading, PageError, StatCard, SectionHeading, Markdown } from "./shared";

const PHASE_OUTPUT_QUERY = `query PhaseOutput($workflowId: ID!, $phaseId: String, $tail: Int) { phaseOutput(workflowId: $workflowId, phaseId: $phaseId, tail: $tail) { lines phaseId hasMore } }`;

function useElapsedTime(startedAt: string | null | undefined): string {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!startedAt) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [startedAt]);
  if (!startedAt) return "";
  const ms = Date.now() - new Date(startedAt).getTime();
  return formatDuration(ms);
}

function formatTimeAgo(ts: string | null | undefined): string {
  if (!ts) return "";
  const ms = Date.now() - new Date(ts).getTime();
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function formatDuration(ms: number): string {
  if (ms < 0) return "0s";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

function getWorkflowStartedAt(wf: { phases?: readonly { startedAt?: string | null }[] | null }): string | null {
  const phases = wf.phases ?? [];
  for (const p of phases) {
    if (p.startedAt) return p.startedAt;
  }
  return null;
}

function getWorkflowCompletedAt(wf: { phases?: readonly { completedAt?: string | null }[] | null }): string | null {
  const phases = wf.phases ?? [];
  for (let i = phases.length - 1; i >= 0; i--) {
    if (phases[i].completedAt) return phases[i].completedAt!;
  }
  return null;
}

function PhaseOutputPanel({ workflowId, currentPhase, isRunning }: { workflowId: string; currentPhase: string | null | undefined; isRunning: boolean }) {
  const [collapsed, setCollapsed] = useState(false);
  const outputRef = useRef<HTMLPreElement>(null);
  const phaseId = currentPhase ?? undefined;

  const [result, reexecute] = useQuery<{ phaseOutput: { lines: string[]; phaseId: string; hasMore: boolean } }>({
    query: PHASE_OUTPUT_QUERY,
    variables: { workflowId, phaseId, tail: 200 },
  });

  useEffect(() => {
    if (!isRunning) return;
    const id = setInterval(() => reexecute(), 3000);
    return () => clearInterval(id);
  }, [isRunning, reexecute]);

  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [result.data?.phaseOutput?.lines]);

  const output = result.data?.phaseOutput;

  return (
    <Card className="border-border/40 bg-card/60">
      <CardHeader className="pb-2 pt-3 px-4">
        <button type="button" onClick={() => setCollapsed(!collapsed)} className="flex items-center justify-between w-full">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">
            Agent Output
            {output && <span className="ml-2 normal-case tracking-normal text-muted-foreground/40">{output.phaseId} &middot; {output.lines.length} line{output.lines.length !== 1 ? "s" : ""}{output.hasMore ? "+" : ""}</span>}
          </CardTitle>
          <span className="text-xs text-muted-foreground">{collapsed ? "\u25B6" : "\u25BC"}</span>
        </button>
      </CardHeader>
      {!collapsed && (
        <CardContent className="px-4 pb-3">
          {result.fetching && !output && <p className="text-xs text-muted-foreground">Loading...</p>}
          {result.error && <p className="text-xs text-destructive">{result.error.message}</p>}
          {output && output.lines.length === 0 && <p className="text-xs text-muted-foreground/60">No output yet</p>}
          {output && output.lines.length > 0 && (
            <pre ref={outputRef} className="text-xs font-mono overflow-auto max-h-80 p-3 rounded bg-muted/20 whitespace-pre-wrap">
              {output.lines.join("\n")}
            </pre>
          )}
        </CardContent>
      )}
    </Card>
  );
}

type WfPhase = { phaseId: string; status?: string | null; startedAt?: string | null; completedAt?: string | null; attempt?: number | null; errorMessage?: string | null };
type WfSummary = { id: string; taskId: string; workflowRef?: string | null; status?: string | null; statusRaw?: string | null; currentPhase?: string | null; totalReworks?: number | null; phases?: readonly WfPhase[] | null };

function ActiveWorkflowRow({ wf }: { wf: WfSummary }) {
  const phases = wf.phases ?? [];
  const completed = phases.filter((p) => p.status === "completed").length;
  const total = phases.length;
  const pct = total > 0 ? (completed / total) * 100 : 0;
  const startedAt = getWorkflowStartedAt(wf);
  const elapsed = useElapsedTime(startedAt);

  return (
    <Link to={`/workflows/${wf.id}`}>
      <Card className="border-border/40 bg-card/60 p-3 hover:border-border/60 transition-colors">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <StatusDot status={wf.statusRaw ?? ""} />
            <span className="font-mono text-xs text-muted-foreground shrink-0">{wf.taskId}</span>
            <span className="text-sm font-medium truncate">{wf.id}</span>
          </div>
          <div className="flex items-center gap-3 text-xs text-muted-foreground shrink-0 hidden sm:flex">
            {wf.currentPhase && <span className="font-mono">{wf.currentPhase}</span>}
            <span>{completed}/{total}</span>
            {elapsed && <span>{elapsed}</span>}
          </div>
        </div>
        <div className="mt-2 h-1 rounded-full bg-muted/30">
          <div className="h-full rounded-full bg-[var(--ao-running)] transition-all" style={{ width: `${pct}%` }} />
        </div>
        {phases.length > 0 && (
          <div className="flex gap-1 mt-2 flex-wrap">
            {phases.map((p) => (
              <span key={p.phaseId} className="text-[10px] font-mono text-muted-foreground">
                {p.status === "completed" ? "\u2713" : p.status === "running" ? "\u25C9" : "\u00B7"}{p.phaseId}
              </span>
            ))}
          </div>
        )}
      </Card>
    </Link>
  );
}

function RecentWorkflowRow({ wf }: { wf: WfSummary }) {
  const startedAt = getWorkflowStartedAt(wf);
  const completedAt = getWorkflowCompletedAt(wf);
  const duration = startedAt && completedAt ? formatDuration(new Date(completedAt).getTime() - new Date(startedAt).getTime()) : "";
  const failedPhase = (wf.phases ?? []).find((p) => p.status === "failed");
  const statusIcon = wf.statusRaw === "completed" ? "\u2713" : wf.statusRaw === "failed" ? "\u2717" : "\u2014";

  return (
    <Link to={`/workflows/${wf.id}`} className="flex items-center gap-2 py-1.5 px-2 rounded hover:bg-muted/20 transition-colors">
      <span className={`text-xs w-4 text-center ${wf.statusRaw === "failed" ? "text-destructive" : "text-muted-foreground"}`}>{statusIcon}</span>
      <span className="font-mono text-xs text-muted-foreground">{wf.taskId}</span>
      <span className="text-sm truncate flex-1">{wf.id}</span>
      {failedPhase && <span className="text-xs text-destructive font-mono">{failedPhase.phaseId}</span>}
      {duration && <span className="text-xs text-muted-foreground">{duration}</span>}
      {completedAt && <span className="text-xs text-muted-foreground">{formatTimeAgo(completedAt)}</span>}
    </Link>
  );
}

export function WorkflowsPage() {
  const [result, reexecute] = useQuery({
    query: WorkflowsDocument,
  });
  const [, runWf] = useMutation(RunWorkflowDocument);
  const [, pauseWf] = useMutation(PauseWorkflowDocument);
  const [, resumeWf] = useMutation(ResumeWorkflowDocument);
  const [, cancelWf] = useMutation(CancelWorkflowDocument);
  const [runTaskId, setRunTaskId] = useState("");
  const [showNewForm, setShowNewForm] = useState(false);
  const [feedback, setFeedback] = useState<{ kind: "ok" | "error"; message: string } | null>(null);
  const [recentPage, setRecentPage] = useState(0);

  const { data, fetching, error } = result;
  const workflows = data?.workflows ?? [];

  const counts = useMemo(() => {
    const c = { running: 0, queued: 0, completed: 0, failed: 0, paused: 0, escalated: 0 };
    for (const w of workflows) {
      const s = (w.statusRaw ?? "").toLowerCase();
      if (s === "running") c.running++;
      else if (s === "queued") c.queued++;
      else if (s === "paused") c.paused++;
      else if (s === "completed") c.completed++;
      else if (s === "failed") c.failed++;
      else if (s === "escalated") c.escalated++;
    }
    return c;
  }, [workflows]);

  const activeWorkflows = useMemo(() => workflows.filter((w) => ["running", "paused", "queued"].includes((w.statusRaw ?? "").toLowerCase())), [workflows]);
  const escalatedWorkflows = useMemo(() => workflows.filter((w) => (w.statusRaw ?? "").toLowerCase() === "escalated"), [workflows]);
  const allRecentWorkflows = useMemo(() => workflows.filter((w) => ["completed", "failed", "cancelled"].includes((w.statusRaw ?? "").toLowerCase())), [workflows]);
  const recentTotalPages = Math.max(1, Math.ceil(allRecentWorkflows.length / 10));
  const recentWorkflows = useMemo(() => allRecentWorkflows.slice(recentPage * 10, (recentPage + 1) * 10), [allRecentWorkflows, recentPage]);

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const onRun = async (e: FormEvent) => {
    e.preventDefault();
    if (!runTaskId.trim()) return;
    const { error: err } = await runWf({ taskId: runTaskId.trim() });
    if (err) setFeedback({ kind: "error", message: err.message });
    else {
      setFeedback({ kind: "ok", message: `Workflow started for ${runTaskId}.` });
      setRunTaskId("");
      setShowNewForm(false);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onBatchAction = async (action: "pause" | "cancel") => {
    const fn = action === "pause" ? pauseWf : cancelWf;
    const targets = activeWorkflows.filter((w) => action === "pause" ? w.statusRaw === "running" : !["completed", "failed", "cancelled"].includes(w.statusRaw ?? ""));
    for (const w of targets) {
      await fn({ id: w.id });
    }
    reexecute({ requestPolicy: "network-only" });
  };

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Workflows</h1>
          <p className="text-sm text-muted-foreground">{counts.running} running &middot; {counts.queued} queued</p>
        </div>
        <div className="relative">
          <Button onClick={() => setShowNewForm(!showNewForm)}>New Workflow</Button>
          {showNewForm && (
            <div className="absolute right-0 top-full mt-2 z-10">
              <Card className="border-border/40 bg-card/60 p-3 w-64">
                <form onSubmit={onRun} className="space-y-2">
                  <Input
                    placeholder="Task ID (e.g. TASK-014)"
                    value={runTaskId}
                    onChange={(e) => setRunTaskId(e.target.value)}
                    autoFocus
                  />
                  <Button type="submit" size="sm" className="w-full">Run Workflow</Button>
                </form>
              </Card>
            </div>
          )}
        </div>
      </div>

      {feedback && (
        <Alert variant={feedback.kind === "error" ? "destructive" : "default"} role={feedback.kind === "error" ? "alert" : "status"}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      {(escalatedWorkflows.length > 0) && (
        <Card className="border-amber-500/40 bg-amber-500/5">
          <CardContent className="pt-3 pb-3 px-4">
            <p className="text-xs uppercase tracking-wider text-amber-500/80 font-medium mb-2">Attention Required</p>
            <div className="space-y-1">
              {escalatedWorkflows.map((wf) => (
                <Link key={wf.id} to={`/workflows/${wf.id}`} className="flex items-center gap-2 text-sm hover:underline">
                  <span className="font-mono text-xs text-muted-foreground">{wf.taskId}</span>
                  <span>{wf.id}</span>
                  <Badge variant="outline" className="text-amber-500 border-amber-500/40 text-[10px]">escalated</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      <div className="grid grid-cols-2 md:grid-cols-4 gap-2">
        <StatCard label="Running" value={counts.running} accent />
        <StatCard label="Queued" value={counts.queued} />
        <StatCard label="Completed" value={counts.completed} />
        <StatCard label="Failed" value={counts.failed} />
      </div>

      {activeWorkflows.length > 0 && (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <SectionHeading>Active</SectionHeading>
            <div className="flex gap-1">
              <Button size="sm" variant="outline" onClick={() => onBatchAction("pause")} disabled={counts.running === 0}>Pause All</Button>
              <Button size="sm" variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={() => onBatchAction("cancel")}>Cancel All</Button>
            </div>
          </div>
          <div className="space-y-2">
            {activeWorkflows.map((wf) => <ActiveWorkflowRow key={wf.id} wf={wf} />)}
          </div>
        </div>
      )}

      {recentWorkflows.length > 0 && (
        <div className="space-y-2">
          <SectionHeading>Recent</SectionHeading>
          <div>
            {recentWorkflows.map((wf) => <RecentWorkflowRow key={wf.id} wf={wf} />)}
          </div>
          {allRecentWorkflows.length > 10 && (
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">Page {recentPage + 1} of {recentTotalPages}</span>
              <div className="flex gap-1">
                <Button size="sm" variant="outline" className="h-6" disabled={recentPage === 0} onClick={() => setRecentPage((p) => p - 1)}>Prev</Button>
                <Button size="sm" variant="outline" className="h-6" disabled={recentPage >= recentTotalPages - 1} onClick={() => setRecentPage((p) => p + 1)}>Next</Button>
              </div>
            </div>
          )}
        </div>
      )}

      {workflows.length === 0 && (
        <div className="flex flex-col items-center justify-center py-12 gap-3">
          <p className="text-sm text-muted-foreground/60">No workflows yet</p>
          <Button variant="outline" onClick={() => setShowNewForm(true)}>New Workflow</Button>
        </div>
      )}
    </div>
  );
}

export function WorkflowDetailPage() {
  const { workflowId } = useParams();
  const [result, reexecute] = useQuery({ query: WorkflowDetailDocument, variables: { id: workflowId! } });
  const [, pauseWf] = useMutation(PauseWorkflowDocument);
  const [, resumeWf] = useMutation(ResumeWorkflowDocument);
  const [, cancelWf] = useMutation(CancelWorkflowDocument);
  const [, approvePhase] = useMutation(ApprovePhaseDocument);
  const [wfMessage, setWfMessage] = useState<string | null>(null);
  const [wfOperating, setWfOperating] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [approveTarget, setApproveTarget] = useState<string | null>(null);
  const [approveNote, setApproveNote] = useState("");
  const [escalationFeedback, setEscalationFeedback] = useState("");
  const [expandedDecisions, setExpandedDecisions] = useState<Set<string>>(new Set());

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
  const isFailed = wf.statusRaw === "failed";
  const isEscalated = wf.statusRaw === "escalated";
  const isTerminal = ["completed", "failed", "cancelled"].includes(wf.statusRaw ?? "");

  const toggleDecision = (phaseId: string) => {
    setExpandedDecisions((prev) => {
      const next = new Set(prev);
      if (next.has(phaseId)) next.delete(phaseId); else next.add(phaseId);
      return next;
    });
  };

  const phaseDecisions = (phaseId: string) => decisions.filter((d) => d.phaseId === phaseId);

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row items-start justify-between gap-3">
        <div>
          <p className="text-sm text-muted-foreground font-mono break-all">{wf.id}</p>
          <h1 className="text-2xl font-semibold tracking-tight">
            Workflow for <Link to={`/tasks/${wf.taskId}`} className="underline">{wf.taskId}</Link>
          </h1>
          <div className="flex gap-2 mt-2">
            <Badge variant={statusColor(wf.statusRaw ?? "")}>{wf.statusRaw}</Badge>
            {wf.workflowRef && <Badge variant="outline">{wf.workflowRef}</Badge>}
            {(wf.totalReworks ?? 0) > 0 && <Badge variant="outline">{wf.totalReworks} reworks</Badge>}
          </div>
        </div>
        {!isTerminal && !isEscalated && (
          <div className="flex items-center gap-2 flex-wrap">
            {isRunning && (
              <>
                <Button variant="secondary" disabled={wfOperating} onClick={() => wfAction("Pause", () => pauseWf({ id: workflowId! }))}>
                  Pause
                </Button>
                {confirmCancel ? (
                  <>
                    <Button variant="destructive" disabled={wfOperating} onClick={() => { setConfirmCancel(false); wfAction("Cancel", () => cancelWf({ id: workflowId! })); }}>
                      Confirm Cancel
                    </Button>
                    <Button variant="outline" onClick={() => setConfirmCancel(false)}>Back</Button>
                  </>
                ) : (
                  <Button variant="ghost" className="text-destructive/60 hover:text-destructive" disabled={wfOperating} onClick={() => setConfirmCancel(true)}>
                    Cancel
                  </Button>
                )}
              </>
            )}
            {isPaused && (
              <>
                <Button variant="secondary" disabled={wfOperating} onClick={() => wfAction("Resume", () => resumeWf({ id: workflowId! }))}>
                  Resume
                </Button>
                {confirmCancel ? (
                  <>
                    <Button variant="destructive" disabled={wfOperating} onClick={() => { setConfirmCancel(false); wfAction("Cancel", () => cancelWf({ id: workflowId! })); }}>
                      Confirm Cancel
                    </Button>
                    <Button variant="outline" onClick={() => setConfirmCancel(false)}>Back</Button>
                  </>
                ) : (
                  <Button variant="ghost" className="text-destructive/60 hover:text-destructive" disabled={wfOperating} onClick={() => setConfirmCancel(true)}>
                    Cancel
                  </Button>
                )}
              </>
            )}
            {isFailed && (
              <>
                <Button variant="secondary" disabled={wfOperating} onClick={() => wfAction("Retry", () => resumeWf({ id: workflowId! }))}>
                  Retry
                </Button>
                {confirmCancel ? (
                  <>
                    <Button variant="destructive" disabled={wfOperating} onClick={() => { setConfirmCancel(false); wfAction("Cancel", () => cancelWf({ id: workflowId! })); }}>
                      Confirm Cancel
                    </Button>
                    <Button variant="outline" onClick={() => setConfirmCancel(false)}>Back</Button>
                  </>
                ) : (
                  <Button variant="ghost" className="text-destructive/60 hover:text-destructive" disabled={wfOperating} onClick={() => setConfirmCancel(true)}>
                    Cancel
                  </Button>
                )}
              </>
            )}
          </div>
        )}
      </div>

      {isEscalated && (
        <Card className="border-amber-500/40 bg-amber-500/5">
          <CardContent className="pt-3 pb-3 px-4 space-y-3">
            <p className="text-xs uppercase tracking-wider text-amber-500/80 font-medium">Escalated</p>
            <Textarea
              value={escalationFeedback}
              onChange={(e) => setEscalationFeedback(e.target.value)}
              placeholder="Provide feedback or instructions..."
              rows={3}
            />
            <div className="flex gap-2">
              <Button size="sm" disabled={wfOperating} onClick={() => wfAction("Resume", () => resumeWf({ id: workflowId!, feedback: escalationFeedback || null }))}>
                Resume
              </Button>
              <Button size="sm" variant="outline" disabled={wfOperating} onClick={() => wfAction("Skip", () => approvePhase({ workflowId: workflowId!, phaseId: wf.currentPhase ?? "", note: escalationFeedback || null }))}>
                Skip
              </Button>
              <Button size="sm" variant="ghost" className="text-destructive/60 hover:text-destructive" disabled={wfOperating} onClick={() => wfAction("Cancel", () => cancelWf({ id: workflowId! }))}>
                Cancel
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {wfMessage && (
        <Alert variant={wfMessage.startsWith("Error") ? "destructive" : "default"} role={wfMessage.startsWith("Error") ? "alert" : "status"}>
          <AlertDescription>{wfMessage}</AlertDescription>
        </Alert>
      )}

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Phase Timeline</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3">
          <div className="space-y-2">
            {(wf.phases ?? []).map((p, i) => {
              const pDecisions = phaseDecisions(p.phaseId);
              const isExpanded = expandedDecisions.has(p.phaseId);
              const phaseDuration = p.startedAt && p.completedAt ? formatDuration(new Date(p.completedAt).getTime() - new Date(p.startedAt).getTime()) : null;
              const needsApproval = wf.currentPhase === p.phaseId && !isTerminal && p.status !== "completed" && p.status !== "running" && p.status !== "failed";

              return (
                <div key={p.phaseId}>
                  <div className="flex items-start gap-3">
                    <div className="flex flex-col items-center">
                      <div className={`h-3 w-3 rounded-full ${
                        p.status === "completed" ? "bg-[var(--ao-success)]" :
                        p.status === "running" ? "bg-[var(--ao-running)] animate-pulse" :
                        p.status === "failed" ? "bg-destructive" :
                        "bg-muted-foreground/30"
                      }`} />
                      {i < (wf.phases ?? []).length - 1 && <div className="w-px h-6 bg-border" />}
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-mono text-sm">{p.phaseId}</span>
                        <Badge variant={statusColor(p.status ?? "")} className="text-[10px]">{p.status}</Badge>
                        {(p.attempt ?? 0) > 1 && <Badge variant="outline" className="text-[10px]">attempt {p.attempt}</Badge>}
                        {phaseDuration && <span className="text-xs text-muted-foreground">{phaseDuration}</span>}
                        {pDecisions.length > 0 && (
                          <button type="button" onClick={() => toggleDecision(p.phaseId)} className="text-xs text-muted-foreground hover:text-foreground">
                            {isExpanded ? "\u25BC" : "\u25B6"} {pDecisions.length} decision{pDecisions.length > 1 ? "s" : ""}
                          </button>
                        )}
                      </div>
                      {p.errorMessage && (
                        /[\n#*`\-|]/.test(p.errorMessage)
                          ? <div className="mt-0.5 text-destructive"><Markdown content={p.errorMessage} /></div>
                          : <p className="text-xs text-destructive mt-0.5">{p.errorMessage}</p>
                      )}
                      {(p.startedAt || p.completedAt) && (
                        <p className="text-xs text-muted-foreground">
                          {p.startedAt && <>Started: {p.startedAt}</>}
                          {p.completedAt && <> &middot; Completed: {p.completedAt}</>}
                        </p>
                      )}
                    </div>
                  </div>

                  {isExpanded && pDecisions.length > 0 && (
                    <div className="ml-6 mt-1 space-y-1">
                      {pDecisions.map((d, di) => (
                        <Card key={di} className="border-border/30 bg-card/40 p-2">
                          <div className="flex items-center gap-2 text-xs">
                            <span className="font-medium">{d.decision}</span>
                            {d.targetPhase && <span className="font-mono text-muted-foreground">&rarr; {d.targetPhase}</span>}
                            {d.confidence != null && <span className="text-muted-foreground">{((d.confidence) * 100).toFixed(0)}%</span>}
                            {d.risk != null && <span className="text-amber-500/80">risk: {d.risk}</span>}
                          </div>
                          {d.reason && <p className="text-[10px] text-muted-foreground/60 mt-0.5">{d.reason}</p>}
                        </Card>
                      ))}
                    </div>
                  )}

                  {needsApproval && (
                    <div className="ml-6 mt-2">
                      <Card className="border-amber-500/40 bg-amber-500/5 p-3">
                        <p className="text-xs uppercase tracking-wider text-amber-500/80 font-medium mb-2">Phase Approval Required</p>
                        {approveTarget === p.phaseId ? (
                          <div className="space-y-2">
                            <Input
                              value={approveNote}
                              onChange={(e) => setApproveNote(e.target.value)}
                              placeholder="Approval note (optional)..."
                              className="h-7 text-xs"
                            />
                            <div className="flex gap-2">
                              <Button
                                size="sm"
                                disabled={wfOperating}
                                onClick={() => {
                                  setApproveTarget(null);
                                  wfAction("Phase approval", () => approvePhase({ workflowId: workflowId!, phaseId: p.phaseId, note: approveNote || null }));
                                  setApproveNote("");
                                }}
                              >
                                Approve
                              </Button>
                              <Button size="sm" variant="outline" onClick={() => { setApproveTarget(null); setApproveNote(""); }}>
                                Reject
                              </Button>
                            </div>
                          </div>
                        ) : (
                          <Button size="sm" variant="outline" disabled={wfOperating} onClick={() => setApproveTarget(p.phaseId)}>
                            Review Phase
                          </Button>
                        )}
                      </Card>
                    </div>
                  )}

                  {wf.currentPhase === p.phaseId && !isTerminal && (p.status === "running" || p.status === "completed") && p.status !== "completed" && (
                    approveTarget === p.phaseId ? (
                      <div className="ml-6 mt-1 flex items-center gap-2">
                        <Input
                          value={approveNote}
                          onChange={(e) => setApproveNote(e.target.value)}
                          placeholder="Approval note (optional)..."
                          className="h-7 text-xs max-w-xs"
                        />
                        <Button
                          size="sm"
                          disabled={wfOperating}
                          onClick={() => {
                            setApproveTarget(null);
                            wfAction("Phase approval", () => approvePhase({ workflowId: workflowId!, phaseId: p.phaseId, note: approveNote || null }));
                            setApproveNote("");
                          }}
                        >
                          Confirm Approve
                        </Button>
                        <Button size="sm" variant="outline" onClick={() => { setApproveTarget(null); setApproveNote(""); }}>
                          Cancel
                        </Button>
                      </div>
                    ) : (
                      <Button size="sm" variant="outline" className="ml-6 mt-1" disabled={wfOperating} onClick={() => setApproveTarget(p.phaseId)}>
                        Approve Phase
                      </Button>
                    )
                  )}
                </div>
              );
            })}
          </div>
        </CardContent>
      </Card>

      <PhaseOutputPanel workflowId={workflowId!} currentPhase={wf.currentPhase} isRunning={isRunning} />

      {checkpoints.length > 0 && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Checkpoints</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            <ul className="space-y-2">
              {checkpoints.map((cp) => (
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
    query: WorkflowDetailDocument,
    variables: { id: workflowId! },
  });

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const checkpoints = data?.workflowCheckpoints ?? [];
  const cp = checkpoints.find((c) => c.id === checkpoint);

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
