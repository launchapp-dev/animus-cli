import {
  createContext,
  ReactNode,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";

import { api, ProjectSummary } from "../lib/api/client";

type ProjectContextSource = "route-param" | "cached-selection" | "server-active" | "none";

export type ProjectContextValue = {
  activeProjectId: string | null;
  source: ProjectContextSource;
  projects: ProjectSummary[];
  setActiveProjectId: (projectId: string | null) => void;
};

export type ResolveProjectContextInput = {
  routeProjectId: string | null;
  cachedProjectId: string | null;
  serverActiveProjectId: string | null;
};

const STORAGE_KEY = "ao.web.active_project";

const ProjectContext = createContext<ProjectContextValue | null>(null);

export function resolveProjectContext(
  input: ResolveProjectContextInput,
): Pick<ProjectContextValue, "activeProjectId" | "source"> {
  if (input.routeProjectId) {
    return {
      activeProjectId: input.routeProjectId,
      source: "route-param",
    };
  }

  if (input.cachedProjectId) {
    return {
      activeProjectId: input.cachedProjectId,
      source: "cached-selection",
    };
  }

  if (input.serverActiveProjectId) {
    return {
      activeProjectId: input.serverActiveProjectId,
      source: "server-active",
    };
  }

  return {
    activeProjectId: null,
    source: "none",
  };
}

export function ProjectContextProvider(props: {
  routeProjectId: string | null;
  children: ReactNode;
}) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [serverActiveProjectId, setServerActiveProjectId] = useState<string | null>(null);
  const [cachedProjectId, setCachedProjectId] = useState<string | null>(() => {
    if (typeof window === "undefined") {
      return null;
    }

    return window.localStorage.getItem(STORAGE_KEY);
  });

  useEffect(() => {
    let isCancelled = false;

    void api.projectsList().then((result) => {
      if (isCancelled || result.kind === "error") {
        return;
      }

      setProjects(result.data);
    });

    void api.projectsActive().then((result) => {
      if (isCancelled || result.kind === "error") {
        return;
      }

      setServerActiveProjectId(result.data?.id ?? null);
    });

    return () => {
      isCancelled = true;
    };
  }, []);

  const resolved = resolveProjectContext({
    routeProjectId: props.routeProjectId,
    cachedProjectId,
    serverActiveProjectId,
  });

  const value = useMemo<ProjectContextValue>(() => {
    return {
      activeProjectId: resolved.activeProjectId,
      source: resolved.source,
      projects,
      setActiveProjectId: (projectId) => {
        setCachedProjectId(projectId);

        if (typeof window !== "undefined") {
          if (projectId) {
            window.localStorage.setItem(STORAGE_KEY, projectId);
          } else {
            window.localStorage.removeItem(STORAGE_KEY);
          }
        }
      },
    };
  }, [projects, resolved.activeProjectId, resolved.source]);

  return <ProjectContext.Provider value={value}>{props.children}</ProjectContext.Provider>;
}

export function useProjectContext() {
  const context = useContext(ProjectContext);

  if (!context) {
    throw new Error("useProjectContext must be used inside ProjectContextProvider");
  }

  return context;
}
