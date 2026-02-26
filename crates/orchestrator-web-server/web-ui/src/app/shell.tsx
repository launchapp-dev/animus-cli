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
export const MAIN_CONTENT_ID = "main-content";

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
  const mainContentRef = useRef<HTMLElement | null>(null);
  const menuButtonRef = useRef<HTMLButtonElement | null>(null);
  const primaryNavRef = useRef<HTMLElement | null>(null);
  const shouldRestoreMenuButtonFocus = useRef(false);

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
    shouldRestoreMenuButtonFocus.current = false;
    setIsMobileMenuOpen(false);
  }, [location.pathname]);

  useEffect(() => {
    const section = location.pathname.split("/")[1] ?? "";

    if (section !== previousSection.current) {
      window.scrollTo(0, 0);
    }

    previousSection.current = section;
    mainContentRef.current?.focus();
  }, [location.pathname]);

  useEffect(() => {
    if (isMobileMenuOpen) {
      const firstNavControl = primaryNavRef.current?.querySelector<HTMLElement>(
        "a[href],button:not([disabled]),[tabindex]:not([tabindex='-1'])",
      );
      firstNavControl?.focus();
      return;
    }

    if (shouldRestoreMenuButtonFocus.current) {
      menuButtonRef.current?.focus();
      shouldRestoreMenuButtonFocus.current = false;
    }
  }, [isMobileMenuOpen]);

  useEffect(() => {
    if (!isMobileMenuOpen) {
      return;
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        shouldRestoreMenuButtonFocus.current = true;
        setIsMobileMenuOpen(false);
      }
    };

    window.addEventListener("keydown", onKeyDown);

    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [isMobileMenuOpen]);

  const onProjectSelectionChange = (projectId: string) => {
    const normalizedProjectId = projectId.length > 0 ? projectId : null;
    projectContext.setActiveProjectId(normalizedProjectId);

    if (normalizedProjectId) {
      navigate(`/projects/${normalizedProjectId}`);
    }
  };

  return (
    <div className="app-shell">
      <a className="skip-link" href={`#${MAIN_CONTENT_ID}`}>
        Skip to main content
      </a>

      <div className="app-layout">
        {isMobileMenuOpen ? (
          <button
            aria-label="Close navigation menu"
            className="mobile-overlay"
            onClick={() => {
              shouldRestoreMenuButtonFocus.current = true;
              setIsMobileMenuOpen(false);
            }}
            type="button"
          />
        ) : null}

        <aside
          aria-label="Primary navigation"
          className="sidebar"
          data-open={isMobileMenuOpen}
        >
          <h1 className="brand">AO Web</h1>
          <p className="brand-subtitle">Agent Orchestrator web shell</p>

          <nav
            aria-label="Primary"
            className="primary-nav"
            id="primary-navigation"
            ref={primaryNavRef}
          >
            {PRIMARY_NAV_ITEMS.map((item) => (
              <NavLink
                key={item.to}
                onClick={() => {
                  shouldRestoreMenuButtonFocus.current = false;
                  setIsMobileMenuOpen(false);
                }}
                to={item.to}
              >
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
                aria-label={isMobileMenuOpen ? "Close primary navigation" : "Open primary navigation"}
                onClick={() =>
                  setIsMobileMenuOpen((current) => {
                    if (current) {
                      shouldRestoreMenuButtonFocus.current = true;
                    }
                    return !current;
                  })
                }
                ref={menuButtonRef}
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

          <main className="content-scroll" id={MAIN_CONTENT_ID} ref={mainContentRef} tabIndex={-1}>
            <Outlet />
          </main>
        </div>
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
