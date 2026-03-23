import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer,
  PieChart, Pie, Cell,
  AreaChart, Area,
  LineChart, Line,
} from "recharts";
import { format, parseISO } from "date-fns";
import { api } from "@/lib/api";
import type { MetricBucket, TimelineBucket } from "@/lib/api";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";

const STATUS_COLORS: Record<string, string> = {
  "backlog": "#94a3b8", "ready": "#60a5fa", "in-progress": "#a78bfa",
  "blocked": "#f87171", "on-hold": "#fbbf24", "done": "#34d399", "cancelled": "#6b7280",
};

const PRIORITY_COLORS: Record<string, string> = {
  "critical": "#ef4444", "high": "#f97316", "medium": "#eab308", "low": "#22c55e",
  "must": "#ef4444", "should": "#f97316", "could": "#eab308", "wont": "#6b7280",
};

const TYPE_COLORS: Record<string, string> = {
  "feature": "#8b5cf6", "bugfix": "#ef4444", "hotfix": "#f97316", "refactor": "#06b6d4",
  "docs": "#10b981", "test": "#3b82f6", "chore": "#6b7280", "experiment": "#ec4899",
};

const REQ_STATUS_COLORS: Record<string, string> = {
  "draft": "#94a3b8", "refined": "#60a5fa", "planned": "#818cf8", "in-progress": "#a78bfa",
  "done": "#34d399", "approved": "#22d3ee", "implemented": "#10b981", "deprecated": "#6b7280",
  "po-review": "#fbbf24", "em-review": "#f59e0b", "needs-rework": "#f87171",
};

const tooltipStyle = {
  background: "hsl(var(--la-card))",
  border: "1px solid hsl(var(--la-border))",
  borderRadius: 8,
  fontSize: 12,
};

const tickStyle = { fill: "hsl(var(--la-muted-foreground))", fontSize: 11 };

function formatWeek(w: string) {
  try { return format(parseISO(w), "MMM d"); } catch { return w; }
}

export function ProjectDataTab({ projectId }: { projectId: string }) {
  const { data: metrics, isLoading } = useQuery({
    queryKey: ["metrics", projectId],
    queryFn: () => api.metrics.get(projectId),
  });

  if (isLoading) {
    return <div className="text-muted-foreground py-8 text-center">Loading metrics...</div>;
  }

  if (!metrics) {
    return <div className="text-muted-foreground py-8 text-center">No data</div>;
  }

  const timeline = metrics.timeline.map((t) => ({ ...t, week: formatWeek(t.week) }));

  const burndown = (() => {
    let cumDone = 0;
    return timeline.map((t) => {
      cumDone += t.completed;
      return { week: t.week, remaining: metrics.tasks.total - cumDone, done: cumDone };
    });
  })();

  const doneCount = metrics.tasks.by_status.find((s) => s.name === "done")?.value ?? 0;
  const inProgressCount = metrics.tasks.by_status.find((s) => s.name === "in-progress")?.value ?? 0;
  const blockedCount = metrics.tasks.by_status.find((s) => s.name === "blocked")?.value ?? 0;
  const completionPct = metrics.tasks.total > 0 ? Math.round((doneCount / metrics.tasks.total) * 100) : 0;

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
        <StatCard label="Total Tasks" value={metrics.tasks.total} />
        <StatCard label="In Progress" value={inProgressCount} />
        <StatCard label="Done" value={doneCount} sub={`${completionPct}%`} />
        <StatCard label="Blocked" value={blockedCount} />
        <StatCard label="Requirements" value={metrics.requirements.total} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <Card>
          <CardHeader><CardTitle className="text-lg">Task Activity (12 weeks)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={280}>
              <AreaChart data={timeline}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="week" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Area type="monotone" dataKey="created" stackId="1" stroke="#8b5cf6" fill="#8b5cf6" fillOpacity={0.4} name="Created" />
                <Area type="monotone" dataKey="completed" stackId="2" stroke="#34d399" fill="#34d399" fillOpacity={0.4} name="Completed" />
              </AreaChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle className="text-lg">Burndown (12 weeks)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={280}>
              <LineChart data={burndown}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="week" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Line type="monotone" dataKey="remaining" stroke="#f87171" strokeWidth={2} dot={false} name="Remaining" />
                <Line type="monotone" dataKey="done" stroke="#34d399" strokeWidth={2} dot={false} name="Done" />
              </LineChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
        <ChartCard title="Tasks by Status" data={metrics.tasks.by_status} colors={STATUS_COLORS} type="pie" />
        <ChartCard title="Tasks by Priority" data={metrics.tasks.by_priority} colors={PRIORITY_COLORS} type="hbar" />
        <ChartCard title="Tasks by Type" data={metrics.tasks.by_type} colors={TYPE_COLORS} type="hbar" />
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <ChartCard title="Requirements by Status" data={metrics.requirements.by_status} colors={REQ_STATUS_COLORS} type="pie" />
        <ChartCard title="Requirements by Priority" data={metrics.requirements.by_priority} colors={PRIORITY_COLORS} type="bar" />
      </div>
    </div>
  );
}

function StatCard({ label, value, sub }: { label: string; value: number; sub?: string }) {
  return (
    <Card>
      <CardContent className="p-4">
        <div className="text-2xl font-bold">{value}</div>
        <div className="text-sm text-muted-foreground">{label}</div>
        {sub && <div className="text-xs text-muted-foreground mt-1">{sub}</div>}
      </CardContent>
    </Card>
  );
}

function ChartCard({ title, data, colors, type }: {
  title: string;
  data: MetricBucket[];
  colors: Record<string, string>;
  type: "pie" | "bar" | "hbar";
}) {
  return (
    <Card>
      <CardHeader><CardTitle className="text-lg">{title}</CardTitle></CardHeader>
      <CardContent>
        <ResponsiveContainer width="100%" height={280}>
          {type === "pie" ? (
            <PieChart>
              <Pie data={data} cx="50%" cy="50%" innerRadius={50} outerRadius={90} paddingAngle={2} dataKey="value"
                label={({ name, value }) => `${name} (${value})`} labelLine={false}>
                {data.map((e) => <Cell key={e.name} fill={colors[e.name] || "#94a3b8"} />)}
              </Pie>
              <Tooltip />
            </PieChart>
          ) : type === "hbar" ? (
            <BarChart data={data} layout="vertical">
              <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
              <XAxis type="number" tick={tickStyle} />
              <YAxis type="category" dataKey="name" width={80} tick={tickStyle} />
              <Tooltip contentStyle={tooltipStyle} />
              <Bar dataKey="value" radius={[0, 4, 4, 0]}>
                {data.map((e) => <Cell key={e.name} fill={colors[e.name] || "#94a3b8"} />)}
              </Bar>
            </BarChart>
          ) : (
            <BarChart data={data}>
              <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
              <XAxis dataKey="name" tick={tickStyle} />
              <YAxis tick={tickStyle} />
              <Tooltip contentStyle={tooltipStyle} />
              <Bar dataKey="value" fill="#8b5cf6" radius={[4, 4, 0, 0]}>
                {data.map((e) => <Cell key={e.name} fill={colors[e.name] || "#8b5cf6"} />)}
              </Bar>
            </BarChart>
          )}
        </ResponsiveContainer>
      </CardContent>
    </Card>
  );
}
