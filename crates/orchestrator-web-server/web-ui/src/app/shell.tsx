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
  Search,
  Menu,
  ChevronRight,
  X,
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

export const PRIMARY_NAV_ITEMS = [
  { to: "/dashboard", label: "Dashboard", icon: LayoutDashboard },
  { to: "/tasks", label: "Tasks", icon: ListTodo },
  { to: "/workflows", label: "Workflows", icon: GitBranch },
  { to: "/queue", label: "Queue", icon: Layers },
  { to: "/planning", label: "Planning", icon: FileText },
  { to: "/daemon", label: "Daemon", icon: Server },
  { to: "/agents", label: "Agents", icon: Bot },
  { to: "/events", label: "Events", icon: Activity },
  { to: "/reviews/handoff", label: "Review", icon: ClipboardCheck },
] as const;

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
      <Toaster theme="dark" position="bottom-right" richColors />
    </div>
  );
}

function SidebarContent() {
  return (
    <div className="flex h-full flex-col">
      <div className="px-4 py-4 flex items-center gap-2">
        <div className="h-7 w-7 rounded-md bg-primary/15 border border-primary/25 flex items-center justify-center">
          <span className="text-[11px] font-mono font-bold text-primary">ao</span>
        </div>
        <div>
          <h1 className="text-sm font-semibold tracking-tight leading-none">AO</h1>
          <p className="text-[10px] text-muted-foreground leading-none mt-0.5">Agent Orchestrator</p>
        </div>
      </div>
      <div className="h-px bg-border/50 mx-3" />
      <nav className="flex-1 px-2 py-2 space-y-0.5" aria-label="Primary">
        {PRIMARY_NAV_ITEMS.map((item) => (
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
                {item.label}
              </>
            )}
          </NavLink>
        ))}
      </nav>
    </div>
  );
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

