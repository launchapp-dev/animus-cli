import { useEffect, useMemo, useRef, useState } from "react";
import {
  NavLink,
  Outlet,
  useLocation,
  useMatches,
  useNavigate,
} from "react-router-dom";

import { ProjectContextProvider, useProjectContext } from "./project-context";

export const PRIMARY_NAV_ITEMS = [
  { to: "/dashboard", label: "Dashboard" },
  { to: "/daemon", label: "Daemon" },
  { to: "/projects", label: "Projects" },
  { to: "/tasks", label: "Tasks" },
  { to: "/workflows", label: "Workflows" },
  { to: "/events", label: "Events" },
  { to: "/reviews/handoff", label: "Review Handoff" },
] as const;

export function AppShellLayout() {
  const routeProjectId = useRouteProjectId();

  return (
    <ProjectContextProvider routeProjectId={routeProjectId}>
      <AppShellFrame />
    </ProjectContextProvider>
  );
}

function AppShellFrame() {
  const [isMobileMenuOpen, setIsMobileMenuOpen] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();
  const previousSection = useRef<string | null>(null);

  const projectContext = useProjectContext();

  const breadcrumb = useMemo(() => {
    const parts = location.pathname
      .split("/")
      .filter(Boolean)
      .map((segment) => segment.replace(/-/g, " "));

    if (parts.length === 0) {
      return "home";
    }

    return parts.join(" / ");
  }, [location.pathname]);

  useEffect(() => {
    setIsMobileMenuOpen(false);
  }, [location.pathname]);

  useEffect(() => {
    const section = location.pathname.split("/")[1] ?? "";

    if (section !== previousSection.current) {
      window.scrollTo(0, 0);
    }

    previousSection.current = section;
  }, [location.pathname]);

  const onProjectSelectionChange = (projectId: string) => {
    const normalizedProjectId = projectId.length > 0 ? projectId : null;
    projectContext.setActiveProjectId(normalizedProjectId);

    if (normalizedProjectId) {
      navigate(`/projects/${normalizedProjectId}`);
    }
  };

  return (
    <div className="app-layout">
      {isMobileMenuOpen ? (
        <button
          aria-label="Close navigation menu"
          className="mobile-overlay"
          onClick={() => setIsMobileMenuOpen(false)}
          type="button"
        />
      ) : null}

      <aside className="sidebar" data-open={isMobileMenuOpen}>
        <h1 className="brand">AO Web</h1>
        <p className="brand-subtitle">Agent Orchestrator web shell</p>

        <nav aria-label="Primary" className="primary-nav" id="primary-navigation">
          {PRIMARY_NAV_ITEMS.map((item) => (
            <NavLink key={item.to} to={item.to}>
              {item.label}
            </NavLink>
          ))}
        </nav>
      </aside>

      <div className="main-column">
        <header className="topbar">
          <div className="mobile-actions">
            <button
              type="button"
              aria-expanded={isMobileMenuOpen}
              aria-controls="primary-navigation"
              onClick={() => setIsMobileMenuOpen((current) => !current)}
            >
              Menu
            </button>
          </div>

          <div className="topbar-row">
            <p className="breadcrumbs" aria-live="polite">
              {breadcrumb}
            </p>
          </div>

          <div className="project-frame">
            <label>
              <span className="visually-hidden">Select active project</span>
              <select
                value={projectContext.activeProjectId ?? ""}
                onChange={(event) => onProjectSelectionChange(event.target.value)}
              >
                <option value="">No active project</option>
                {projectContext.projects.map((project) => (
                  <option key={project.id} value={project.id}>
                    {project.name}
                  </option>
                ))}
              </select>
            </label>

            <span className="badge" aria-label="Active project source">
              {projectContext.activeProjectId ?? "none"} ({projectContext.source})
            </span>
          </div>
        </header>

        <main className="content-scroll" id="main-content" tabIndex={-1}>
          <Outlet />
        </main>
      </div>
    </div>
  );
}

function useRouteProjectId(): string | null {
  const matches = useMatches();

  for (let index = matches.length - 1; index >= 0; index -= 1) {
    const params = matches[index].params as Record<string, string | undefined>;
    const projectId = params.projectId;
    if (projectId) {
      return projectId;
    }
  }

  return null;
}
