import { FormEvent, useMemo, useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router-dom";
import { useQuery, useMutation } from "urql";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Skeleton } from "@/components/ui/skeleton";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Separator } from "@/components/ui/separator";

const VISION_QUERY = `
  query Vision {
    vision { title summary goals targetAudience successCriteria constraints raw }
  }
`;

const SAVE_VISION = `mutation SaveVision($content: String!) { saveVision(content: $content) { title summary goals targetAudience successCriteria constraints raw } }`;
const REFINE_VISION = `mutation RefineVision($feedback: String) { refineVision(feedback: $feedback) { title summary goals targetAudience successCriteria constraints raw } }`;

const REQUIREMENTS_QUERY = `
  query Requirements {
    requirements { id title description priority priorityRaw status statusRaw requirementType tags linkedTaskIds acceptanceCriteria }
  }
`;

const REQUIREMENT_QUERY = `
  query Requirement($id: ID!) {
    requirement(id: $id) { id title description priority priorityRaw status statusRaw requirementType tags linkedTaskIds acceptanceCriteria }
  }
`;

const CREATE_REQUIREMENT = `mutation CreateRequirement($title: String!, $description: String, $priority: String, $requirementType: String, $acceptanceCriteria: [String!]) { createRequirement(title: $title, description: $description, priority: $priority, requirementType: $requirementType, acceptanceCriteria: $acceptanceCriteria) { id } }`;
const UPDATE_REQUIREMENT = `mutation UpdateRequirement($id: ID!, $title: String, $description: String, $priority: String, $status: String, $requirementType: String, $acceptanceCriteria: [String!]) { updateRequirement(id: $id, title: $title, description: $description, priority: $priority, status: $status, requirementType: $requirementType, acceptanceCriteria: $acceptanceCriteria) { id } }`;
const DELETE_REQUIREMENT = `mutation DeleteRequirement($id: ID!) { deleteRequirement(id: $id) }`;
const DRAFT_REQUIREMENT = `mutation DraftRequirement($context: String) { draftRequirement(context: $context) { id title } }`;
const REFINE_REQUIREMENT = `mutation RefineRequirement($id: String!, $feedback: String) { refineRequirement(id: $id, feedback: $feedback) { id } }`;

const PRIORITY_OPTIONS = ["must", "should", "could", "wont"] as const;
const STATUS_OPTIONS = ["draft", "refined", "planned", "in-progress", "done", "po-review", "em-review", "needs-rework", "approved", "implemented", "deprecated"] as const;

function priorityColor(p: string) {
  switch (p) {
    case "must": return "destructive" as const;
    case "should": return "default" as const;
    case "could": return "secondary" as const;
    case "wont": return "outline" as const;
    default: return "secondary" as const;
  }
}

function statusColor(s: string) {
  switch (s) {
    case "done": case "approved": case "implemented": return "default" as const;
    case "in-progress": return "default" as const;
    case "draft": return "secondary" as const;
    case "deprecated": return "outline" as const;
    default: return "secondary" as const;
  }
}

export function PlanningEntryRedirectPage() {
  return <Navigate to="/planning/vision" replace />;
}

export function PlanningVisionPage() {
  const [{ data, fetching, error }, reexecute] = useQuery({ query: VISION_QUERY });
  const [, saveVision] = useMutation(SAVE_VISION);
  const [, refineVision] = useMutation(REFINE_VISION);
  const [content, setContent] = useState("");
  const [feedback, setFeedback] = useState("");
  const [saving, setSaving] = useState(false);
  const [refining, setRefining] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);

  const vision = data?.vision;

  if (vision && !initialized) {
    setContent(vision.raw || "");
    setInitialized(true);
  }

  const onSave = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setMessage(null);
    const result = await saveVision({ content });
    setSaving(false);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage("Vision saved.");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onRefine = async () => {
    setRefining(true);
    setMessage(null);
    const result = await refineVision({ feedback: feedback || null });
    setRefining(false);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage("Vision refined.");
      setInitialized(false);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  if (fetching) return <div className="space-y-3"><Skeleton className="h-8 w-48" /><Skeleton className="h-40 w-full" /></div>;
  if (error) return <Alert variant="destructive"><AlertDescription>{error.message}</AlertDescription></Alert>;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold">Planning Vision</h1>
        <p className="text-sm text-muted-foreground">Author product vision and refine it iteratively.</p>
      </div>

      {vision && (
        <Card>
          <CardHeader><CardTitle>Current Vision</CardTitle></CardHeader>
          <CardContent className="space-y-3">
            {vision.title && <p className="font-medium">{vision.title}</p>}
            {vision.summary && <p className="text-sm text-muted-foreground">{vision.summary}</p>}
            {vision.goals?.length > 0 && (
              <div>
                <p className="text-sm font-medium mb-1">Goals</p>
                <ul className="list-disc list-inside text-sm space-y-0.5">
                  {vision.goals.map((g: string, i: number) => <li key={i}>{g}</li>)}
                </ul>
              </div>
            )}
            {vision.constraints?.length > 0 && (
              <div>
                <p className="text-sm font-medium mb-1">Constraints</p>
                <ul className="list-disc list-inside text-sm space-y-0.5">
                  {vision.constraints.map((c: string, i: number) => <li key={i}>{c}</li>)}
                </ul>
              </div>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader><CardTitle>{vision ? "Edit Vision" : "Create Vision"}</CardTitle></CardHeader>
        <CardContent>
          <form onSubmit={onSave} className="space-y-4">
            <Textarea
              rows={12}
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder="Enter vision content (markdown supported)..."
              className="font-mono text-sm"
            />
            <div className="flex items-center gap-3">
              <Button type="submit" disabled={saving}>{saving ? "Saving..." : "Save Vision"}</Button>
              <Separator orientation="vertical" className="h-6" />
              <Input
                value={feedback}
                onChange={(e) => setFeedback(e.target.value)}
                placeholder="Optional refinement focus..."
                className="max-w-xs"
              />
              <Button type="button" variant="secondary" onClick={onRefine} disabled={refining}>
                {refining ? "Refining..." : "Refine Vision"}
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {message && (
        <Alert variant={message.startsWith("Error") ? "destructive" : "default"}>
          <AlertDescription>{message}</AlertDescription>
        </Alert>
      )}
    </div>
  );
}

export function PlanningRequirementsPage() {
  const [{ data, fetching, error }, reexecute] = useQuery({ query: REQUIREMENTS_QUERY });
  const [, draftRequirement] = useMutation(DRAFT_REQUIREMENT);
  const [, refineRequirement] = useMutation(REFINE_REQUIREMENT);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [refineFocus, setRefineFocus] = useState("");
  const [operating, setOperating] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const requirements = useMemo(() => {
    const list = data?.requirements ?? [];
    return [...list].sort((a: { id: string }, b: { id: string }) => a.id.localeCompare(b.id));
  }, [data]);

  const toggleSelection = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const onDraft = async () => {
    setOperating("drafting");
    setMessage(null);
    const result = await draftRequirement({ context: null });
    setOperating(null);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage(`Drafted requirement ${result.data?.draftRequirement?.id ?? ""}.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onRefineSelected = async () => {
    if (selectedIds.size === 0) return;
    setOperating("refining");
    setMessage(null);
    let refined = 0;
    for (const id of selectedIds) {
      const result = await refineRequirement({ id, feedback: refineFocus || null });
      if (!result.error) refined++;
    }
    setOperating(null);
    setMessage(`Refined ${refined} requirement(s).`);
    reexecute({ requestPolicy: "network-only" });
  };

  if (fetching) return <div className="space-y-3"><Skeleton className="h-8 w-48" /><Skeleton className="h-20 w-full" /><Skeleton className="h-20 w-full" /></div>;
  if (error) return <Alert variant="destructive"><AlertDescription>{error.message}</AlertDescription></Alert>;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">Planning Requirements</h1>
          <p className="text-sm text-muted-foreground">Browse, draft, and refine requirements.</p>
        </div>
        <div className="flex items-center gap-2">
          <Link to="/planning/requirements/new">
            <Button>New Requirement</Button>
          </Link>
          <Button variant="secondary" onClick={onDraft} disabled={operating !== null}>
            {operating === "drafting" ? "Drafting..." : "Draft Suggestion"}
          </Button>
        </div>
      </div>

      <div className="flex items-center gap-3">
        <Input
          value={refineFocus}
          onChange={(e) => setRefineFocus(e.target.value)}
          placeholder="Refine focus (optional)..."
          className="max-w-xs"
        />
        <Button
          variant="secondary"
          onClick={onRefineSelected}
          disabled={selectedIds.size === 0 || operating !== null}
        >
          {operating === "refining" ? "Refining..." : `Refine Selected (${selectedIds.size})`}
        </Button>
      </div>

      {message && (
        <Alert variant={message.startsWith("Error") ? "destructive" : "default"}>
          <AlertDescription>{message}</AlertDescription>
        </Alert>
      )}

      {requirements.length === 0 ? (
        <Card>
          <CardContent className="py-8 text-center text-muted-foreground">
            No requirements yet. Create one or run draft suggestions.
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-2">
          {requirements.map((req: { id: string; title: string; description: string; priorityRaw: string; statusRaw: string }) => (
            <Card key={req.id} className="hover:bg-accent/50 transition-colors">
              <CardContent className="flex items-center gap-3 py-3">
                <input
                  type="checkbox"
                  checked={selectedIds.has(req.id)}
                  onChange={() => toggleSelection(req.id)}
                  className="h-4 w-4 shrink-0"
                  aria-label={`Select ${req.id}`}
                />
                <div className="flex-1 min-w-0">
                  <Link to={`/planning/requirements/${encodeURIComponent(req.id)}`} className="font-medium hover:underline">
                    {req.id} · {req.title}
                  </Link>
                  {req.description && <p className="text-sm text-muted-foreground truncate">{req.description}</p>}
                </div>
                <Badge variant={priorityColor(req.priorityRaw)}>{req.priorityRaw}</Badge>
                <Badge variant={statusColor(req.statusRaw)}>{req.statusRaw}</Badge>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}

export function PlanningRequirementCreatePage() {
  const navigate = useNavigate();
  const [, createRequirement] = useMutation(CREATE_REQUIREMENT);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("should");
  const [reqType, setReqType] = useState("");
  const [criteria, setCriteria] = useState<string[]>([]);
  const [newCriterion, setNewCriterion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const addCriterion = () => {
    const val = newCriterion.trim();
    if (!val) return;
    setCriteria((prev) => [...prev, val]);
    setNewCriterion("");
  };

  const removeCriterion = (index: number) => {
    setCriteria((prev) => prev.filter((_, i) => i !== index));
  };

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim()) { setErrorMsg("Title is required."); return; }
    setSubmitting(true);
    setErrorMsg(null);
    const result = await createRequirement({
      title: title.trim(),
      description: description.trim() || null,
      priority,
      requirementType: reqType || null,
      acceptanceCriteria: criteria.length > 0 ? criteria : null,
    });
    setSubmitting(false);
    if (result.error) {
      setErrorMsg(result.error.message);
    } else {
      navigate(`/planning/requirements/${encodeURIComponent(result.data.createRequirement.id)}`, { replace: true });
    }
  };

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold">New Requirement</h1>
        <p className="text-sm text-muted-foreground">Create a requirement entry for the active project.</p>
      </div>

      <Card>
        <CardContent className="pt-6">
          <form onSubmit={onSubmit} className="space-y-4">
            <div>
              <label className="text-sm font-medium">Title</label>
              <Input required value={title} onChange={(e) => setTitle(e.target.value)} />
            </div>
            <div>
              <label className="text-sm font-medium">Description</label>
              <Textarea rows={3} value={description} onChange={(e) => setDescription(e.target.value)} />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="text-sm font-medium">Priority</label>
                <select value={priority} onChange={(e) => setPriority(e.target.value)} className="w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                  {PRIORITY_OPTIONS.map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-sm font-medium">Type</label>
                <Input value={reqType} onChange={(e) => setReqType(e.target.value)} placeholder="e.g., functional, non-functional" />
              </div>
            </div>
            <div>
              <label className="text-sm font-medium">Acceptance Criteria</label>
              {criteria.length > 0 && (
                <ul className="mt-1 space-y-1">
                  {criteria.map((c, i) => (
                    <li key={i} className="flex items-center gap-2 text-sm">
                      <span className="flex-1">{i + 1}. {c}</span>
                      <Button type="button" variant="ghost" size="sm" onClick={() => removeCriterion(i)}>Remove</Button>
                    </li>
                  ))}
                </ul>
              )}
              <div className="flex items-center gap-2 mt-2">
                <Input
                  value={newCriterion}
                  onChange={(e) => setNewCriterion(e.target.value)}
                  placeholder="Add acceptance criterion..."
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addCriterion(); } }}
                />
                <Button type="button" variant="secondary" onClick={addCriterion}>Add</Button>
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Button type="submit" disabled={submitting}>{submitting ? "Creating..." : "Create Requirement"}</Button>
              <Link to="/planning/requirements"><Button variant="outline">Cancel</Button></Link>
            </div>
          </form>
        </CardContent>
      </Card>

      {errorMsg && <Alert variant="destructive"><AlertDescription>{errorMsg}</AlertDescription></Alert>}
    </div>
  );
}

export function PlanningRequirementDetailPage() {
  const navigate = useNavigate();
  const params = useParams();
  const requirementId = params.requirementId ?? "";

  const [{ data, fetching, error }, reexecute] = useQuery({ query: REQUIREMENT_QUERY, variables: { id: requirementId } });
  const [, updateRequirement] = useMutation(UPDATE_REQUIREMENT);
  const [, deleteRequirement] = useMutation(DELETE_REQUIREMENT);
  const [, refineRequirement] = useMutation(REFINE_REQUIREMENT);

  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("should");
  const [status, setStatus] = useState("draft");
  const [reqType, setReqType] = useState("");
  const [detailCriteria, setDetailCriteria] = useState<string[]>([]);
  const [detailNewCriterion, setDetailNewCriterion] = useState("");
  const [refineFeedback, setRefineFeedback] = useState("");
  const [operating, setOperating] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [initialized, setInitialized] = useState(false);

  const req = data?.requirement;

  if (req && !initialized) {
    setTitle(req.title);
    setDescription(req.description);
    setPriority(req.priorityRaw);
    setStatus(req.statusRaw);
    setReqType(req.requirementType ?? "");
    setDetailCriteria(req.acceptanceCriteria ?? []);
    setInitialized(true);
  }

  const addDetailCriterion = () => {
    const val = detailNewCriterion.trim();
    if (!val) return;
    setDetailCriteria((prev) => [...prev, val]);
    setDetailNewCriterion("");
  };

  const removeDetailCriterion = (index: number) => {
    setDetailCriteria((prev) => prev.filter((_, i) => i !== index));
  };

  const onSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim()) { setMessage("Error: Title is required."); return; }
    setOperating("saving");
    setMessage(null);
    const result = await updateRequirement({
      id: requirementId,
      title: title.trim(),
      description: description.trim(),
      priority,
      status,
      requirementType: reqType || null,
      acceptanceCriteria: detailCriteria,
    });
    setOperating(null);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage("Requirement updated.");
      setInitialized(false);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onDelete = async () => {
    setOperating("deleting");
    const result = await deleteRequirement({ id: requirementId });
    setOperating(null);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      navigate("/planning/requirements", { replace: true });
    }
  };

  const onRefine = async () => {
    setOperating("refining");
    setMessage(null);
    const result = await refineRequirement({ id: requirementId, feedback: refineFeedback || null });
    setOperating(null);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage("Requirement refined.");
      setInitialized(false);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  if (fetching) return <div className="space-y-3"><Skeleton className="h-8 w-48" /><Skeleton className="h-40 w-full" /></div>;
  if (error) return <Alert variant="destructive"><AlertDescription>{error.message}</AlertDescription></Alert>;
  if (!req) return (
    <div className="space-y-4">
      <Alert><AlertDescription>Requirement {requirementId} not found.</AlertDescription></Alert>
      <Link to="/planning/requirements"><Button variant="outline">Back to Requirements</Button></Link>
    </div>
  );

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{req.id}</h1>
          <p className="text-sm text-muted-foreground">{req.title}</p>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={priorityColor(req.priorityRaw)}>{req.priorityRaw}</Badge>
          <Badge variant={statusColor(req.statusRaw)}>{req.statusRaw}</Badge>
        </div>
      </div>

      <Card>
        <CardHeader><CardTitle>Edit Requirement</CardTitle></CardHeader>
        <CardContent>
          <form onSubmit={onSave} className="space-y-4">
            <div>
              <label className="text-sm font-medium">Title</label>
              <Input required value={title} onChange={(e) => setTitle(e.target.value)} />
            </div>
            <div>
              <label className="text-sm font-medium">Description</label>
              <Textarea rows={4} value={description} onChange={(e) => setDescription(e.target.value)} />
            </div>
            <div className="grid grid-cols-3 gap-4">
              <div>
                <label className="text-sm font-medium">Priority</label>
                <select value={priority} onChange={(e) => setPriority(e.target.value)} className="w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                  {PRIORITY_OPTIONS.map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-sm font-medium">Status</label>
                <select value={status} onChange={(e) => setStatus(e.target.value)} className="w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                  {STATUS_OPTIONS.map((s) => <option key={s} value={s}>{s}</option>)}
                </select>
              </div>
              <div>
                <label className="text-sm font-medium">Type</label>
                <Input value={reqType} onChange={(e) => setReqType(e.target.value)} />
              </div>
            </div>
            <Button type="submit" disabled={operating !== null}>
              {operating === "saving" ? "Saving..." : "Save Changes"}
            </Button>
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>Acceptance Criteria</CardTitle></CardHeader>
        <CardContent>
          {detailCriteria.length > 0 ? (
            <ol className="space-y-1 mb-3">
              {detailCriteria.map((c, i) => (
                <li key={i} className="flex items-center gap-2 text-sm">
                  <span className="flex-1">{i + 1}. {c}</span>
                  <Button type="button" variant="ghost" size="sm" onClick={() => removeDetailCriterion(i)}>Remove</Button>
                </li>
              ))}
            </ol>
          ) : (
            <p className="text-sm text-muted-foreground mb-3">No acceptance criteria defined.</p>
          )}
          <div className="flex items-center gap-2">
            <Input
              value={detailNewCriterion}
              onChange={(e) => setDetailNewCriterion(e.target.value)}
              placeholder="Add acceptance criterion..."
              onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addDetailCriterion(); } }}
            />
            <Button type="button" variant="secondary" onClick={addDetailCriterion}>Add</Button>
          </div>
          <p className="text-xs text-muted-foreground mt-2">Changes are saved when you click &quot;Save Changes&quot; above.</p>
        </CardContent>
      </Card>

      {req.linkedTaskIds?.length > 0 && (
        <Card>
          <CardHeader><CardTitle>Linked Tasks</CardTitle></CardHeader>
          <CardContent>
            <div className="flex flex-wrap gap-2">
              {req.linkedTaskIds.map((id: string) => (
                <Link key={id} to={`/tasks/${id}`}>
                  <Badge variant="outline" className="hover:bg-accent cursor-pointer">{id}</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader><CardTitle>Refine</CardTitle></CardHeader>
        <CardContent className="flex items-center gap-3">
          <Input
            value={refineFeedback}
            onChange={(e) => setRefineFeedback(e.target.value)}
            placeholder="Optional refinement feedback..."
            className="max-w-sm"
          />
          <Button variant="secondary" onClick={onRefine} disabled={operating !== null}>
            {operating === "refining" ? "Refining..." : "Refine Requirement"}
          </Button>
        </CardContent>
      </Card>

      <div className="flex items-center gap-3">
        <Link to="/planning/requirements"><Button variant="outline">Back to List</Button></Link>
        {confirmDelete ? (
          <>
            <Button variant="destructive" onClick={onDelete} disabled={operating !== null}>
              {operating === "deleting" ? "Deleting..." : "Confirm Delete"}
            </Button>
            <Button variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
          </>
        ) : (
          <Button variant="destructive" onClick={() => setConfirmDelete(true)} disabled={operating !== null}>
            Delete Requirement
          </Button>
        )}
      </div>

      {message && (
        <Alert variant={message.startsWith("Error") ? "destructive" : "default"}>
          <AlertDescription>{message}</AlertDescription>
        </Alert>
      )}
    </div>
  );
}
