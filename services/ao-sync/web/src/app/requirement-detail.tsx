import { useParams, Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Markdown } from "@/components/markdown";

function statusVariant(status: string) {
  if (["done", "implemented", "approved"].includes(status)) return "success" as const;
  if (["in-progress"].includes(status)) return "default" as const;
  if (["blocked", "cancelled", "deprecated", "needs-rework"].includes(status)) return "destructive" as const;
  return "secondary" as const;
}

function priorityVariant(priority: string) {
  if (priority === "must") return "destructive" as const;
  if (priority === "should") return "warning" as const;
  return "secondary" as const;
}

export function RequirementDetailPage() {
  const { projectId, reqId } = useParams<{ projectId: string; reqId: string }>();

  const { data, isLoading } = useQuery({
    queryKey: ["requirement", projectId, reqId],
    queryFn: () => api.requirements.get(projectId!, reqId!),
    enabled: !!projectId && !!reqId,
  });

  if (isLoading) {
    return <div className="text-muted-foreground py-8 text-center">Loading...</div>;
  }

  const req = data?.requirement;
  if (!req) {
    return <div className="text-muted-foreground py-8 text-center">Requirement not found</div>;
  }

  return (
    <div className="space-y-6 max-w-4xl">
      <Link to={`/projects/${projectId}`}>
        <Button variant="ghost" size="sm">
          <ArrowLeft className="h-4 w-4 mr-1" /> Back to project
        </Button>
      </Link>

      <div>
        <div className="flex items-center gap-3 mb-2">
          <span className="font-mono text-sm text-muted-foreground">{req.id}</span>
          <Badge variant={statusVariant(req.status)}>{req.status}</Badge>
          <Badge variant={priorityVariant(req.priority)}>{req.priority}</Badge>
          {req.type && <Badge variant="outline">{req.type}</Badge>}
          {req.category && <Badge variant="outline">{req.category}</Badge>}
        </div>
        <h1 className="text-2xl font-bold">{req.title}</h1>
      </div>

      {req.tags.length > 0 && (
        <div className="flex gap-1 flex-wrap">
          {req.tags.map((tag) => (
            <Badge key={tag} variant="outline" className="text-xs">{tag}</Badge>
          ))}
        </div>
      )}

      {req.description && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Description</CardTitle></CardHeader>
          <CardContent>
            <Markdown>{req.description}</Markdown>
          </CardContent>
        </Card>
      )}

      {req.body && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Details</CardTitle></CardHeader>
          <CardContent>
            <Markdown>{req.body}</Markdown>
          </CardContent>
        </Card>
      )}

      {req.acceptance_criteria.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Acceptance Criteria</CardTitle></CardHeader>
          <CardContent>
            <ul className="space-y-2">
              {req.acceptance_criteria.map((criterion, i) => (
                <li key={i} className="flex items-start gap-2 text-sm">
                  <span className="text-muted-foreground shrink-0">{i + 1}.</span>
                  <Markdown>{criterion}</Markdown>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      {req.linked_task_ids.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-lg">Linked Tasks</CardTitle></CardHeader>
          <CardContent>
            <div className="flex gap-2 flex-wrap">
              {req.linked_task_ids.map((taskId) => (
                <Link key={taskId} to={`/projects/${projectId}/tasks/${taskId}`}>
                  <Badge variant="outline" className="cursor-pointer hover:bg-accent">{taskId}</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {req.source && (
        <div className="text-sm text-muted-foreground">
          Source: {req.source}
        </div>
      )}

      <div className="text-xs text-muted-foreground space-y-1">
        <div>Created: {req.created_at}</div>
        <div>Updated: {req.updated_at}</div>
      </div>
    </div>
  );
}
