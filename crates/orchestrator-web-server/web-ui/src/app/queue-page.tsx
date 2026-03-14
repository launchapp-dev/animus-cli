import { Link } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { toast } from "sonner";
import { Card } from "@/components/ui/card";
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
  QueueDocument,
  QueueHoldDocument,
  QueueReleaseDocument,
  QueueReorderDocument,
} from "@/lib/graphql/generated/graphql";
import { statusColor, priorityColor, PageLoading, PageError, StatCard } from "./shared";

export function QueuePage() {
  const [result, reexecute] = useQuery({ query: QueueDocument });
  const [, holdMut] = useMutation(QueueHoldDocument);
  const [, releaseMut] = useMutation(QueueReleaseDocument);
  const [, reorderMut] = useMutation(QueueReorderDocument);
  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const entries = data?.queue ?? [];
  const stats = data?.queueStats;

  const onHold = async (taskId: string) => {
    const { error: err } = await holdMut({ taskId });
    if (err) toast.error(err.message);
    else {
      toast.success(`Held ${taskId}.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onRelease = async (taskId: string) => {
    const { error: err } = await releaseMut({ taskId });
    if (err) toast.error(err.message);
    else {
      toast.success(`Released ${taskId}.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const moveEntry = async (index: number, direction: -1 | 1) => {
    const ids = entries.map((e) => e.taskId);
    const newIndex = index + direction;
    if (newIndex < 0 || newIndex >= ids.length) return;
    [ids[index], ids[newIndex]] = [ids[newIndex], ids[index]];
    const { error: err } = await reorderMut({ taskIds: ids });
    if (err) toast.error(err.message);
    else reexecute({ requestPolicy: "network-only" });
  };

  const sortByPriority = async () => {
    const priorityOrder: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3 };
    const sorted = [...entries].sort((a, b) => (priorityOrder[a.priority ?? ""] ?? 9) - (priorityOrder[b.priority ?? ""] ?? 9));
    const { error: err } = await reorderMut({ taskIds: sorted.map((e) => e.taskId) });
    if (err) toast.error(err.message);
    else {
      toast.success("Queue reordered by priority.");
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
              {entries.map((entry, i) => (
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
