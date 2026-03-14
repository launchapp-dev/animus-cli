import { Link } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { toast } from "sonner";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  DaemonDocument,
  DaemonStartDocument,
  DaemonStopDocument,
  DaemonPauseDocument,
  DaemonResumeDocument,
  DaemonClearLogsDocument,
} from "@/lib/graphql/generated/graphql";
import { statusColor, StatusDot, PageLoading, PageError, SectionHeading } from "./shared";

export function DaemonPage() {
  const [result, reexecute] = useQuery({ query: DaemonDocument });
  const [, startMut] = useMutation(DaemonStartDocument);
  const [, stopMut] = useMutation(DaemonStopDocument);
  const [, pauseMut] = useMutation(DaemonPauseDocument);
  const [, resumeMut] = useMutation(DaemonResumeDocument);
  const [, clearLogsMut] = useMutation(DaemonClearLogsDocument);
  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const status = data?.daemonStatus;
  const health = data?.daemonHealth;
  const agents = data?.agentRuns ?? [];
  const logs = data?.daemonLogs ?? [];

  const runAction = async (label: string, fn: () => Promise<any>) => {
    const { error: err } = await fn();
    if (err) toast.error(err.message);
    else {
      toast.success(`${label} successful.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Daemon</h1>
          <StatusDot status={status?.healthy ? "healthy" : "error"} />
        </div>
        <div className="flex items-center gap-2">
          <Button size="sm" onClick={() => runAction("Start", () => startMut({}))}>Start</Button>
          <Button size="sm" variant="secondary" onClick={() => runAction("Resume", () => resumeMut({}))}>Resume</Button>
          <Button size="sm" variant="outline" onClick={() => runAction("Pause", () => pauseMut({}))}>Pause</Button>
          <Button size="sm" variant="destructive" onClick={() => runAction("Stop", () => stopMut({}))}>Stop</Button>
        </div>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Status</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3 space-y-2">
          <div className="flex items-center gap-2">
            <Badge variant={status?.healthy ? "default" : "destructive"}>{status?.statusRaw ?? "unknown"}</Badge>
            {status?.runnerConnected && <Badge variant="outline" className="text-[10px] h-4 px-1.5 border-primary/20 text-primary/70">runner</Badge>}
          </div>
          <div className="flex gap-4 text-xs text-muted-foreground">
            <span>Agents: <span className="font-mono text-foreground/70">{status?.activeAgents ?? 0}{status?.maxAgents ? ` / ${status.maxAgents}` : ""}</span></span>
            {status?.projectRoot && <span className="truncate">Root: <span className="font-mono text-foreground/70">{status.projectRoot}</span></span>}
          </div>
        </CardContent>
      </Card>

      {agents.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <SectionHeading>Active Agents</SectionHeading>
            <Badge variant="outline" className="text-[10px] h-4 px-1.5 font-mono border-primary/20 text-primary/70">{agents.length}</Badge>
          </div>
          <Card className="border-border/40 bg-card/60 overflow-hidden">
            <CardContent className="px-0 pb-0 pt-0">
              <Table>
                <TableHeader>
                  <TableRow className="border-border/30 hover:bg-transparent">
                    <TableHead className="text-[10px] uppercase tracking-wider h-7">Run</TableHead>
                    <TableHead className="text-[10px] uppercase tracking-wider h-7">Task</TableHead>
                    <TableHead className="text-[10px] uppercase tracking-wider h-7">Phase</TableHead>
                    <TableHead className="text-[10px] uppercase tracking-wider h-7">Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {agents.map((a) => (
                    <TableRow key={a.runId} className="border-border/20 hover:bg-accent/30">
                      <TableCell className="font-mono text-[11px] text-muted-foreground py-2">{a.runId}</TableCell>
                      <TableCell className="py-2">
                        {a.taskId ? (
                          <Link to={`/tasks/${a.taskId}`} className="text-primary/80 hover:text-primary text-xs transition-colors">
                            {a.taskTitle ?? a.taskId}
                          </Link>
                        ) : (
                          <span className="text-muted-foreground/40">-</span>
                        )}
                      </TableCell>
                      <TableCell className="font-mono text-[11px] text-muted-foreground py-2">{a.phaseId ?? "-"}</TableCell>
                      <TableCell className="py-2">
                        <div className="flex items-center gap-1.5">
                          <StatusDot status={a.status} />
                          <span className="text-[11px]">{a.status}</span>
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </div>
      )}

      {logs.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <SectionHeading>Logs</SectionHeading>
            <Button size="sm" variant="ghost" className="h-6 text-[10px] text-muted-foreground" onClick={() => runAction("Clear Logs", () => clearLogsMut({}))}>Clear</Button>
          </div>
          <Card className="border-border/40 bg-card/60">
            <CardContent className="pt-3 pb-3 px-4">
              <div className="max-h-80 overflow-y-auto font-mono text-xs space-y-0.5">
                {logs.map((log, i) => (
                  <div key={i} className="flex gap-2">
                    <span className="text-muted-foreground/50 shrink-0 text-[10px]">{log.timestamp ?? ""}</span>
                    <span className={log.level === "ERROR" ? "text-destructive" : "text-foreground/70"}>{log.message ?? ""}</span>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}
