import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  NavLink,
  Outlet,
  useLocation,
  useNavigate,
} from "react-router-dom";
import {
  LayoutDashboard,
  ListTodo,
  GitBranch,
  Layers,
  FileText,
  Server,
  Bot,
  Activity,
  ClipboardCheck,
  Settings,
  Search,
  Menu,
  ChevronRight,
  X,
  Sun,
  Moon,
  Monitor,
  AlertTriangle,
  Wrench,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import {
  Dialog,
  DialogContent,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { GraphQLProvider } from "@/lib/graphql/provider";
import { Toaster } from "@/components/ui/sonner";
import { useTheme } from "./theme-provider";
import { useQuery } from "@/lib/graphql/client";
import { DashboardDocument } from "@/lib/graphql/generated/graphql";

export const NAV_GROUPS = [
  {
    label: "Operate",
    items: [
      { to: "/dashboard", label: "Dashboard", icon: LayoutDashboard, badgeKey: null },
      { to: "/tasks", label: "Tasks", icon: ListTodo, badgeKey: "tasks" as const },
      { to: "/workflows", label: "Workflows", icon: GitBranch, badgeKey: "workflows" as const },
      { to: "/queue", label: "Queue", icon: Layers, badgeKey: "queue" as const },
      { to: "/agents", label: "Agents", icon: Bot, badgeKey: "agents" as const },
    ],
  },
  {
    label: "Plan",
    items: [
      { to: "/planning/vision", label: "Vision", icon: FileText, badgeKey: null },
      { to: "/planning/requirements", label: "Requirements", icon: ClipboardCheck, badgeKey: null },
    ],
  },
  {
    label: "Monitor",
    items: [
      { to: "/events", label: "Events", icon: Activity, badgeKey: "events" as const },
      { to: "/errors", label: "Errors", icon: AlertTriangle, badgeKey: "errors" as const },
      { to: "/daemon", label: "Daemon", icon: Server, badgeKey: null },
    ],
  },
  {
    label: "Configure",
    items: [
      { to: "/workflows/builder", label: "Builder", icon: Wrench, badgeKey: null },
      { to: "/skills", label: "Skills", icon: Layers, badgeKey: null },
      { to: "/settings/mcp", label: "Settings", icon: Settings, badgeKey: null },
    ],
  },
] as const;

export const PRIMARY_NAV_ITEMS = NAV_GROUPS.flatMap(g => g.items);

export const MAIN_CONTENT_ID = "main-content";

export function AppShellLayout() {
  return (
    <GraphQLProvider>
      <AppShellFrame />
    </GraphQLProvider>
  );
}

function AppShellFrame() {
  const [mobileOpen, setMobileOpen] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();

  useEffect(() => {
    setMobileOpen(false);
  }, [location.pathname]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setCommandOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const breadcrumbs = useMemo(() => {
    return location.pathname
      .split("/")
      .filter(Boolean)
      .map((s) => s.replace(/-/g, " "));
  }, [location.pathname]);

  return (
    <div className="flex h-screen overflow-hidden bg-background text-foreground">
      <aside className="hidden md:flex w-60 flex-col border-r border-border/50 bg-[var(--ao-surface)]">
        <SidebarContent />
      </aside>

      <div className="flex flex-1 flex-col overflow-hidden">
        <header className="flex h-11 items-center gap-3 border-b border-border/50 px-4 bg-[var(--ao-surface)]/60 backdrop-blur-md">
          <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon" className="md:hidden h-7 w-7">
                <Menu className="h-4 w-4" />
                <span className="sr-only">Toggle navigation</span>
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="w-60 p-0 bg-[var(--ao-surface)] border-border/50">
              <SidebarContent />
            </SheetContent>
          </Sheet>

          <nav aria-label="Breadcrumb" className="flex items-center gap-1 text-xs text-muted-foreground min-w-0">
            {breadcrumbs.map((crumb, i) => (
              <span key={i} className="flex items-center gap-1 capitalize truncate">
                {i > 0 && <ChevronRight className="h-3 w-3 shrink-0 opacity-40" />}
                <span className={i === breadcrumbs.length - 1 ? "text-foreground/80 font-medium" : ""}>
                  {crumb}
                </span>
              </span>
            ))}
          </nav>

          <div className="ml-auto flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              className="hidden sm:flex items-center gap-2 text-muted-foreground text-[11px] h-7 px-2 rounded-md border border-border/50 bg-transparent hover:bg-accent/50"
              onClick={() => setCommandOpen(true)}
            >
              <Search className="h-3 w-3 opacity-50" />
              <span className="opacity-60">Search</span>
              <kbd className="ml-1 pointer-events-none border border-border/50 rounded px-1 py-px text-[9px] font-mono bg-muted/30">
                {"\u2318"}K
              </kbd>
            </Button>
          </div>
        </header>

        <main
          id={MAIN_CONTENT_ID}
          className="flex-1 overflow-y-auto p-5 md:p-6"
          tabIndex={-1}
        >
          <div className="ao-fade-in max-w-6xl">
            <Outlet />
          </div>
        </main>
      </div>

      <CommandPalette
        open={commandOpen}
        onOpenChange={setCommandOpen}
        navigate={navigate}
      />
      <ThemedToaster />
    </div>
  );
}

function useSidebarData() {
  const [result] = useQuery({ query: DashboardDocument });
  const data = result.data;

  const taskStats = data?.taskStats;
  const health = data?.daemonHealth;
  const agents = data?.agentRuns ?? [];

  const byStatus: Record<string, number> = taskStats?.byStatus ? JSON.parse(taskStats.byStatus) : {};
  const inProgress = byStatus["in-progress"] ?? 0;
  const blocked = byStatus["blocked"] ?? 0;

  return {
    daemonHealthy: health?.healthy ?? false,
    daemonStatus: health?.status ?? "unknown",
    agentCount: agents.length,
    badges: {
      tasks: taskStats?.total ?? 0,
      workflows: inProgress,
      queue: 0,
      agents: agents.length > 0 ? `${agents.length}/${health?.activeDaemons ?? "?"}` : null,
      events: null,
      errors: blocked > 0 ? blocked : null,
    } as Record<string, number | string | null>,
  };
}

function SidebarBadge({ value, badgeKey }: { value: number | string; badgeKey: string }) {
  if (value === 0 || value === "0") return null;
  const isError = badgeKey === "errors";
  return (
    <span className={`text-[10px] font-mono tabular-nums ${
      isError ? "text-destructive" : "text-muted-foreground/50"
    }`}>
      {value}
    </span>
  );
}

function SidebarContent() {
  const sidebarData = useSidebarData();
  const { theme, setTheme } = useTheme();

  return (
    <div className="flex h-full flex-col">
      <div className="px-4 pt-4 pb-3">
        <div className="flex items-center gap-2.5">
          <div className="h-8 w-8 rounded-lg bg-primary/15 border border-primary/25 flex items-center justify-center">
            <span className="text-xs font-mono font-bold text-primary">ao</span>
          </div>
          <div className="flex-1 min-w-0">
            <h1 className="text-sm font-semibold tracking-tight leading-none">AO</h1>
            <p className="text-[10px] text-muted-foreground/60 leading-none mt-0.5">Agent Orchestrator</p>
          </div>
        </div>
        <div className="flex items-center gap-1.5 mt-3 px-1">
          <span className={`h-1.5 w-1.5 rounded-full ${sidebarData.daemonHealthy ? "bg-[var(--ao-success)]" : "bg-destructive"}`} />
          <span className="text-[10px] text-muted-foreground/50">
            {sidebarData.daemonStatus}
          </span>
          {sidebarData.agentCount > 0 && (
            <span className="text-[10px] text-muted-foreground/30 ml-auto font-mono">
              {sidebarData.agentCount} agent{sidebarData.agentCount !== 1 ? "s" : ""}
            </span>
          )}
        </div>
      </div>

      <div className="h-px bg-border/50 mx-3" />

      <nav className="flex-1 overflow-y-auto px-2 py-2" aria-label="Primary">
        {NAV_GROUPS.map((group) => (
          <div key={group.label} className="mb-3">
            <p className="px-2.5 mb-1 text-[10px] uppercase tracking-wider text-muted-foreground/40 font-medium">
              {group.label}
            </p>
            <div className="space-y-0.5">
              {group.items.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) =>
                    `group flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-[13px] transition-all duration-150 relative ${
                      isActive
                        ? "text-primary font-medium bg-primary/8"
                        : "text-muted-foreground hover:text-foreground/80 hover:bg-accent/40"
                    }`
                  }
                >
                  {({ isActive }) => (
                    <>
                      {isActive && (
                        <div className="absolute left-0 top-1/2 -translate-y-1/2 w-[2px] h-4 rounded-full bg-primary" />
                      )}
                      <item.icon className={`h-3.5 w-3.5 shrink-0 transition-colors ${isActive ? "text-primary" : "text-muted-foreground/60 group-hover:text-muted-foreground"}`} />
                      <span className="flex-1">{item.label}</span>
                      {item.badgeKey && sidebarData.badges[item.badgeKey] != null && (
                        <SidebarBadge value={sidebarData.badges[item.badgeKey]!} badgeKey={item.badgeKey} />
                      )}
                    </>
                  )}
                </NavLink>
              ))}
            </div>
          </div>
        ))}
      </nav>

      <div className="h-px bg-border/50 mx-3" />

      <div className="px-3 py-2.5 space-y-2">
        <NavLink
          to="/reviews/handoff"
          className={({ isActive }) =>
            `flex items-center gap-2 text-[11px] transition-colors ${
              isActive ? "text-primary" : "text-muted-foreground/60 hover:text-foreground/70"
            }`
          }
        >
          <ClipboardCheck className="h-3 w-3" />
          Review Handoff
        </NavLink>

        <div className="flex items-center justify-between">
          <div className="flex items-center gap-1">
            {(["system", "dark", "light"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setTheme(t)}
                className={`h-6 w-6 rounded-md flex items-center justify-center transition-colors ${
                  theme === t ? "bg-accent text-foreground" : "text-muted-foreground/40 hover:text-muted-foreground"
                }`}
                aria-label={`${t} theme`}
              >
                {t === "system" ? <Monitor className="h-3 w-3" /> :
                 t === "dark" ? <Moon className="h-3 w-3" /> :
                 <Sun className="h-3 w-3" />}
              </button>
            ))}
          </div>
          <span className="text-[9px] text-muted-foreground/30 font-mono">v0.1.0</span>
        </div>
      </div>
    </div>
  );
}

function ThemedToaster() {
  const { resolvedTheme } = useTheme();
  return <Toaster theme={resolvedTheme} position="bottom-right" richColors />;
}

function CommandPalette({
  open,
  onOpenChange,
  navigate,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  navigate: ReturnType<typeof useNavigate>;
}) {
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setQuery("");
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  const goTo = useCallback(
    (path: string) => {
      onOpenChange(false);
      navigate(path);
    },
    [navigate, onOpenChange],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key !== "Enter" || !query.trim()) return;
      const q = query.trim().toUpperCase();

      if (q.startsWith("TASK-")) {
        goTo(`/tasks/${q}`);
      } else if (q.startsWith("WF-") || q.startsWith("WORKFLOW-")) {
        goTo(`/workflows/${q}`);
      } else if (q.startsWith("REQ-")) {
        goTo(`/planning/requirements/${q}`);
      } else {
        goTo(`/tasks?search=${encodeURIComponent(query.trim())}`);
      }
    },
    [query, goTo],
  );

  const filteredNav = useMemo(() => {
    if (!query.trim()) return PRIMARY_NAV_ITEMS;
    const q = query.toLowerCase();
    return PRIMARY_NAV_ITEMS.filter(
      (item) =>
        item.label.toLowerCase().includes(q) ||
        item.to.toLowerCase().includes(q),
    );
  }, [query]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md p-0 gap-0 bg-[var(--ao-surface)] border-border/50 shadow-2xl shadow-black/40">
        <div className="flex items-center border-b border-border/50 px-3">
          <Search className="h-4 w-4 text-muted-foreground/50 shrink-0" />
          <Input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Go to TASK-XXX, REQ-XXX, or search..."
            className="border-0 focus-visible:ring-0 shadow-none bg-transparent text-sm"
          />
          {query && (
            <button type="button" onClick={() => setQuery("")} className="text-muted-foreground hover:text-foreground">
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
        <div className="max-h-64 overflow-y-auto p-1">
          {filteredNav.map((item) => (
            <button
              key={item.to}
              type="button"
              onClick={() => goTo(item.to)}
              className="flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm text-left text-muted-foreground hover:text-foreground hover:bg-accent/50 transition-colors"
            >
              <item.icon className="h-4 w-4 opacity-50" />
              {item.label}
            </button>
          ))}
          {query.trim() && (
            <p className="px-3 py-2 text-[11px] text-muted-foreground/60 font-mono">
              {"\u23CE"} Enter to jump to ID or search tasks
            </p>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

