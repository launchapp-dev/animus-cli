import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, FolderGit2 } from "lucide-react";
import { toast } from "sonner";
import { authClient } from "@/lib/auth-client";
import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";

export function DashboardPage() {
  const queryClient = useQueryClient();
  const { data: projectsData, isLoading: projectsLoading } = useQuery({
    queryKey: ["projects"],
    queryFn: () => api.projects.list(),
  });

  const orgs = authClient.useListOrganizations();
  const [showNewOrg, setShowNewOrg] = useState(false);
  const [showNewProject, setShowNewProject] = useState(false);
  const [orgName, setOrgName] = useState("");
  const [orgSlug, setOrgSlug] = useState("");
  const [projectName, setProjectName] = useState("");
  const [projectRepo, setProjectRepo] = useState("");
  const [selectedOrg, setSelectedOrg] = useState("");

  const organizations = orgs.data ?? [];
  const projects = projectsData?.projects ?? [];

  async function createOrg(e: React.FormEvent) {
    e.preventDefault();
    try {
      await authClient.organization.create({ name: orgName, slug: orgSlug });
      setOrgName("");
      setOrgSlug("");
      setShowNewOrg(false);
      orgs.refetch();
      toast.success("Organization created");
    } catch (err: any) {
      toast.error(err.message);
    }
  }

  async function createProject(e: React.FormEvent) {
    e.preventDefault();
    try {
      await api.projects.create({ name: projectName, organizationId: selectedOrg, repoOriginUrl: projectRepo });
      setProjectName("");
      setProjectRepo("");
      setShowNewProject(false);
      queryClient.invalidateQueries({ queryKey: ["projects"] });
      toast.success("Project created");
    } catch (err: any) {
      toast.error(err.message);
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Dashboard</h1>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => setShowNewOrg(!showNewOrg)}>
            <Plus className="h-4 w-4 mr-1" /> Organization
          </Button>
          <Button size="sm" onClick={() => setShowNewProject(!showNewProject)} disabled={organizations.length === 0}>
            <Plus className="h-4 w-4 mr-1" /> Project
          </Button>
        </div>
      </div>

      {showNewOrg && (
        <Card>
          <CardHeader><CardTitle className="text-lg">New Organization</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={createOrg} className="flex gap-3 items-end">
              <div className="space-y-1 flex-1">
                <Label>Name</Label>
                <Input value={orgName} onChange={(e) => { setOrgName(e.target.value); setOrgSlug(e.target.value.toLowerCase().replace(/\s+/g, "-")); }} required />
              </div>
              <div className="space-y-1 flex-1">
                <Label>Slug</Label>
                <Input value={orgSlug} onChange={(e) => setOrgSlug(e.target.value)} required />
              </div>
              <Button type="submit">Create</Button>
            </form>
          </CardContent>
        </Card>
      )}

      {showNewProject && (
        <Card>
          <CardHeader><CardTitle className="text-lg">New Project</CardTitle></CardHeader>
          <CardContent>
            <form onSubmit={createProject} className="flex gap-3 items-end flex-wrap">
              <div className="space-y-1 flex-1 min-w-[200px]">
                <Label>Name</Label>
                <Input value={projectName} onChange={(e) => setProjectName(e.target.value)} placeholder="my-app" required />
              </div>
              <div className="space-y-1 flex-1 min-w-[200px]">
                <Label>Git Origin URL</Label>
                <Input value={projectRepo} onChange={(e) => setProjectRepo(e.target.value)} placeholder="https://github.com/org/repo.git" required />
              </div>
              <div className="space-y-1 min-w-[200px]">
                <Label>Organization</Label>
                <select
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                  value={selectedOrg}
                  onChange={(e) => setSelectedOrg(e.target.value)}
                  required
                >
                  <option value="">Select org...</option>
                  {organizations.map((o) => (
                    <option key={o.id} value={o.id}>{o.name}</option>
                  ))}
                </select>
              </div>
              <Button type="submit">Create</Button>
            </form>
          </CardContent>
        </Card>
      )}

      {organizations.length === 0 && !showNewOrg && (
        <Card>
          <CardContent className="py-8 text-center">
            <p className="text-muted-foreground mb-3">Create an organization to get started</p>
            <Button onClick={() => setShowNewOrg(true)}><Plus className="h-4 w-4 mr-1" /> Create Organization</Button>
          </CardContent>
        </Card>
      )}

      {projectsLoading ? (
        <div className="text-muted-foreground">Loading projects...</div>
      ) : projects.length === 0 && organizations.length > 0 ? (
        <Card>
          <CardContent className="py-8 text-center">
            <p className="text-muted-foreground mb-3">No projects yet</p>
            <Button onClick={() => setShowNewProject(true)}><Plus className="h-4 w-4 mr-1" /> Create Project</Button>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {projects.map((p) => (
            <Link key={p.id} to={`/projects/${p.id}`}>
              <Card className="hover:border-primary/50 transition-colors cursor-pointer">
                <CardHeader className="pb-3">
                  <div className="flex items-center gap-2">
                    <FolderGit2 className="h-5 w-5 text-primary" />
                    <CardTitle className="text-base">{p.name}</CardTitle>
                  </div>
                  <CardDescription className="truncate text-xs">{p.repoOriginUrl}</CardDescription>
                </CardHeader>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
