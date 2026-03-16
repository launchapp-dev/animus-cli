import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DaemonDocument,
  WorkflowsDocument,
  GqlWorkflowStatus,
} from "@/lib/graphql/generated/graphql";
import { StatCard, PageLoading, PageError } from "./shared";

type ErrorEntry = {
  id: string;
  severity: "ERROR";
  timestamp: string;
  message: string;
  source: "daemon" | "workflow";
  workflowId?: string;
  taskId?: string;
  phaseId?: string;
  fields?: string;
};

type ErrorGroup = {
  type: string;
  count: number;
  lastSeen: string;
  entries: ErrorEntry[];
};

function timeAgo(ts: string): string {
  const diff = Date.now() - new Date(ts).getTime();
  if (Number.isNaN(diff) || diff < 0) return "just now";
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

function extractErrorType(message: string): string {
  const firstLine = message.split("\n")[0].trim();
  const colonIdx = firstLine.indexOf(":");
  if (colonIdx > 0 && colonIdx < 60) {
    return firstLine.slice(0, colonIdx).trim();
  }
  return firstLine.length > 60 ? firstLine.slice(0, 60) + "…" : firstLine;
}

export function ErrorBrowserPage() {
  const [daemonResult] = useQuery({ query: DaemonDocument });
  const [workflowResult] = useQuery({ query: WorkflowsDocument, variables: {} });
  const [sourceFilter, setSourceFilter] = useState<"all" | "daemon" | "workflow">("all");
  const [viewMode, setViewMode] = useState<"list" | "grouped">("list");
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const fetching = daemonResult.fetching || workflowResult.fetching;
  const error = daemonResult.error || workflowResult.error;

  const errors = useMemo<ErrorEntry[]>(() => {
    const entries: ErrorEntry[] = [];

    const logs = daemonResult.data?.daemonLogs ?? [];
    for (const log of logs) {
      if (log.level !== "ERROR") continue;
      entries.push({
        id: `daemon-${log.timestamp}-${log.message?.slice(0, 20)}`,
        severity: "ERROR",
        timestamp: log.timestamp ?? "",
        message: log.message ?? "",
        source: "daemon",
        fields: log.fields ?? undefined,
      });
    }

    const workflows = workflowResult.data?.workflows ?? [];
    for (const wf of workflows) {
      if (wf.status !== GqlWorkflowStatus.Failed) continue;
      for (const phase of wf.phases) {
        if (phase.errorMessage) {
          entries.push({
            id: `wf-${wf.id}-${phase.phaseId}-${phase.attempt}`,
            severity: "ERROR",
            timestamp: phase.completedAt ?? phase.startedAt ?? "",
            message: phase.errorMessage,
            source: "workflow",
            workflowId: wf.id,
            taskId: wf.taskId,
            phaseId: phase.phaseId,
          });
        }
      }
    }

    entries.sort((a, b) => {
      const ta = new Date(a.timestamp).getTime() || 0;
      const tb = new Date(b.timestamp).getTime() || 0;
      return tb - ta;
    });

    return entries;
  }, [daemonResult.data, workflowResult.data]);

  const filtered = useMemo(
    () => sourceFilter === "all" ? errors : errors.filter((e) => e.source === sourceFilter),
    [errors, sourceFilter],
  );

  const grouped = useMemo<ErrorGroup[]>(() => {
    const map = new Map<string, ErrorGroup>();
    for (const entry of filtered) {
      const type = extractErrorType(entry.message);
      const existing = map.get(type);
      if (existing) {
        existing.count++;
        existing.entries.push(entry);
        if ((new Date(entry.timestamp).getTime() || 0) > (new Date(existing.lastSeen).getTime() || 0)) {
          existing.lastSeen = entry.timestamp;
        }
      } else {
        map.set(type, { type, count: 1, lastSeen: entry.timestamp, entries: [entry] });
      }
    }
    return Array.from(map.values()).sort((a, b) => b.count - a.count || (new Date(b.lastSeen).getTime() || 0) - (new Date(a.lastSeen).getTime() || 0));
  }, [filtered]);

  const workflowPhaseFailureCount = useMemo(
    () => errors.filter((e) => e.source === "workflow").length,
    [errors],
  );

  const failedWorkflowCount = useMemo(() => {
    const ids = new Set(errors.filter((e) => e.source === "workflow").map((e) => e.workflowId));
    return ids.size;
  }, [errors]);

  const lastErrorTime = errors.length > 0 ? timeAgo(errors[0].timestamp) : "-";

  const toggleExpanded = (id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Errors</h1>
        <p className="text-sm text-muted-foreground/60 mt-1">
          {errors.length} {errors.length === 1 ? "error" : "errors"} detected
        </p>
      </div>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatCard label="Total Errors" value={errors.length} accent={errors.length > 0} />
        <StatCard label="Workflow Failures" value={failedWorkflowCount} />
        <StatCard label="Phase Failures" value={workflowPhaseFailureCount} />
        <StatCard label="Last Error" value={lastErrorTime} />
      </div>

      <div className="flex items-center gap-2 flex-wrap">
        <div className="flex items-center gap-2">
          {(["all", "daemon", "workflow"] as const).map((f) => (
            <Button
              key={f}
              size="sm"
              variant={sourceFilter === f ? "default" : "outline"}
              className="text-xs capitalize"
              onClick={() => setSourceFilter(f)}
            >
              {f === "all" ? "All Sources" : f === "daemon" ? "Daemon Logs" : "Workflow Errors"}
            </Button>
          ))}
        </div>
        <div className="flex items-center gap-2 ml-auto">
          {(["list", "grouped"] as const).map((v) => (
            <Button
              key={v}
              size="sm"
              variant={viewMode === v ? "default" : "outline"}
              className="text-xs capitalize"
              onClick={() => setViewMode(v)}
            >
              {v === "list" ? "List" : "Group by Type"}
            </Button>
          ))}
        </div>
      </div>

      {filtered.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No errors found.</p>
      ) : viewMode === "grouped" ? (
        <div className="space-y-3">
          {grouped.map((group) => (
            <Card key={group.type} className="border-border/40 bg-card/60">
              <CardHeader className="pb-2 pt-3 px-4">
                <div className="flex items-center gap-2">
                  <Badge variant="destructive" className="text-[10px] shrink-0">ERROR</Badge>
                  <span className="text-sm font-medium text-foreground/90 flex-1 min-w-0 truncate">
                    {group.type}
                  </span>
                  <Badge variant="secondary" className="text-[10px] shrink-0 tabular-nums">
                    {group.count}×
                  </Badge>
                  <span className="text-[10px] font-mono text-muted-foreground/40 shrink-0">
                    {timeAgo(group.lastSeen)}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-5 text-[10px] text-muted-foreground/50 px-1 shrink-0"
                    onClick={() => toggleExpanded(group.type)}
                  >
                    {expandedIds.has(group.type) ? "Collapse" : "Expand"}
                  </Button>
                </div>
              </CardHeader>
              {expandedIds.has(group.type) && (
                <CardContent className="pt-0 pb-3 px-4 space-y-2">
                  {group.entries.map((entry) => (
                    <ErrorEntryRow key={entry.id} entry={entry} expandedIds={expandedIds} onToggle={toggleExpanded} />
                  ))}
                </CardContent>
              )}
            </Card>
          ))}
        </div>
      ) : (
        <div className="space-y-2">
          {filtered.map((entry) => (
            <Card key={entry.id} className="border-border/40 bg-card/60">
              <CardContent className="pt-3 pb-3 px-4">
                <ErrorEntryRow entry={entry} expandedIds={expandedIds} onToggle={toggleExpanded} />
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}

function ErrorEntryRow({
  entry,
  expandedIds,
  onToggle,
}: {
  entry: ErrorEntry;
  expandedIds: Set<string>;
  onToggle: (id: string) => void;
}) {
  const hasDetails = Boolean(entry.fields);
  const stackLines = entry.message.includes("\n") ? entry.message.split("\n").slice(1).join("\n").trim() : null;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-2">
        <Badge variant="outline" className="text-[10px] shrink-0 font-mono border-border/30 text-muted-foreground/60">
          {entry.source}
        </Badge>
        {entry.phaseId && (
          <Badge variant="outline" className="text-[10px] shrink-0 font-mono border-border/20 text-muted-foreground/50">
            {entry.phaseId}
          </Badge>
        )}
        <span className="text-[10px] font-mono text-muted-foreground/40 ml-auto shrink-0">
          {timeAgo(entry.timestamp)}
        </span>
      </div>

      <p className="text-sm text-foreground/80">{entry.message.split("\n")[0]}</p>

      {entry.source === "workflow" && (
        <div className="flex items-center gap-3 text-[11px]">
          {entry.workflowId && (
            <Link
              to={`/workflows/${entry.workflowId}`}
              className="text-primary/80 hover:text-primary transition-colors font-mono"
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

      {(stackLines || hasDetails) && (
        <div>
          <Button
            size="sm"
            variant="ghost"
            className="h-5 text-[10px] text-muted-foreground/50 px-1"
            onClick={() => onToggle(entry.id)}
          >
            {expandedIds.has(entry.id) ? "Hide details" : "Show details"}
          </Button>
          {expandedIds.has(entry.id) && (
            <div className="space-y-1 mt-1">
              {stackLines && (
                <pre className="border border-border/30 bg-background/50 rounded-md p-3 font-mono text-[11px] text-foreground/60 overflow-x-auto whitespace-pre-wrap">
                  {stackLines}
                </pre>
              )}
              {hasDetails && (
                <pre className="border border-border/30 bg-background/50 rounded-md p-3 font-mono text-[11px] text-foreground/60 overflow-x-auto">
                  {(() => {
                    try { return JSON.stringify(JSON.parse(entry.fields!), null, 2); } catch { return entry.fields; }
                  })()}
                </pre>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
