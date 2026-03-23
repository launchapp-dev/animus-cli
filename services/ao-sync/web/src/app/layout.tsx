import { useEffect } from "react";
import { Outlet, Link, useNavigate, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { LayoutDashboard, Settings, FolderGit2, LogOut } from "lucide-react";
import { authClient } from "@/lib/auth-client";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

export function Layout() {
  const navigate = useNavigate();
  const location = useLocation();
  const session = authClient.useSession();

  useEffect(() => {
    if (!session.isPending && !session.data) {
      navigate("/login");
    }
  }, [session.isPending, session.data, navigate]);

  const { data: projectsData } = useQuery({
    queryKey: ["projects"],
    queryFn: () => api.projects.list(),
    enabled: !!session.data,
  });

  if (session.isPending) {
    return (
      <div className="flex h-screen items-center justify-center">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  if (!session.data) return null;

  const projects = projectsData?.projects ?? [];

  return (
    <div className="flex h-screen">
      <aside className="w-60 border-r bg-card flex flex-col">
        <div className="p-4 border-b">
          <h1 className="text-lg font-bold">AO Sync</h1>
          <p className="text-xs text-muted-foreground">{session.data.user.email}</p>
        </div>

        <nav className="flex-1 p-2 space-y-1 overflow-y-auto">
          <Link to="/dashboard">
            <Button
              variant={location.pathname === "/dashboard" ? "secondary" : "ghost"}
              className="w-full justify-start"
              size="sm"
            >
              <LayoutDashboard className="h-4 w-4 mr-2" />
              Dashboard
            </Button>
          </Link>

          {projects.length > 0 && (
            <div className="pt-2">
              <p className="px-3 py-1 text-xs font-medium text-muted-foreground uppercase">Projects</p>
              {projects.map((p) => (
                <Link key={p.id} to={`/projects/${p.id}`}>
                  <Button
                    variant={location.pathname.startsWith(`/projects/${p.id}`) ? "secondary" : "ghost"}
                    className="w-full justify-start text-left"
                    size="sm"
                  >
                    <FolderGit2 className="h-4 w-4 mr-2 shrink-0" />
                    <span className="truncate">{p.name}</span>
                  </Button>
                </Link>
              ))}
            </div>
          )}

          <div className="pt-2">
            <Link to="/settings">
              <Button
                variant={location.pathname === "/settings" ? "secondary" : "ghost"}
                className="w-full justify-start"
                size="sm"
              >
                <Settings className="h-4 w-4 mr-2" />
                Settings
              </Button>
            </Link>
          </div>
        </nav>

        <div className="p-2 border-t">
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-start text-muted-foreground"
            onClick={async () => {
              await authClient.signOut();
              navigate("/login");
            }}
          >
            <LogOut className="h-4 w-4 mr-2" />
            Sign out
          </Button>
        </div>
      </aside>

      <main className="flex-1 overflow-y-auto p-6">
        <Outlet />
      </main>
    </div>
  );
}
