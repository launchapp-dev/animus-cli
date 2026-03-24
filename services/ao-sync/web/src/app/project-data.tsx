import { useState, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer,
  PieChart, Pie, Cell,
  AreaChart, Area,
  LineChart, Line,
  Legend,
} from "recharts";
import { format, parseISO } from "date-fns";
import { api } from "@/lib/api";
import type { MetricBucket } from "@/lib/api";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

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

function fmtWeek(w: string) { try { return format(parseISO(w), "MMM d"); } catch { return w; } }
function fmtMonth(m: string) { try { return format(parseISO(m), "MMM yyyy"); } catch { return m; } }

const ALL_STATUSES = ["backlog", "ready", "in-progress", "blocked", "on-hold", "done", "cancelled"];

export function ProjectDataTab({ projectId }: { projectId: string }) {
  const [excludedStatuses, setExcludedStatuses] = useState<Set<string>>(new Set(["cancelled"]));

  const { data: metrics, isLoading } = useQuery({
    queryKey: ["metrics", projectId],
    queryFn: () => api.metrics.get(projectId),
  });

  const toggleStatus = (s: string) => {
    setExcludedStatuses((prev) => {
      const next = new Set(prev);
      if (next.has(s)) next.delete(s); else next.add(s);
      return next;
    });
  };

  const filteredStatusData = useMemo(() => {
    if (!metrics) return [];
    return metrics.tasks.by_status.filter((s) => !excludedStatuses.has(s.name));
  }, [metrics, excludedStatuses]);

  const filteredTotal = useMemo(() => filteredStatusData.reduce((s, r) => s + r.value, 0), [filteredStatusData]);

  if (isLoading) return <div className="text-muted-foreground py-8 text-center">Loading metrics...</div>;
  if (!metrics) return <div className="text-muted-foreground py-8 text-center">No data</div>;

  const weekly = metrics.timeline.weekly.map((t) => ({ ...t, week: fmtWeek(t.week) }));
  const monthly = metrics.timeline.monthly.map((t) => ({ ...t, month: fmtMonth(t.month) }));

  const burndown = (() => {
    let cumDone = 0;
    return weekly.map((t) => {
      cumDone += t.completed;
      return { week: t.week, remaining: filteredTotal - cumDone, done: cumDone };
    });
  })();

  const weeklyStacked = useMemo(() => {
    const byWeek: Record<string, Record<string, number>> = {};
    for (const r of metrics.timeline.weekly_by_status) {
      if (excludedStatuses.has(r.status)) continue;
      const w = fmtWeek(r.week);
      if (!byWeek[w]) byWeek[w] = { week: w as any };
      (byWeek[w] as any)[r.status] = r.count;
    }
    return Object.values(byWeek);
  }, [metrics, excludedStatuses]);

  const activeStatuses = useMemo(() => {
    const set = new Set<string>();
    for (const r of metrics.timeline.weekly_by_status) {
      if (!excludedStatuses.has(r.status)) set.add(r.status);
    }
    return Array.from(set);
  }, [metrics, excludedStatuses]);

  const doneCount = metrics.tasks.by_status.find((s) => s.name === "done")?.value ?? 0;
  const inProgressCount = metrics.tasks.by_status.find((s) => s.name === "in-progress")?.value ?? 0;
  const blockedCount = metrics.tasks.by_status.find((s) => s.name === "blocked")?.value ?? 0;
  const completionPct = filteredTotal > 0 ? Math.round((doneCount / filteredTotal) * 100) : 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm font-medium text-muted-foreground">Exclude:</span>
        {ALL_STATUSES.map((s) => (
          <button
            key={s}
            onClick={() => toggleStatus(s)}
            className={cn(
              "px-2.5 py-1 rounded-full text-xs font-medium border transition-all",
              excludedStatuses.has(s)
                ? "bg-muted text-muted-foreground border-border line-through opacity-60"
                : "border-transparent text-white"
            )}
            style={!excludedStatuses.has(s) ? { backgroundColor: STATUS_COLORS[s] } : undefined}
          >
            {s}
          </button>
        ))}
      </div>

      <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
        <StatCard label="Filtered Tasks" value={filteredTotal} sub={`of ${metrics.tasks.total}`} />
        <StatCard label="In Progress" value={inProgressCount} />
        <StatCard label="Done" value={doneCount} sub={`${completionPct}%`} />
        <StatCard label="Blocked" value={blockedCount} />
        <StatCard label="Requirements" value={metrics.requirements.total} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <Card>
          <CardHeader><CardTitle className="text-lg">Weekly Activity (12 weeks)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={280}>
              <BarChart data={weekly}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="week" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Legend />
                <Bar dataKey="created" fill="#8b5cf6" radius={[4, 4, 0, 0]} name="Created" />
                <Bar dataKey="completed" fill="#34d399" radius={[4, 4, 0, 0]} name="Completed" />
                <Bar dataKey="cancelled" fill="#6b7280" radius={[4, 4, 0, 0]} name="Cancelled" />
              </BarChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle className="text-lg">Monthly Activity (12 months)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={280}>
              <BarChart data={monthly}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="month" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Legend />
                <Bar dataKey="created" fill="#8b5cf6" radius={[4, 4, 0, 0]} name="Created" />
                <Bar dataKey="completed" fill="#34d399" radius={[4, 4, 0, 0]} name="Completed" />
                <Bar dataKey="blocked" fill="#f87171" radius={[4, 4, 0, 0]} name="Blocked" />
              </BarChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <Card>
          <CardHeader><CardTitle className="text-lg">Weekly by Status (stacked)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={300}>
              <BarChart data={weeklyStacked}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="week" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Legend />
                {activeStatuses.map((s) => (
                  <Bar key={s} dataKey={s} stackId="status" fill={STATUS_COLORS[s] || "#94a3b8"} name={s} />
                ))}
              </BarChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle className="text-lg">Burndown (12 weeks)</CardTitle></CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={300}>
              <AreaChart data={burndown}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="week" tick={tickStyle} />
                <YAxis tick={tickStyle} />
                <Tooltip contentStyle={tooltipStyle} />
                <Area type="monotone" dataKey="remaining" stroke="#f87171" fill="#f87171" fillOpacity={0.15} strokeWidth={2} name="Remaining" />
                <Area type="monotone" dataKey="done" stroke="#34d399" fill="#34d399" fillOpacity={0.15} strokeWidth={2} name="Done" />
              </AreaChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
        <ChartCard title="Tasks by Status" data={filteredStatusData} colors={STATUS_COLORS} type="pie" />
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
              <Bar dataKey="value" radius={[4, 4, 0, 0]}>
                {data.map((e) => <Cell key={e.name} fill={colors[e.name] || "#8b5cf6"} />)}
              </Bar>
            </BarChart>
          )}
        </ResponsiveContainer>
      </CardContent>
    </Card>
  );
}
