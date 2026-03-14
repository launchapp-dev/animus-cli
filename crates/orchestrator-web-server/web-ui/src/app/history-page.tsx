import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@/lib/graphql/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DaemonDocument,
  WorkflowsDocument,
  GqlWorkflowStatus,
} from "@/lib/graphql/generated/graphql";
import { statusColor, PageLoading, PageError } from "./shared";

type TimeRange = "24h" | "7d" | "30d" | "all";
type StatusFilter = "all" | "completed" | "failed" | "running";

type HistoryEntry = {
  id: string;
  timestamp: string;
  type: "workflow" | "daemon";
  status: string;
  description: string;
  workflowId?: string;
  taskId?: string;
};

const PAGE_SIZE = 10;

function timeMs(range: TimeRange): number {
  if (range === "24h") return 24 * 60 * 60_000;
  if (range === "7d") return 7 * 24 * 60 * 60_000;
  if (range === "30d") return 30 * 24 * 60 * 60_000;
  return Infinity;
}

function formatTimestamp(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function workflowStatusToFilter(status: GqlWorkflowStatus): StatusFilter {
  if (status === GqlWorkflowStatus.Completed) return "completed";
  if (status === GqlWorkflowStatus.Failed || status === GqlWorkflowStatus.Cancelled) return "failed";
  if (status === GqlWorkflowStatus.Running) return "running";
  return "all";
}

export function HistoryPage() {
  const [daemonResult] = useQuery({ query: DaemonDocument });
  const [workflowResult] = useQuery({ query: WorkflowsDocument, variables: {} });
  const [timeRange, setTimeRange] = useState<TimeRange>("7d");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [page, setPage] = useState(0);

  const fetching = daemonResult.fetching || workflowResult.fetching;
  const error = daemonResult.error || workflowResult.error;

  const entries = useMemo<HistoryEntry[]>(() => {
    const items: HistoryEntry[] = [];
    const cutoff = timeRange === "all" ? 0 : Date.now() - timeMs(timeRange);

    const logs = daemonResult.data?.daemonLogs ?? [];
    for (const log of logs) {
      const ts = log.timestamp ?? "";
      if (new Date(ts).getTime() < cutoff) continue;
      items.push({
        id: `daemon-${ts}-${log.message?.slice(0, 20)}`,
        timestamp: ts,
        type: "daemon",
        status: log.level ?? "INFO",
        description: log.message ?? "",
      });
    }

    const workflows = workflowResult.data?.workflows ?? [];
    for (const wf of workflows) {
      const lastPhase = wf.phases.length > 0
        ? wf.phases.reduce((a, b) => ((b.completedAt ?? b.startedAt ?? "") > (a.completedAt ?? a.startedAt ?? "") ? b : a))
        : null;
      const ts = lastPhase?.completedAt ?? lastPhase?.startedAt ?? "";
      if (ts && new Date(ts).getTime() < cutoff) continue;

      items.push({
        id: `wf-${wf.id}`,
        timestamp: ts,
        type: "workflow",
        status: wf.statusRaw ?? wf.status,
        description: `Workflow ${wf.id}${wf.currentPhase ? ` — phase ${wf.currentPhase}` : ""}`,
        workflowId: wf.id,
        taskId: wf.taskId,
      });
    }

    items.sort((a, b) => {
      const ta = new Date(a.timestamp).getTime() || 0;
      const tb = new Date(b.timestamp).getTime() || 0;
      return tb - ta;
    });

    return items;
  }, [daemonResult.data, workflowResult.data, timeRange]);

  const filtered = useMemo(() => {
    if (statusFilter === "all") return entries;
    return entries.filter((e) => {
      if (e.type === "workflow") {
        const wfStatus = workflowStatusToFilter(e.status as GqlWorkflowStatus);
        return wfStatus === statusFilter;
      }
      if (statusFilter === "failed") return e.status === "ERROR";
      return false;
    });
  }, [entries, statusFilter]);

  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const pageEntries = filtered.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">History</h1>
        <p className="text-sm text-muted-foreground/60 mt-1">
          Execution history and past agent runs
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-4">
        <div className="flex items-center gap-1.5">
          <span className="text-[11px] text-muted-foreground/50 uppercase tracking-wider mr-1">Range</span>
          {(["24h", "7d", "30d", "all"] as const).map((r) => (
            <Button
              key={r}
              size="sm"
              variant="ghost"
              className={`text-xs h-7 px-2.5 ${
                timeRange === r
                  ? "bg-primary text-primary-foreground hover:bg-primary/90 hover:text-primary-foreground"
                  : "bg-accent/50 text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => { setTimeRange(r); setPage(0); }}
            >
              {r === "all" ? "All" : `Last ${r}`}
            </Button>
          ))}
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[11px] text-muted-foreground/50 uppercase tracking-wider mr-1">Status</span>
          {(["all", "completed", "failed", "running"] as const).map((s) => (
            <Button
              key={s}
              size="sm"
              variant="ghost"
              className={`text-xs h-7 px-2.5 capitalize ${
                statusFilter === s
                  ? "bg-primary text-primary-foreground hover:bg-primary/90 hover:text-primary-foreground"
                  : "bg-accent/50 text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => { setStatusFilter(s); setPage(0); }}
            >
              {s}
            </Button>
          ))}
        </div>
      </div>

      <p className="text-[11px] text-muted-foreground/40">
        {filtered.length} {filtered.length === 1 ? "entry" : "entries"}
      </p>

      {pageEntries.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No history entries found.</p>
      ) : (
        <div className="relative pl-5">
          <div className="absolute left-[7px] top-2 bottom-2 w-px bg-border/40" />

          <div className="space-y-1">
            {pageEntries.map((entry) => (
              <div key={entry.id} className="relative">
                <div className="absolute left-[-17px] top-3.5 h-2.5 w-2.5 rounded-full border-2 border-border/60 bg-background" />

                <Card className="border-border/40 bg-card/60 ml-1">
                  <CardContent className="pt-3 pb-3 px-4 space-y-1.5">
                    <div className="flex items-center gap-2 flex-wrap">
                      <Badge
                        variant={entry.type === "workflow" ? statusColor(entry.status) : entry.status === "ERROR" ? "destructive" : "outline"}
                        className="text-[10px] shrink-0"
                      >
                        {entry.type === "workflow" ? entry.status : entry.status}
                      </Badge>
                      <Badge variant="outline" className="text-[10px] shrink-0 font-mono border-border/30 text-muted-foreground/60">
                        {entry.type}
                      </Badge>
                      <span className="text-[10px] font-mono text-muted-foreground/40 ml-auto shrink-0">
                        {formatTimestamp(entry.timestamp)}
                      </span>
                    </div>

                    <p className="text-sm text-foreground/80">{entry.description}</p>

                    {(entry.workflowId || entry.taskId) && (
                      <div className="flex items-center gap-3 text-[11px]">
                        {entry.workflowId && (
                          <Link
                            to={`/workflows/${entry.workflowId}`}
                            className="text-primary/80 hover:text-primary transition-colors"
                          >
                            {entry.workflowId}
                          </Link>
                        )}
                        {entry.taskId && (
                          <Link
                            to={`/tasks/${entry.taskId}`}
                            className="text-primary/80 hover:text-primary transition-colors"
                          >
                            {entry.taskId}
                          </Link>
                        )}
                      </div>
                    )}
                  </CardContent>
                </Card>
              </div>
            ))}
          </div>
        </div>
      )}

      {totalPages > 1 && (
        <div className="flex items-center justify-center gap-2">
          <Button
            size="sm"
            variant="outline"
            className="text-xs"
            disabled={page === 0}
            onClick={() => setPage(page - 1)}
          >
            Previous
          </Button>
          <span className="text-[11px] text-muted-foreground/50 font-mono">
            {page + 1} / {totalPages}
          </span>
          <Button
            size="sm"
            variant="outline"
            className="text-xs"
            disabled={page >= totalPages - 1}
            onClick={() => setPage(page + 1)}
          >
            Next
          </Button>
        </div>
      )}
    </div>
  );
}
