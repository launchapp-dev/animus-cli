import { useState } from "react";
import { Link, useLocation } from "react-router-dom";
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
import { Separator } from "@/components/ui/separator";
import { WorkflowConfigDocument, SaveAgentProfileDocument, WorkflowConfigQuery } from "@/lib/graphql/generated/graphql";
import { PageLoading, PageError, SectionHeading } from "./shared";

function SettingsNav() {
  const { pathname } = useLocation();
  const links = [
    { to: "/settings/mcp", label: "MCP Servers" },
    { to: "/settings/agents", label: "Agent Profiles" },
    { to: "/settings/daemon", label: "Daemon" },
  ];
  return (
    <div className="flex gap-2 mb-4">
      {links.map((l) => (
        <Link
          key={l.to}
          to={l.to}
          className={`text-sm px-2 py-1 rounded-md transition-colors ${
            pathname === l.to
              ? "text-primary font-medium bg-primary/8"
              : "text-muted-foreground hover:text-foreground/80"
          }`}
        >
          {l.label}
        </Link>
      ))}
    </div>
  );
}

export function McpServersPage() {
  const [result] = useQuery({ query: WorkflowConfigDocument });
  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const config = data?.workflowConfig;
  const mcpServers = config?.mcpServers ?? [];
  const tools = config?.tools ?? [];
  const schedules = config?.schedules ?? [];

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">MCP Servers</h1>
        <p className="text-sm text-muted-foreground mt-1">Configure MCP tool servers for agent workflows</p>
      </div>

      <SettingsNav />

      {mcpServers.length === 0 ? (
        <Card className="border-border/40 bg-card/60">
          <CardContent className="pt-3 pb-3 px-4">
            <p className="text-sm text-muted-foreground text-center py-4">No MCP servers configured.</p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {mcpServers.map((srv) => (
            <Card key={srv.name} className="border-border/40 bg-card/60">
              <CardHeader className="pb-2 pt-3 px-4">
                <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">MCP Server</CardTitle>
              </CardHeader>
              <CardContent className="px-4 pb-4 space-y-3">
                <p className="font-mono text-primary text-sm">{srv.name}</p>
                <p className="font-mono text-xs text-foreground/70">{srv.command} {srv.args.join(" ")}</p>
                {srv.transport && (
                  <Badge variant="outline" className="text-[10px] h-4 px-1.5">{srv.transport}</Badge>
                )}
                {srv.tools.length > 0 && (
                  <div className="flex flex-wrap gap-1">
                    {srv.tools.map((t) => (
                      <Badge key={t} variant="secondary" className="text-[10px] h-4 px-1.5 font-mono">{t}</Badge>
                    ))}
                  </div>
                )}
                {srv.env.length > 0 && (
                  <div className="space-y-1">
                    {srv.env.map((e) => (
                      <div key={e.key} className="flex items-center gap-2 text-xs">
                        <span className="text-muted-foreground/60 font-mono">{e.key}</span>
                        <span className="text-foreground/70 font-mono">{e.value}</span>
                      </div>
                    ))}
                  </div>
                )}
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {tools.length > 0 && (
        <div className="space-y-3">
          <SectionHeading>Tools</SectionHeading>
          <div className="grid gap-3 md:grid-cols-2">
            {tools.map((t) => (
              <Card key={t.name} className="border-border/40 bg-card/60">
                <CardContent className="pt-3 pb-3 px-4 space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="font-mono text-sm text-foreground/90">{t.name}</span>
                    <span className="text-xs text-muted-foreground font-mono">{t.executable}</span>
                  </div>
                  <div className="flex items-center gap-2">
                    {t.supportsMcp && <Badge variant="secondary" className="text-[10px] h-4 px-1.5">MCP</Badge>}
                    {t.supportsWrite && <Badge variant="secondary" className="text-[10px] h-4 px-1.5">Write</Badge>}
                    {t.contextWindow != null && (
                      <span className="text-xs text-muted-foreground">{t.contextWindow.toLocaleString()} ctx</span>
                    )}
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        </div>
      )}

      {schedules.length > 0 && (
        <div className="space-y-3">
          <SectionHeading>Schedules</SectionHeading>
          <div className="grid gap-3 md:grid-cols-2">
            {schedules.map((s) => (
              <Card key={s.id} className="border-border/40 bg-card/60">
                <CardContent className="pt-3 pb-3 px-4 space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-sm text-foreground/90">{s.id}</span>
                    <Badge variant={s.enabled ? "secondary" : "outline"} className="text-[10px] h-4 px-1.5">
                      {s.enabled ? "enabled" : "disabled"}
                    </Badge>
                  </div>
                  <p className="font-mono text-xs text-foreground/70">{s.cron}</p>
                  {s.workflowRef && (
                    <span className="text-xs text-muted-foreground">{s.workflowRef}</span>
                  )}
                </CardContent>
              </Card>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

type AgentProfileItem = NonNullable<WorkflowConfigQuery["workflowConfig"]>["agentProfiles"][number];

function ProfileCard({ p, onSaved }: { p: AgentProfileItem; onSaved: () => void }) {
  const [model, setModel] = useState(p.model ?? "");
  const [tool, setTool] = useState(p.tool ?? "");
  const [role, setRole] = useState(p.role ?? "");
  const [{ fetching }, executeSave] = useMutation(SaveAgentProfileDocument);

  const handleSave = async () => {
    const { error } = await executeSave({
      name: p.name,
      model: model || null,
      tool: tool || null,
      role: role || null,
    });
    if (error) toast.error(error.message);
    else {
      toast.success("Profile saved.");
      onSaved();
    }
  };

  return (
    <Card className="border-border/40 bg-card/60">
      <CardHeader className="pb-2 pt-3 px-4">
        <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Agent Profile</CardTitle>
      </CardHeader>
      <CardContent className="px-4 pb-4 space-y-3">
        <p className="font-mono text-primary text-sm">{p.name}</p>
        {p.description && <p className="text-xs text-muted-foreground">{p.description}</p>}
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground/60 w-10 shrink-0">role</span>
            <input
              className="flex-1 h-6 px-2 text-xs font-mono bg-muted/40 border border-border/40 rounded focus:outline-none focus:ring-1 focus:ring-primary/50"
              value={role}
              onChange={(e) => setRole(e.target.value)}
              placeholder="e.g. implementer"
            />
          </div>
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground/60 w-10 shrink-0">model</span>
            <input
              className="flex-1 h-6 px-2 text-xs font-mono bg-muted/40 border border-border/40 rounded focus:outline-none focus:ring-1 focus:ring-primary/50"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="e.g. claude-sonnet-4-6"
            />
          </div>
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground/60 w-10 shrink-0">tool</span>
            <input
              className="flex-1 h-6 px-2 text-xs font-mono bg-muted/40 border border-border/40 rounded focus:outline-none focus:ring-1 focus:ring-primary/50"
              value={tool}
              onChange={(e) => setTool(e.target.value)}
              placeholder="e.g. claude"
            />
          </div>
        </div>
        <div className="flex items-center justify-between">
          <div className="flex flex-wrap gap-1">
            {p.mcpServers.map((s) => (
              <Badge key={s} variant="secondary" className="text-[10px] h-4 px-1.5 font-mono">{s}</Badge>
            ))}
            {p.skills.map((s) => (
              <Badge key={s} variant="outline" className="text-[10px] h-4 px-1.5 font-mono">{s}</Badge>
            ))}
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-3 text-xs shrink-0"
            onClick={handleSave}
            disabled={fetching}
          >
            {fetching ? "Saving…" : "Save"}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

export function AgentProfilesPage() {
  const [result, reexecute] = useQuery({ query: WorkflowConfigDocument });
  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const config = data?.workflowConfig;
  const profiles = config?.agentProfiles ?? [];
  const catalog = config?.phaseCatalog ?? [];

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Agent Profiles</h1>
        <p className="text-sm text-muted-foreground mt-1">Configure agent model, tool, and role</p>
      </div>

      <SettingsNav />

      {profiles.length === 0 ? (
        <Card className="border-border/40 bg-card/60">
          <CardContent className="pt-3 pb-3 px-4">
            <p className="text-sm text-muted-foreground text-center py-4">No agent profiles configured.</p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {profiles.map((p) => (
            <ProfileCard key={p.name} p={p} onSaved={reexecute} />
          ))}
        </div>
      )}

      {catalog.length > 0 && (
        <div className="space-y-3">
          <SectionHeading>Phase Catalog</SectionHeading>
          <Card className="border-border/40 bg-card/60">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-36">ID</TableHead>
                  <TableHead>Label</TableHead>
                  <TableHead>Description</TableHead>
                  <TableHead>Category</TableHead>
                  <TableHead>Tags</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {catalog.map((c) => (
                  <TableRow key={c.id}>
                    <TableCell className="font-mono text-xs">{c.id}</TableCell>
                    <TableCell className="text-sm">{c.label}</TableCell>
                    <TableCell className="text-xs text-muted-foreground">{c.description}</TableCell>
                    <TableCell><Badge variant="outline" className="text-[10px] h-4 px-1.5">{c.category}</Badge></TableCell>
                    <TableCell>
                      <div className="flex flex-wrap gap-1">
                        {c.tags.map((t) => (
                          <Badge key={t} variant="secondary" className="text-[10px] h-4 px-1.5">{t}</Badge>
                        ))}
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Card>
        </div>
      )}
    </div>
  );
}

const DAEMON_SETTINGS = [
  {
    key: "auto_merge_enabled",
    label: "Auto Merge",
    description: "Automatically merge completed workflow branches into the base branch",
  },
  {
    key: "auto_pr_enabled",
    label: "Auto PR",
    description: "Automatically create pull requests for completed workflow branches",
  },
  {
    key: "auto_commit_before_merge",
    label: "Auto Commit Before Merge",
    description: "Commit any uncommitted changes before merging workflow branches",
  },
  {
    key: "auto_prune_worktrees_after_merge",
    label: "Auto Prune Worktrees",
    description: "Remove git worktrees after their branches have been merged",
  },
] as const;

function ToggleRow({ label, description, checked }: { label: string; description: string; checked: boolean }) {
  return (
    <div className="flex items-center justify-between py-3">
      <div className="space-y-0.5">
        <p className="text-sm font-medium text-foreground/90">{label}</p>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      <div
        className={`relative inline-flex h-5 w-9 shrink-0 cursor-default rounded-full border-2 border-transparent transition-colors ${
          checked ? "bg-primary" : "bg-muted"
        }`}
      >
        <span
          className={`pointer-events-none block h-4 w-4 rounded-full bg-background shadow-sm ring-0 transition-transform ${
            checked ? "translate-x-4" : "translate-x-0"
          }`}
        />
      </div>
    </div>
  );
}

export function DaemonConfigPage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Daemon Settings</h1>
        <p className="text-sm text-muted-foreground mt-1">Configure daemon automation behavior</p>
      </div>

      <SettingsNav />

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Automation</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4">
          {DAEMON_SETTINGS.map((setting, i) => (
            <div key={setting.key}>
              {i > 0 && <Separator />}
              <ToggleRow label={setting.label} description={setting.description} checked={false} />
            </div>
          ))}
        </CardContent>
      </Card>

      <div className="rounded-md border border-border/40 bg-muted/30 px-4 py-3">
        <p className="text-xs text-muted-foreground">
          Read-only view. Configure via <code className="font-mono text-[11px] bg-muted px-1 py-0.5 rounded">ao daemon config --auto-merge true/false</code>
        </p>
      </div>
    </div>
  );
}
