import { FormEvent, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { toast } from "sonner";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  ProjectsDocument,
  ProjectDetailDocument,
  CreateProjectDocument,
  UpdateProjectDocument,
  DeleteProjectDocument,
  LoadProjectDocument,
  ArchiveProjectDocument,
  RequirementDetailDocument,
} from "@/lib/graphql/generated/graphql";
import { statusColor, priorityColor, PageLoading, PageError } from "./shared";

export function ProjectsPage() {
  const [result, reexecute] = useQuery({ query: ProjectsDocument });
  const [, createProject] = useMutation(CreateProjectDocument);
  const { data, fetching, error } = result;
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [description, setDescription] = useState("");

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const projects = data?.projects ?? [];
  const active = data?.projectsActive ?? [];

  const onCreateProject = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !path.trim()) return;
    const { error: err } = await createProject({ name: name.trim(), path: path.trim(), description: description.trim() || undefined });
    if (err) { toast.error(err.message); return; }
    toast.success(`Project "${name.trim()}" created.`);
    setName(""); setPath(""); setDescription(""); setShowCreate(false);
    reexecute({ requestPolicy: "network-only" });
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold tracking-tight">Projects</h1>
        <Button size="sm" onClick={() => setShowCreate(!showCreate)}>{showCreate ? "Cancel" : "New Project"}</Button>
      </div>
      {active.length > 0 && (
        <p className="text-sm text-muted-foreground">Active: {active.map((p) => p.name).join(", ")}</p>
      )}

      {showCreate && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4"><CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Create Project</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={onCreateProject} className="space-y-3">
              <Input placeholder="Project name" value={name} onChange={(e) => setName(e.target.value)} required />
              <Input placeholder="Project path (e.g. /path/to/project)" value={path} onChange={(e) => setPath(e.target.value)} required />
              <Input placeholder="Description (optional)" value={description} onChange={(e) => setDescription(e.target.value)} />
              <Button type="submit" size="sm">Create</Button>
            </form>
          </CardContent>
        </Card>
      )}

      {projects.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No projects found.</p>
      ) : (
        <div className="grid md:grid-cols-2 lg:grid-cols-3 gap-4">
          {projects.map((p) => (
            <Link key={p.id} to={`/projects/${p.id}`}>
              <Card className="border-border/40 bg-card/60 hover:border-border/60 transition-colors">
                <CardContent className="pt-4">
                  <p className="font-medium">{p.name}</p>
                  {p.path && <p className="text-xs text-muted-foreground truncate">{p.path}</p>}
                  {p.description && <p className="text-sm text-muted-foreground mt-1 line-clamp-2">{p.description}</p>}
                  {p.archived && <Badge variant="outline" className="mt-1">archived</Badge>}
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}

export function ProjectDetailPage() {
  const { projectId } = useParams();
  const navigate = useNavigate();
  const [result, reexecute] = useQuery({ query: ProjectDetailDocument, variables: { id: projectId! } });
  const [, updateProject] = useMutation(UpdateProjectDocument);
  const [, deleteProject] = useMutation(DeleteProjectDocument);
  const [, loadProject] = useMutation(LoadProjectDocument);
  const [, archiveProject] = useMutation(ArchiveProjectDocument);
  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editType, setEditType] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [initialized, setInitialized] = useState(false);

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const project = data?.project;
  if (!project) return <PageError message={`Project ${projectId} not found.`} />;

  if (project && !initialized) {
    setEditName(project.name ?? "");
    setEditDescription(project.description ?? "");
    setEditType(project.type ?? "");
    setInitialized(true);
  }

  const onSave = async (e: FormEvent) => {
    e.preventDefault();
    const { error: err } = await updateProject({
      id: projectId!,
      name: editName.trim() || undefined,
      description: editDescription.trim() || undefined,
      projectType: editType.trim() || undefined,
    });
    if (err) { toast.error(err.message); return; }
    toast.success("Project updated.");
    setEditing(false);
    setInitialized(false);
    reexecute({ requestPolicy: "network-only" });
  };

  const onDelete = async () => {
    const { error: err } = await deleteProject({ id: projectId! });
    if (err) { toast.error(err.message); return; }
    navigate("/projects", { replace: true });
  };

  const onLoad = async () => {
    const { error: err } = await loadProject({ id: projectId! });
    if (err) toast.error(err.message);
    else toast.success("Project set as active.");
  };

  const onArchive = async () => {
    const { error: err } = await archiveProject({ id: projectId! });
    if (err) { toast.error(err.message); return; }
    toast.success("Project archived.");
    setInitialized(false);
    reexecute({ requestPolicy: "network-only" });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{project.name}</h1>
          {project.path && <p className="text-sm text-muted-foreground">{project.path}</p>}
        </div>
        <div className="flex items-center gap-2">
          {project.type && <Badge variant="outline">{project.type}</Badge>}
          {project.archived && <Badge variant="destructive">archived</Badge>}
        </div>
      </div>

      {project.description && !editing && <p className="text-sm">{project.description}</p>}

      {(project.techStack ?? []).length > 0 && (
        <div className="flex gap-2 flex-wrap">
          {project.techStack!.map((t) => <Badge key={t} variant="outline">{t}</Badge>)}
        </div>
      )}

      <div className="flex gap-2 flex-wrap">
        <Button size="sm" variant="outline" onClick={() => setEditing(!editing)}>{editing ? "Cancel Edit" : "Edit"}</Button>
        <Button size="sm" variant="outline" onClick={onLoad}>Set Active</Button>
        {!project.archived && <Button size="sm" variant="outline" onClick={onArchive}>Archive</Button>}
        {confirmDelete ? (
          <>
            <Button size="sm" variant="destructive" onClick={onDelete}>Confirm Delete</Button>
            <Button size="sm" variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
          </>
        ) : (
          <Button size="sm" variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={() => setConfirmDelete(true)}>Delete</Button>
        )}
      </div>

      {editing && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4"><CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Edit Project</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={onSave} className="space-y-3">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
                <Input value={editName} onChange={(e) => setEditName(e.target.value)} />
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
                <Textarea value={editDescription} onChange={(e) => setEditDescription(e.target.value)} rows={3} />
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</label>
                <Input value={editType} onChange={(e) => setEditType(e.target.value)} placeholder="e.g., rust, node, python" />
              </div>
              <Button type="submit" size="sm">Save Changes</Button>
            </form>
          </CardContent>
        </Card>
      )}

      <Link to="/projects"><Button variant="outline" size="sm">Back to Projects</Button></Link>
    </div>
  );
}

export function RequirementDetailPage() {
  const params = useParams();
  const requirementId = params.requirementId ?? params.projectId ?? "";
  const [result] = useQuery({ query: RequirementDetailDocument, variables: { id: requirementId } });

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const req = data?.requirement;
  if (!req) return <PageError message={`Requirement ${requirementId} not found.`} />;

  return (
    <div className="space-y-4">
      <div>
        <p className="text-sm text-muted-foreground font-mono">{req.id}</p>
        <h1 className="text-2xl font-semibold tracking-tight">{req.title}</h1>
        <div className="flex gap-2 mt-2">
          <Badge variant={statusColor(req.statusRaw ?? "")}>{req.statusRaw}</Badge>
          <Badge variant={priorityColor(req.priorityRaw ?? "")}>{req.priorityRaw}</Badge>
          {req.requirementType && <Badge variant="outline">{req.requirementType}</Badge>}
        </div>
      </div>
      {req.description && (
        <Card className="border-border/40 bg-card/60">
          <CardContent className="pt-4 text-sm whitespace-pre-wrap">{req.description}</CardContent>
        </Card>
      )}
      {(req.linkedTaskIds ?? []).length > 0 && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4"><CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Linked Tasks</CardTitle></CardHeader>
          <CardContent>
            <div className="flex gap-2 flex-wrap">
              {req.linkedTaskIds!.map((id) => (
                <Link key={id} to={`/tasks/${id}`}><Badge variant="outline" className="font-mono">{id}</Badge></Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
