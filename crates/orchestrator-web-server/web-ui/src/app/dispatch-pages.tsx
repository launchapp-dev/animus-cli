import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  ReadyTasksDocument,
  WorkflowDefinitionsDocument,
  DaemonDocument,
  RunWorkflowDocument,
} from "@/lib/graphql/generated/graphql";
import { statusColor, priorityColor, PageLoading, PageError } from "./shared";

const REQUIREMENTS_QUERY = `
  query Requirements {
    requirements { id title description priority priorityRaw status statusRaw requirementType tags linkedTaskIds acceptanceCriteria }
  }
`;

function WorkflowTypeSelector({
  value,
  onChange,
  definitions,
}: {
  value: string;
  onChange: (v: string) => void;
  definitions: Array<{ id: string; name: string; description?: string | null; phases: string[] }>;
}) {
  const selected = definitions.find((d) => d.id === value);
  return (
    <div className="space-y-3">
      <div className="flex gap-2 flex-wrap" role="radiogroup" aria-label="Workflow type">
        {definitions.map((def) => (
          <button
            key={def.id}
            type="button"
            role="radio"
            aria-checked={value === def.id}
            onClick={() => onChange(def.id)}
            className={`rounded-md border px-4 py-2 text-sm transition-all duration-150 ${
              value === def.id
                ? "border-primary/30 bg-primary/5 text-primary"
                : "border-border/40 text-muted-foreground hover:bg-accent/50 hover:text-foreground"
            }`}
          >
            {def.name}
          </button>
        ))}
      </div>
      {selected && selected.description && (
        <p className="text-xs text-muted-foreground/70">{selected.description}</p>
      )}
      {selected && selected.phases.length > 0 && (
        <div className="flex gap-1.5 flex-wrap">
          {selected.phases.map((phase) => (
            <span
              key={phase}
              className="rounded-full bg-muted/40 px-2.5 py-0.5 text-[10px] font-mono text-muted-foreground"
            >
              {phase}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function BackLink() {
  return (
    <Link to="/workflows" className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors">
      &larr; Back to Workflows
    </Link>
  );
}

function PreflightCheck({ check, index }: { check: { label: string; passed: boolean; fix?: string }; index: number }) {
  return (
    <div className="flex items-start gap-2.5 ao-fade-in" style={{ animationDelay: `${index * 60}ms` }}>
      <span
        className={`mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full ${
          check.passed
            ? "bg-[var(--ao-success)] shadow-[0_0_6px_var(--ao-success)]"
            : "bg-destructive shadow-[0_0_6px_oklch(0.65_0.22_25/50%)]"
        }`}
        aria-hidden="true"
      />
      <div>
        <p className="text-sm">{check.label}</p>
        {!check.passed && check.fix && (
          <p className="text-xs text-muted-foreground/60 mt-0.5">{check.fix}</p>
        )}
      </div>
    </div>
  );
}

function ComingSoonNotice({ command }: { command: string }) {
  return (
    <div className="rounded-md border border-border/40 bg-card/60 px-4 py-3">
      <p className="text-xs text-muted-foreground">
        Coming soon &mdash; use <code className="font-mono text-[11px] bg-muted/40 px-1 py-0.5 rounded">{command}</code> via CLI
      </p>
    </div>
  );
}

export function TaskDispatchPage() {
  const navigate = useNavigate();
  const [search, setSearch] = useState("");
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [workflowType, setWorkflowType] = useState<string>("standard");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [model, setModel] = useState("auto");
  const [tool, setTool] = useState("auto");
  const [maxReworks, setMaxReworks] = useState(3);
  const [phaseTimeout, setPhaseTimeout] = useState("");
  const [skipPhases, setSkipPhases] = useState("");
  const [vars, setVars] = useState("");
  const [launching, setLaunching] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const [readyResult] = useQuery({ query: ReadyTasksDocument, variables: { search: search.trim() || null, limit: 50 } });
  const [defsResult] = useQuery({ query: WorkflowDefinitionsDocument });
  const [daemonResult] = useQuery({ query: DaemonDocument });
  const [, runWorkflow] = useMutation(RunWorkflowDocument);

  const tasks = readyResult.data?.readyTasks ?? [];
  const definitions = defsResult.data?.workflowDefinitions ?? [];
  const daemonStatus = daemonResult.data?.daemonStatus;
  const daemonHealth = daemonResult.data?.daemonHealth;

  const selectedTask = useMemo(
    () => tasks.find((t) => t.id === selectedTaskId) ?? null,
    [tasks, selectedTaskId]
  );

  const selectedDef = useMemo(
    () => definitions.find((d) => d.id === workflowType),
    [definitions, workflowType]
  );

  useMemo(() => {
    if (definitions.length > 0 && !definitions.find((d) => d.id === workflowType)) {
      setWorkflowType(definitions[0].id);
    }
  }, [definitions, workflowType]);

  const preflightChecks = useMemo(() => {
    const checks: { label: string; passed: boolean; fix?: string }[] = [];

    const daemonRunning = daemonStatus?.statusRaw === "running";
    checks.push({
      label: "Daemon is running",
      passed: daemonRunning,
      fix: daemonRunning ? undefined : "Start the daemon from the Daemon page or run `ao daemon start`",
    });

    const runnerOk = daemonHealth?.runnerConnected === true;
    checks.push({
      label: "Runner is connected",
      passed: runnerOk,
      fix: runnerOk ? undefined : "Check runner health via `ao runner health`",
    });

    const maxAgents = daemonStatus?.maxAgents ?? 0;
    const activeAgents = daemonStatus?.activeAgents ?? 0;
    const hasCapacity = maxAgents === 0 || activeAgents < maxAgents;
    checks.push({
      label: "Agent capacity available",
      passed: hasCapacity,
      fix: hasCapacity ? undefined : `All ${maxAgents} agent slots in use. Wait for a slot or increase max agents.`,
    });

    if (selectedTask) {
      const validStatus = ["ready", "backlog"].includes(selectedTask.statusRaw ?? "");
      checks.push({
        label: `Task status is dispatchable (${selectedTask.statusRaw})`,
        passed: validStatus,
        fix: validStatus ? undefined : "Set task status to 'ready' or 'backlog' before dispatching",
      });
    }

    return checks;
  }, [daemonStatus, daemonHealth, selectedTask]);

  const allPassed = selectedTask !== null && preflightChecks.every((c) => c.passed);

  const onLaunch = async () => {
    if (!selectedTaskId) return;
    setLaunching(true);
    setErrorMsg(null);

    const workflowRef = selectedDef && selectedDef.id !== "standard" ? selectedDef.id : null;
    const resolvedModel = model !== "auto" ? model : null;
    const resolvedTool = tool !== "auto" ? tool : null;
    const resolvedTimeout = phaseTimeout ? parseInt(phaseTimeout, 10) : null;
    const resolvedSkipPhases = skipPhases.trim()
      ? skipPhases.split(",").map((s) => s.trim()).filter(Boolean)
      : null;
    const resolvedVars = vars.trim() || null;

    const { data, error } = await runWorkflow({
      taskId: selectedTaskId,
      workflowRef,
      model: resolvedModel,
      tool: resolvedTool,
      vars: resolvedVars,
      skipPhases: resolvedSkipPhases,
      phaseTimeoutSecs: resolvedTimeout,
    });
    setLaunching(false);
    if (error) {
      setErrorMsg(error.message);
    } else if (data?.runWorkflow?.id) {
      navigate(`/workflows/${data.runWorkflow.id}`);
    }
  };

  if (readyResult.fetching || daemonResult.fetching || defsResult.fetching) return <PageLoading />;
  if (readyResult.error) return <PageError message={readyResult.error.message} />;

  return (
    <div className="space-y-6 ao-fade-in">
      <BackLink />
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Run Workflow</h1>
        <p className="text-sm text-muted-foreground mt-1">Dispatch a workflow for a task</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Task</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4">
          <Input
            placeholder="Search ready tasks by ID or title..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search tasks"
            className="mb-3"
          />
          <div className="max-h-64 overflow-y-auto space-y-1">
            {tasks.length === 0 ? (
              <p className="text-sm text-muted-foreground py-8 text-center">No ready tasks match.</p>
            ) : (
              tasks.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  onClick={() => setSelectedTaskId(t.id)}
                  aria-pressed={selectedTaskId === t.id}
                  className={`w-full text-left rounded-md border px-3 py-2 transition-all duration-150 ${
                    selectedTaskId === t.id
                      ? "border-primary/30 bg-primary/5"
                      : "border-transparent hover:bg-accent/30"
                  }`}
                >
                  <div className="flex items-center gap-3">
                    <span className="font-mono text-xs text-muted-foreground shrink-0">{t.id}</span>
                    <span className="text-sm flex-1 truncate">{t.title}</span>
                    <Badge variant={priorityColor(t.priorityRaw ?? "")} className="text-[10px]">{t.priorityRaw}</Badge>
                    <Badge variant={statusColor(t.statusRaw ?? "")} className="text-[10px]">{t.statusRaw}</Badge>
                  </div>
                </button>
              ))
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Workflow Type</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4">
          {definitions.length > 0 ? (
            <WorkflowTypeSelector value={workflowType} onChange={setWorkflowType} definitions={definitions} />
          ) : (
            <p className="text-sm text-muted-foreground py-4 text-center">No workflow definitions available.</p>
          )}
        </CardContent>
      </Card>

      <div>
        <button
          type="button"
          onClick={() => setShowAdvanced(!showAdvanced)}
          aria-expanded={showAdvanced}
          className="inline-flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          <span className={`inline-block transition-transform duration-150 ${showAdvanced ? "rotate-90" : ""}`}>&#9656;</span>
          {showAdvanced ? "Hide advanced" : "Show advanced"}
        </button>
        {showAdvanced && (
          <Card className="border-border/40 bg-card/60 mt-2 ao-fade-in">
            <CardHeader className="pb-2 pt-3 px-4">
              <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Configuration</CardTitle>
            </CardHeader>
            <CardContent className="px-4 pb-4">
              <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Model</label>
                  <select
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
                  >
                    <option value="auto">Auto (default)</option>
                    <option value="claude-sonnet-4-6">claude-sonnet-4-6</option>
                    <option value="claude-opus-4-6">claude-opus-4-6</option>
                    <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview</option>
                  </select>
                </div>
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Tool</label>
                  <select
                    value={tool}
                    onChange={(e) => setTool(e.target.value)}
                    className="mt-1 h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
                  >
                    <option value="auto">Auto (default)</option>
                    <option value="claude">claude</option>
                    <option value="codex">codex</option>
                    <option value="gemini">gemini</option>
                  </select>
                </div>
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Max Reworks</label>
                  <Input
                    type="number"
                    min={0}
                    max={10}
                    value={maxReworks}
                    onChange={(e) => setMaxReworks(Number(e.target.value))}
                    className="mt-1"
                  />
                </div>
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Phase Timeout (s)</label>
                  <Input
                    type="number"
                    min={0}
                    value={phaseTimeout}
                    onChange={(e) => setPhaseTimeout(e.target.value)}
                    placeholder="default"
                    className="mt-1"
                  />
                </div>
              </div>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mt-4">
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Skip Phases</label>
                  <Input
                    value={skipPhases}
                    onChange={(e) => setSkipPhases(e.target.value)}
                    placeholder="phase1, phase2"
                    className="mt-1"
                  />
                  <p className="text-[10px] text-muted-foreground/50 mt-1">Comma-separated phase IDs to skip</p>
                </div>
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Variables (JSON)</label>
                  <Input
                    value={vars}
                    onChange={(e) => setVars(e.target.value)}
                    placeholder='{"key": "value"}'
                    className="mt-1"
                  />
                  <p className="text-[10px] text-muted-foreground/50 mt-1">JSON string passed as workflow variables</p>
                </div>
              </div>
            </CardContent>
          </Card>
        )}
      </div>

      {selectedTask && (
        <Card className="border-border/40 bg-card/60 ao-fade-in">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Pre-flight</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-4">
            <div className="space-y-2.5">
              {preflightChecks.map((check, i) => (
                <PreflightCheck key={check.label} check={check} index={i} />
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {errorMsg && (
        <Alert variant="destructive" className="ao-fade-in border-destructive/30 bg-destructive/8">
          <AlertDescription>{errorMsg}</AlertDescription>
        </Alert>
      )}

      <Button
        onClick={onLaunch}
        disabled={!allPassed || launching}
      >
        {launching ? "Launching..." : "Launch Workflow"}
      </Button>
    </div>
  );
}

export function RequirementDispatchPage() {
  const [{ data, fetching, error }] = useQuery({ query: REQUIREMENTS_QUERY });
  const [defsResult] = useQuery({ query: WorkflowDefinitionsDocument });
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [autoStart, setAutoStart] = useState(true);
  const [includeWont, setIncludeWont] = useState(false);
  const [workflowType, setWorkflowType] = useState<string>("standard");

  const definitions = defsResult.data?.workflowDefinitions ?? [];

  useMemo(() => {
    if (definitions.length > 0 && !definitions.find((d) => d.id === workflowType)) {
      setWorkflowType(definitions[0].id);
    }
  }, [definitions, workflowType]);

  const requirements = useMemo(() => {
    const list = (data?.requirements ?? []) as Array<{
      id: string;
      title: string;
      priorityRaw: string;
      statusRaw: string;
    }>;
    if (includeWont) return list;
    return list.filter((r) => r.priorityRaw !== "wont");
  }, [data, includeWont]);

  const toggleSelection = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const selectAll = () => {
    setSelectedIds(new Set(requirements.map((r) => r.id)));
  };

  const selectMust = () => {
    setSelectedIds(new Set(requirements.filter((r) => r.priorityRaw === "must").map((r) => r.id)));
  };

  if (fetching || defsResult.fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  return (
    <div className="space-y-6 ao-fade-in">
      <BackLink />
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Execute Requirements</h1>
        <p className="text-sm text-muted-foreground mt-1">Generate tasks and dispatch workflows from requirements</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <div className="flex items-center justify-between">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Requirements</CardTitle>
            <div className="flex gap-2">
              <Button size="sm" variant="outline" className="h-6 text-xs" onClick={selectAll}>Select All</Button>
              <Button size="sm" variant="outline" className="h-6 text-xs" onClick={selectMust}>Select Must</Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="px-4 pb-4">
          {requirements.length === 0 ? (
            <p className="text-sm text-muted-foreground py-8 text-center">No requirements found.</p>
          ) : (
            <div className="max-h-80 overflow-y-auto space-y-1">
              {requirements.map((req) => (
                <label
                  key={req.id}
                  className={`flex items-center gap-3 rounded-md border px-3 py-2 transition-all duration-150 cursor-pointer ${
                    selectedIds.has(req.id)
                      ? "border-primary/30 bg-primary/5"
                      : "border-transparent hover:bg-accent/30"
                  }`}
                >
                  <input
                    type="checkbox"
                    checked={selectedIds.has(req.id)}
                    onChange={() => toggleSelection(req.id)}
                    className="h-4 w-4 shrink-0"
                  />
                  <span className="font-mono text-xs text-muted-foreground shrink-0">{req.id}</span>
                  <span className="text-sm flex-1 truncate">{req.title}</span>
                  <Badge variant={req.priorityRaw === "must" ? "destructive" : req.priorityRaw === "should" ? "default" : "secondary"} className="text-[10px]">
                    {req.priorityRaw}
                  </Badge>
                  <Badge variant="outline" className="text-[10px]">{req.statusRaw}</Badge>
                </label>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Options</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4 space-y-3">
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={autoStart}
              onChange={(e) => setAutoStart(e.target.checked)}
              className="h-4 w-4"
            />
            <span className="text-sm">Auto-start workflows after task creation</span>
          </label>
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={includeWont}
              onChange={(e) => setIncludeWont(e.target.checked)}
              className="h-4 w-4"
            />
            <span className="text-sm">Include won't-fix requirements</span>
          </label>
          <div>
            <p className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium mb-2">Workflow Type</p>
            {definitions.length > 0 ? (
              <WorkflowTypeSelector value={workflowType} onChange={setWorkflowType} definitions={definitions} />
            ) : (
              <p className="text-sm text-muted-foreground">Loading workflow definitions...</p>
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Preview</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4">
          <div className="space-y-1 text-sm">
            <p><span className="font-mono font-semibold">{selectedIds.size}</span> requirements selected</p>
            <p>Estimated tasks: <span className="font-mono font-semibold">{selectedIds.size}</span></p>
            {autoStart && selectedIds.size > 0 && (
              <p className="text-muted-foreground">Will dispatch up to <span className="font-mono font-semibold">{selectedIds.size}</span> workflows</p>
            )}
          </div>
        </CardContent>
      </Card>

      <div className="space-y-2">
        <Button disabled>Execute Requirements</Button>
        <ComingSoonNotice command="ao workflow run --requirement-id" />
      </div>
    </div>
  );
}

export function CustomDispatchPage() {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [workflowType, setWorkflowType] = useState<string>("standard");
  const [defsResult] = useQuery({ query: WorkflowDefinitionsDocument });

  const definitions = defsResult.data?.workflowDefinitions ?? [];

  useMemo(() => {
    if (definitions.length > 0 && !definitions.find((d) => d.id === workflowType)) {
      setWorkflowType(definitions[0].id);
    }
  }, [definitions, workflowType]);

  return (
    <div className="space-y-6 ao-fade-in">
      <BackLink />
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Custom Workflow</h1>
        <p className="text-sm text-muted-foreground mt-1">Run an ad-hoc workflow</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Details</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4 space-y-4">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</label>
            <Input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Workflow title..."
              className="mt-1"
            />
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
            <Textarea
              rows={4}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Describe what this workflow should accomplish..."
              className="mt-1"
            />
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Workflow Type</label>
            <div className="mt-2">
              {definitions.length > 0 ? (
                <WorkflowTypeSelector value={workflowType} onChange={setWorkflowType} definitions={definitions} />
              ) : (
                <p className="text-sm text-muted-foreground">Loading workflow definitions...</p>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      <div className="space-y-2">
        <Button disabled>Launch Custom Workflow</Button>
        <ComingSoonNotice command="custom dispatch is not yet available via the GraphQL API" />
      </div>
    </div>
  );
}
