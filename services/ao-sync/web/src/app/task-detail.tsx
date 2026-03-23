import { useParams, Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, Clock, User, GitBranch, CheckCircle2, Circle } from "lucide-react";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Markdown } from "@/components/markdown";

function statusVariant(status: string) {
  if (["done", "implemented", "approved"].includes(status)) return "success" as const;
  if (["in-progress"].includes(status)) return "default" as const;
  if (["blocked", "cancelled", "deprecated"].includes(status)) return "destructive" as const;
  if (["on-hold", "needs-rework"].includes(status)) return "warning" as const;
  return "secondary" as const;
}

function priorityVariant(priority: string) {
  if (priority === "critical") return "destructive" as const;
  if (priority === "high") return "warning" as const;
  return "secondary" as const;
}

export function TaskDetailPage() {
  const { projectId, taskId } = useParams<{ projectId: string; taskId: string }>();

  const { data, isLoading } = useQuery({
    queryKey: ["task", projectId, taskId],
    queryFn: () => api.tasks.get(projectId!, taskId!),
    enabled: !!projectId && !!taskId,
  });

  if (isLoading) {
    return <div className="text-muted-foreground py-8 text-center">Loading...</div>;
  }

  const task = data?.task;
  if (!task) {
    return <div className="text-muted-foreground py-8 text-center">Task not found</div>;
  }

  const assigneeLabel = task.assignee.type === "human"
    ? `Human: ${task.assignee.user_id}`
    : task.assignee.type === "agent"
    ? `Agent: ${task.assignee.role}${task.assignee.model ? ` (${task.assignee.model})` : ""}`
    : "Unassigned";

  return (
    <div className="space-y-6 max-w-4xl">
      <Link to={`/projects/${projectId}`}>
        <Button variant="ghost" size="sm">
          <ArrowLeft className="h-4 w-4 mr-1" /> Back to project
        </Button>
      </Link>

      <div>
        <div className="flex items-center gap-3 mb-2">
          <span className="font-mono text-sm text-muted-foreground">{task.id}</span>
          <Badge variant={statusVariant(task.status)}>{task.status}</Badge>
          <Badge variant={priorityVariant(task.priority)}>{task.priority}</Badge>
          <Badge variant="outline">{task.type}</Badge>
        </div>
        <h1 className="text-2xl font-bold">{task.title}</h1>
      </div>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
        <div className="space-y-1">
          <span className="text-muted-foreground">Assignee</span>
          <div className="flex items-center gap-1"><User className="h-3 w-3" /> {assigneeLabel}</div>
        </div>
        <div className="space-y-1">
          <span className="text-muted-foreground">Risk / Scope / Complexity</span>
          <div>{task.risk} / {task.scope} / {task.complexity}</div>
        </div>
        {task.branch_name && (
          <div className="space-y-1">
            <span className="text-muted-foreground">Branch</span>
            <div className="flex items-center gap-1"><GitBranch className="h-3 w-3" /> {task.branch_name}</div>
          </div>
        )}
        {task.deadline && (
          <div className="space-y-1">
            <span className="text-muted-foreground">Deadline</span>
            <div className="flex items-center gap-1"><Clock className="h-3 w-3" /> {task.deadline}</div>
          </div>
        )}
      </div>

      {task.tags.length > 0 && (
        <div className="flex gap-1 flex-wrap">
          {task.tags.map((tag) => (
            <Badge key={tag} variant="outline" className="text-xs">{tag}</Badge>
          ))}
        </div>
      )}

      {task.description && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Description</CardTitle></CardHeader>
          <CardContent>
            <Markdown>{task.description}</Markdown>
          </CardContent>
        </Card>
      )}

      {task.checklist.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">
              Checklist ({task.checklist.filter((c) => c.completed).length}/{task.checklist.length})
            </CardTitle>
          </CardHeader>
          <CardContent>
            <ul className="space-y-2">
              {task.checklist.map((item) => (
                <li key={item.id} className="flex items-start gap-2">
                  {item.completed
                    ? <CheckCircle2 className="h-4 w-4 text-emerald-500 mt-0.5 shrink-0" />
                    : <Circle className="h-4 w-4 text-muted-foreground mt-0.5 shrink-0" />
                  }
                  <span className={item.completed ? "line-through text-muted-foreground" : ""}>
                    {item.description}
                  </span>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      {task.linked_requirements.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Linked Requirements</CardTitle></CardHeader>
          <CardContent>
            <div className="flex gap-2 flex-wrap">
              {task.linked_requirements.map((reqId) => (
                <Link key={reqId} to={`/projects/${projectId}/requirements/${reqId}`}>
                  <Badge variant="outline" className="cursor-pointer hover:bg-accent">{reqId}</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {task.dependencies.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Dependencies</CardTitle></CardHeader>
          <CardContent>
            <ul className="space-y-1 text-sm">
              {task.dependencies.map((dep) => (
                <li key={dep.task_id} className="flex items-center gap-2">
                  <Badge variant="outline" className="text-xs">{dep.dependency_type}</Badge>
                  <Link to={`/projects/${projectId}/tasks/${dep.task_id}`} className="text-primary hover:underline">
                    {dep.task_id}
                  </Link>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      {task.blocked_reason && (
        <Card className="border-destructive/50">
          <CardHeader><CardTitle className="text-lg text-destructive">Blocked</CardTitle></CardHeader>
          <CardContent>
            <p className="text-sm">{task.blocked_reason}</p>
            {task.blocked_by && (
              <p className="text-sm mt-1">Blocked by: <Link to={`/projects/${projectId}/tasks/${task.blocked_by}`} className="text-primary hover:underline">{task.blocked_by}</Link></p>
            )}
          </CardContent>
        </Card>
      )}

      <div className="text-xs text-muted-foreground space-y-1">
        <div>Created: {task.metadata.created_at} by {task.metadata.created_by}</div>
        <div>Updated: {task.metadata.updated_at} by {task.metadata.updated_by}</div>
        <div>Version: {task.metadata.version}</div>
      </div>
    </div>
  );
}
