import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useQuery } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { statusColor, PageLoading, PageError } from "./shared";

const TASK_QUERY = `query Task($id: ID!) { task(id: $id) { id title statusRaw workflowId } }`;
const WORKFLOW_QUERY = `query Workflow($id: ID!) { workflow(id: $id) { id statusRaw phases { phaseId status startedAt completedAt } } }`;
const PHASE_OUTPUT_QUERY = `query PhaseOutput($workflowId: ID!, $phaseId: String, $tail: Int) { phaseOutput(workflowId: $workflowId, phaseId: $phaseId, tail: $tail) { lines phaseId hasMore } }`;

type TaskData = { task: { id: string; title: string; statusRaw: string | null; workflowId: string | null } };
type WorkflowPhase = { phaseId: string; status: string | null; startedAt: string | null; completedAt: string | null };
type Workflow = { id: string; statusRaw: string | null; phases: WorkflowPhase[] };
type WorkflowData = { workflow: Workflow | null };
type PhaseOutputData = { phaseOutput: { lines: string[]; phaseId: string; hasMore: boolean } };

function PhaseSection({
  phase,
  workflowId,
  searchTerm,
  expanded,
  onToggle,
}: {
  phase: WorkflowPhase;
  workflowId: string;
  searchTerm: string;
  expanded: boolean;
  onToggle: () => void;
}) {
  const [result] = useQuery<PhaseOutputData>({
    query: PHASE_OUTPUT_QUERY,
    variables: { workflowId, phaseId: phase.phaseId, tail: 500 },
    pause: !expanded,
  });

  const lines = result.data?.phaseOutput?.lines ?? [];
  const filteredLines = useMemo(() => {
    if (!searchTerm) return lines;
    const lower = searchTerm.toLowerCase();
    return lines.filter((l) => l.toLowerCase().includes(lower));
  }, [lines, searchTerm]);

  return (
    <Card className="border-border/40 bg-card/60">
      <CardHeader className="pb-2 pt-3 px-4">
        <button type="button" onClick={onToggle} className="flex items-center justify-between w-full text-left">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">
            {phase.phaseId}
          </CardTitle>
          <div className="flex items-center gap-2">
            <Badge variant={statusColor(phase.status ?? "")}>{phase.status ?? "unknown"}</Badge>
            <span className="text-xs text-muted-foreground/50">{expanded ? "\u25B2" : "\u25BC"}</span>
          </div>
        </button>
      </CardHeader>
      {expanded && (
        <CardContent className="px-4 pb-3">
          {result.fetching && <p className="text-xs text-muted-foreground/50">Loading output...</p>}
          {result.error && <p className="text-xs text-destructive">{result.error.message}</p>}
          {!result.fetching && !result.error && filteredLines.length === 0 && (
            <p className="text-xs text-muted-foreground/50">
              {searchTerm ? "No matching lines." : "No output available."}
            </p>
          )}
          {filteredLines.length > 0 && (
            <pre data-output-pre className="font-mono text-[11px] text-foreground/70 bg-background/50 rounded-md p-3 overflow-x-auto max-h-[600px] overflow-y-auto whitespace-pre-wrap break-words">
              {searchTerm
                ? filteredLines.map((line, i) => (
                    <HighlightedLine key={i} line={line} term={searchTerm} />
                  ))
                : filteredLines.join("\n")}
            </pre>
          )}
          {result.data?.phaseOutput?.hasMore && (
            <p className="text-[10px] text-muted-foreground/50 mt-1">Output truncated. Showing last 500 lines.</p>
          )}
        </CardContent>
      )}
    </Card>
  );
}

function HighlightedLine({ line, term }: { line: string; term: string }) {
  const lower = line.toLowerCase();
  const termLower = term.toLowerCase();
  const parts: { text: string; highlight: boolean }[] = [];
  let idx = 0;
  while (idx < line.length) {
    const found = lower.indexOf(termLower, idx);
    if (found === -1) {
      parts.push({ text: line.slice(idx), highlight: false });
      break;
    }
    if (found > idx) {
      parts.push({ text: line.slice(idx, found), highlight: false });
    }
    parts.push({ text: line.slice(found, found + term.length), highlight: true });
    idx = found + term.length;
  }
  return (
    <span>
      {parts.map((p, i) =>
        p.highlight ? (
          <mark key={i} className="bg-yellow-400/30 text-foreground rounded-sm px-0.5">{p.text}</mark>
        ) : (
          <span key={i}>{p.text}</span>
        ),
      )}
      {"\n"}
    </span>
  );
}

export function TaskOutputPage() {
  const { taskId } = useParams();
  const [searchTerm, setSearchTerm] = useState("");
  const [expandedPhases, setExpandedPhases] = useState<Set<string>>(new Set());
  const [allExpanded, setAllExpanded] = useState(false);

  const [taskResult] = useQuery<TaskData>({ query: TASK_QUERY, variables: { id: taskId! } });
  const workflowId = taskResult.data?.task?.workflowId ?? null;
  const [workflowResult] = useQuery<WorkflowData>({ query: WORKFLOW_QUERY, variables: { id: workflowId! }, pause: !workflowId });

  const task = taskResult.data?.task;
  const workflow = workflowResult.data?.workflow ?? null;
  const phases = workflow?.phases ?? [];

  const togglePhase = (phaseId: string) => {
    setExpandedPhases((prev) => {
      const next = new Set(prev);
      if (next.has(phaseId)) next.delete(phaseId);
      else next.add(phaseId);
      return next;
    });
  };

  const toggleAll = () => {
    if (allExpanded) {
      setExpandedPhases(new Set());
      setAllExpanded(false);
    } else {
      setExpandedPhases(new Set(phases.map((p) => p.phaseId)));
      setAllExpanded(true);
    }
  };

  const copyAll = async () => {
    const allOutputs = document.querySelectorAll("[data-output-pre]");
    const text = Array.from(allOutputs)
      .map((el) => el.textContent ?? "")
      .join("\n\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      /* clipboard not available */
    }
  };

  if (taskResult.fetching || (workflowId && workflowResult.fetching)) return <PageLoading />;
  if (taskResult.error) return <PageError message={taskResult.error.message} />;
  if (workflowResult.error) return <PageError message={workflowResult.error.message} />;
  if (!task) return <PageError message={`Task ${taskId} not found.`} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <Link to={`/tasks/${taskId}`} className="text-xs text-muted-foreground/60 hover:text-foreground transition-colors">
            &larr; Back to task
          </Link>
          <h1 className="text-2xl font-semibold tracking-tight">Output for {task.id}</h1>
          <p className="text-sm text-muted-foreground/70 mt-0.5">{task.title}</p>
        </div>
        <div className="flex items-center gap-2">
          {workflow && <Badge variant={statusColor(workflow.statusRaw ?? "")}>{workflow.statusRaw}</Badge>}
        </div>
      </div>

      {!workflow ? (
        <Card className="border-border/40 bg-card/60">
          <CardContent className="pt-4 pb-3 px-4">
            <p className="text-sm text-muted-foreground/60">No workflow found for this task.</p>
          </CardContent>
        </Card>
      ) : (
        <>
          <div className="flex items-center gap-3">
            <Input
              placeholder="Search output..."
              value={searchTerm}
              onChange={(e) => setSearchTerm(e.target.value)}
              className="max-w-sm"
            />
            <Button size="sm" variant="outline" onClick={toggleAll}>
              {allExpanded ? "Collapse All" : "Expand All"}
            </Button>
            <Button size="sm" variant="outline" onClick={copyAll}>
              Copy All
            </Button>
          </div>

          <div className="space-y-3">
            {phases.map((phase) => (
              <PhaseSection
                key={phase.phaseId}
                phase={phase}
                workflowId={workflow.id}
                searchTerm={searchTerm}
                expanded={expandedPhases.has(phase.phaseId)}
                onToggle={() => togglePhase(phase.phaseId)}
              />
            ))}
          </div>

          {phases.length === 0 && (
            <p className="text-sm text-muted-foreground/50 text-center py-8">No phases in this workflow.</p>
          )}
        </>
      )}
    </div>
  );
}
