import { ReactFlow, Background, Controls, MiniMap, Handle, Position, type Node, type Edge } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useQuery } from "@/lib/graphql/client";
import { DashboardDocument } from "@/lib/graphql/generated/graphql";
import { PageError, PageLoading } from "./shared";

const WORKFLOWS_QUERY = `query ActiveWorkflows { workflows(status: "running") { id taskId status statusRaw currentPhase phases { phaseId status startedAt completedAt } } }`;

function TaskPoolNode({ data }: { data: { total: number; ready: number; inProgress: number; blocked: number; done: number } }) {
  return (
    <div className="bg-card border border-border/60 rounded-lg px-4 py-3 min-w-[180px] shadow-lg">
      <Handle type="source" position={Position.Right} />
      <div className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium mb-1">Task Pool</div>
      <div className="text-2xl font-semibold tabular-nums">{data.total}</div>
      <div className="flex gap-1 mt-2 h-1.5 rounded-full overflow-hidden bg-muted/30">
        {data.ready > 0 && <div className="bg-primary/60" style={{ flex: data.ready }} />}
        {data.inProgress > 0 && <div className="bg-[var(--ao-running)]" style={{ flex: data.inProgress }} />}
        {data.blocked > 0 && <div className="bg-[var(--ao-amber)]" style={{ flex: data.blocked }} />}
        {data.done > 0 && <div className="bg-[var(--ao-success)]" style={{ flex: data.done }} />}
      </div>
      <div className="flex gap-2 mt-1.5 text-[9px] text-muted-foreground/50">
        <span>{data.ready} ready</span>
        <span>{data.inProgress} active</span>
        <span>{data.blocked} blocked</span>
      </div>
    </div>
  );
}

function QueueNode({ data }: { data: { depth: number; held: number } }) {
  return (
    <div className="bg-card border border-border/60 rounded-lg px-4 py-3 min-w-[180px] shadow-lg">
      <Handle type="target" position={Position.Left} />
      <Handle type="source" position={Position.Right} />
      <div className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium mb-1">Dispatch Queue</div>
      <div className="text-2xl font-semibold tabular-nums">{data.depth}</div>
      {data.held > 0 && <div className="text-[10px] text-[var(--ao-amber)]">{data.held} held</div>}
    </div>
  );
}

function DaemonNode({ data }: { data: { healthy: boolean; status: string; agents: number; pid?: number } }) {
  return (
    <div className="bg-card border-2 border-primary/30 rounded-xl px-5 py-4 min-w-[200px] shadow-lg">
      <Handle type="target" position={Position.Left} />
      <Handle type="source" position={Position.Right} />
      <div className="flex items-center gap-2 mb-2">
        <span className={`h-2.5 w-2.5 rounded-full ${data.healthy ? "bg-[var(--ao-success)]" : "bg-destructive"}`} />
        <span className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Daemon</span>
      </div>
      <div className="text-sm font-medium">{data.status}</div>
      <div className="text-[10px] text-muted-foreground/50 mt-1">{data.agents} agents active</div>
    </div>
  );
}

function WorkflowNode({ data }: { data: { id: string; taskId: string; currentPhase: string; phasesDone: number; phasesTotal: number } }) {
  const progress = data.phasesTotal > 0 ? (data.phasesDone / data.phasesTotal) * 100 : 0;
  return (
    <div className="bg-card border border-[var(--ao-running-border)] rounded-lg px-3 py-2.5 min-w-[200px] shadow-lg">
      <Handle type="target" position={Position.Left} />
      <Handle type="source" position={Position.Right} />
      <div className="flex items-center gap-2">
        <span className="h-2 w-2 rounded-full bg-[var(--ao-running)] animate-pulse" />
        <span className="text-[10px] font-mono text-muted-foreground/50">{data.taskId}</span>
      </div>
      <div className="text-xs font-medium mt-1">{data.currentPhase || "starting"}</div>
      <div className="h-1 rounded-full bg-muted/30 mt-1.5 overflow-hidden">
        <div className="h-full bg-[var(--ao-running)] rounded-full transition-all" style={{ width: `${progress}%` }} />
      </div>
      <div className="text-[9px] text-muted-foreground/40 mt-0.5">{data.phasesDone}/{data.phasesTotal} phases</div>
    </div>
  );
}

function OutcomeNode({ data }: { data: { label: string; count: number; color: string } }) {
  return (
    <div className="bg-card border rounded-lg px-4 py-3 min-w-[140px] shadow-lg" style={{ borderColor: `var(${data.color})` }}>
      <Handle type="target" position={Position.Left} />
      <div className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium mb-1">{data.label}</div>
      <div className="text-xl font-semibold tabular-nums" style={{ color: `var(${data.color})` }}>{data.count}</div>
    </div>
  );
}

const nodeTypes = {
  taskPool: TaskPoolNode,
  queue: QueueNode,
  daemon: DaemonNode,
  workflow: WorkflowNode,
  outcome: OutcomeNode,
};

function buildGraph(dashData: any, workflows: any[]) {
  const nodes: Node[] = [];
  const edges: Edge[] = [];

  const byStatus: Record<string, number> = dashData?.taskStats?.byStatus ? JSON.parse(dashData.taskStats.byStatus) : {};

  nodes.push({
    id: "task-pool",
    type: "taskPool",
    position: { x: 0, y: 50 },
    data: {
      total: dashData?.taskStats?.total ?? 0,
      ready: byStatus["ready"] ?? 0,
      inProgress: byStatus["in-progress"] ?? 0,
      blocked: byStatus["blocked"] ?? 0,
      done: byStatus["done"] ?? 0,
    },
  });

  nodes.push({
    id: "queue",
    type: "queue",
    position: { x: 0, y: 300 },
    data: { depth: dashData?.queueStats?.depth ?? 0, held: 0 },
  });

  const health = dashData?.daemonHealth;
  nodes.push({
    id: "daemon",
    type: "daemon",
    position: { x: 350, y: 150 },
    data: {
      healthy: health?.healthy ?? false,
      status: health?.status ?? "unknown",
      agents: dashData?.agentRuns?.length ?? 0,
    },
  });

  edges.push(
    { id: "e-tasks-queue", source: "task-pool", target: "queue", animated: true, style: { stroke: "var(--border)" } },
    { id: "e-queue-daemon", source: "queue", target: "daemon", animated: true, style: { stroke: "var(--primary)", strokeWidth: 2 } },
  );

  const running = workflows.filter((w: any) => w.statusRaw === "running" || w.status === "Running");
  if (running.length > 0) {
    running.forEach((wf: any, i: number) => {
      const phases = wf.phases ?? [];
      const done = phases.filter((p: any) => p.status === "completed").length;
      nodes.push({
        id: `wf-${wf.id}`,
        type: "workflow",
        position: { x: 700, y: i * 120 },
        data: {
          id: wf.id,
          taskId: wf.taskId,
          currentPhase: wf.currentPhase ?? "",
          phasesDone: done,
          phasesTotal: phases.length,
        },
      });
      edges.push({
        id: `e-daemon-wf-${wf.id}`,
        source: "daemon",
        target: `wf-${wf.id}`,
        animated: true,
        style: { stroke: "var(--ao-running)", strokeWidth: 2 },
      });
    });
  } else {
    nodes.push({
      id: "no-workflows",
      type: "outcome",
      position: { x: 700, y: 150 },
      data: { label: "Idle", count: 0, color: "--muted-foreground" },
    });
    edges.push({
      id: "e-daemon-idle",
      source: "daemon",
      target: "no-workflows",
      style: { stroke: "var(--border)", strokeDasharray: "5 5" },
    });
  }

  const doneCount = byStatus["done"] ?? 0;
  const failedWorkflows = workflows.filter((w: any) => w.statusRaw === "failed").length;
  const escalatedWorkflows = workflows.filter((w: any) => w.statusRaw === "escalated").length;

  nodes.push(
    { id: "completed", type: "outcome", position: { x: 1050, y: 0 }, data: { label: "Completed", count: doneCount, color: "--ao-success" } },
    { id: "failed", type: "outcome", position: { x: 1050, y: 150 }, data: { label: "Failed", count: failedWorkflows, color: "--destructive" } },
    { id: "escalated", type: "outcome", position: { x: 1050, y: 300 }, data: { label: "Escalated", count: escalatedWorkflows, color: "--ao-amber" } },
  );

  if (running.length > 0) {
    edges.push(
      { id: "e-wf-completed", source: `wf-${running[running.length - 1].id}`, target: "completed", style: { stroke: "var(--ao-success)", strokeDasharray: "5 5" } },
      { id: "e-wf-failed", source: "daemon", target: "failed", style: { stroke: "var(--destructive)", strokeDasharray: "5 5" } },
    );
  }
  if (escalatedWorkflows > 0) {
    edges.push({ id: "e-wf-escalated", source: "daemon", target: "escalated", style: { stroke: "var(--ao-amber)", strokeDasharray: "5 5" } });
  }

  return { nodes, edges };
}

export function OpsMapPage() {
  const [dashResult] = useQuery({ query: DashboardDocument });
  const [wfResult] = useQuery({ query: WORKFLOWS_QUERY });

  if (dashResult.fetching || wfResult.fetching) return <PageLoading />;
  if (dashResult.error) return <PageError message="Failed to load dashboard data" />;
  if (wfResult.error) return <PageError message="Failed to load workflow data" />;

  const { nodes, edges } = buildGraph(
    dashResult.data,
    (wfResult.data as any)?.workflows ?? [],
  );

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Operations Map</h1>
        <p className="text-xs text-muted-foreground/60 mt-0.5">Live system topology</p>
      </div>
      <div className="h-[600px] border border-border/40 rounded-lg overflow-hidden bg-background">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          fitView
          proOptions={{ hideAttribution: true }}
          defaultEdgeOptions={{ type: "smoothstep" }}
        >
          <Background gap={20} size={1} color="var(--border)" />
          <Controls className="!bg-card !border-border/40 !shadow-lg [&_button]:!bg-card [&_button]:!border-border/40 [&_button]:!text-foreground [&_button:hover]:!bg-accent" />
          <MiniMap
            nodeColor={() => "var(--primary)"}
            maskColor="var(--background)"
            className="!bg-card !border-border/40"
          />
        </ReactFlow>
      </div>
    </div>
  );
}
