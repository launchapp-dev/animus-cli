import { FormEvent, Fragment, useEffect, useMemo, useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Skeleton } from "@/components/ui/skeleton";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Separator } from "@/components/ui/separator";
import { toast } from "sonner";

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

const EXECUTE_REQUIREMENTS = `mutation ExecuteRequirements($ids: [String!]!, $startWorkflows: Boolean, $workflowRef: String) { executeRequirements(ids: $ids, startWorkflows: $startWorkflows, workflowRef: $workflowRef) { requirementsProcessed tasksCreated tasksReused workflowsStarted } }`;

const EDITABLE_STATUSES = ["draft", "refined", "needs-rework"];
const PIPELINE_STAGES = ["draft", "refined", "po-review", "em-review", "approved"] as const;
const TERMINAL_STATUSES = ["approved", "implemented", "done", "deprecated"];

function StatusPipeline({ currentStatus }: { currentStatus: string }) {
  const currentIndex = PIPELINE_STAGES.indexOf(currentStatus as typeof PIPELINE_STAGES[number]);
  const isRework = currentStatus === "needs-rework";

  return (
    <div className="flex items-center gap-1">
      {PIPELINE_STAGES.map((stage, i) => {
        const isPast = i < currentIndex;
        const isCurrent = stage === currentStatus;
        return (
          <Fragment key={stage}>
            {i > 0 && <div className={`h-px w-4 ${isPast ? "bg-[var(--ao-success)]" : "bg-border/40"}`} />}
            <div className="flex items-center gap-1.5">
              <div className={`h-2.5 w-2.5 rounded-full ${
                isPast ? "bg-[var(--ao-success)]" :
                isCurrent ? "bg-primary ring-2 ring-primary/30" :
                "bg-muted-foreground/20"
              }`} />
              <span className={`text-[10px] font-mono ${
                isCurrent ? "text-primary font-medium" :
                isPast ? "text-[var(--ao-success)]" :
                "text-muted-foreground/40"
              }`}>{stage}</span>
            </div>
          </Fragment>
        );
      })}
      {isRework && (
        <div className="ml-2 flex items-center gap-1">
          <div className="h-2.5 w-2.5 rounded-full bg-[var(--ao-amber)] ring-2 ring-[var(--ao-amber)]/30" />
          <span className="text-[10px] font-mono text-[var(--ao-amber)] font-medium">needs-rework</span>
        </div>
      )}
    </div>
  );
}

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
  const [title, setTitle] = useState("");
  const [summary, setSummary] = useState("");
  const [targetAudience, setTargetAudience] = useState("");
  const [goals, setGoals] = useState<string[]>([]);
  const [successCriteria, setSuccessCriteria] = useState<string[]>([]);
  const [constraints, setConstraints] = useState<string[]>([]);
  const [newGoal, setNewGoal] = useState("");
  const [newCriterion, setNewCriterion] = useState("");
  const [newConstraint, setNewConstraint] = useState("");
  const [refineFeedback, setRefineFeedback] = useState("");
  const [saving, setSaving] = useState(false);
  const [refining, setRefining] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const vision = data?.vision;
  const visionRaw = vision?.raw ?? "";

  useEffect(() => {
    if (!vision) return;
    setTitle(vision.title ?? "");
    setSummary(vision.summary ?? "");
    setTargetAudience(vision.targetAudience ?? "");
    setGoals(vision.goals ?? []);
    setSuccessCriteria(vision.successCriteria ?? []);
    setConstraints(vision.constraints ?? []);
  }, [visionRaw]);

  const addListItem = (list: string[], setList: (v: string[]) => void, value: string, clear: () => void) => {
    const v = value.trim();
    if (!v) return;
    setList([...list, v]);
    clear();
  };

  const removeListItem = (list: string[], setList: (v: string[]) => void, index: number) => {
    setList(list.filter((_, i) => i !== index));
  };

  const buildContent = () => {
    const obj: Record<string, unknown> = {};
    if (title.trim()) obj.title = title.trim();
    if (summary.trim()) obj.summary = summary.trim();
    if (targetAudience.trim()) obj.target_audience = targetAudience.trim();
    if (goals.length > 0) obj.goals = goals;
    if (successCriteria.length > 0) obj.success_criteria = successCriteria;
    if (constraints.length > 0) obj.constraints = constraints;
    return JSON.stringify(obj, null, 2);
  };

  const onSave = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setMessage(null);
    const result = await saveVision({ content: buildContent() });
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
    const result = await refineVision({ feedback: refineFeedback || null });
    setRefining(false);
    if (result.error) {
      setMessage(`Error: ${result.error.message}`);
    } else {
      setMessage("Vision refined.");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  if (fetching) return <div className="space-y-3"><Skeleton className="h-8 w-48" /><Skeleton className="h-40 w-full" /></div>;
  if (error) return <Alert variant="destructive"><AlertDescription>{error.message}</AlertDescription></Alert>;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Planning Vision</h1>
        <p className="text-xs text-muted-foreground/60">Define the product vision and refine it iteratively.</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardContent className="pt-5 pb-4">
          <form onSubmit={onSave} className="space-y-5">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</label>
              <Input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="Project vision title..."
                className="mt-1"
              />
            </div>

            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Summary</label>
              <Textarea
                rows={3}
                value={summary}
                onChange={(e) => setSummary(e.target.value)}
                placeholder="A concise summary of the product vision..."
                className="mt-1"
              />
            </div>

            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Target Audience</label>
              <Input
                value={targetAudience}
                onChange={(e) => setTargetAudience(e.target.value)}
                placeholder="Who is this product for?"
                className="mt-1"
              />
            </div>

            <ListFieldEditor
              label="Goals"
              items={goals}
              newValue={newGoal}
              setNewValue={setNewGoal}
              placeholder="Add a goal..."
              onAdd={() => addListItem(goals, setGoals, newGoal, () => setNewGoal(""))}
              onRemove={(i) => removeListItem(goals, setGoals, i)}
            />

            <ListFieldEditor
              label="Success Criteria"
              items={successCriteria}
              newValue={newCriterion}
              setNewValue={setNewCriterion}
              placeholder="Add a success criterion..."
              onAdd={() => addListItem(successCriteria, setSuccessCriteria, newCriterion, () => setNewCriterion(""))}
              onRemove={(i) => removeListItem(successCriteria, setSuccessCriteria, i)}
            />

            <ListFieldEditor
              label="Constraints"
              items={constraints}
              newValue={newConstraint}
              setNewValue={setNewConstraint}
              placeholder="Add a constraint..."
              onAdd={() => addListItem(constraints, setConstraints, newConstraint, () => setNewConstraint(""))}
              onRemove={(i) => removeListItem(constraints, setConstraints, i)}
            />

            <Separator className="border-border/30" />

            <div className="flex items-center gap-3">
              <Button type="submit" disabled={saving}>{saving ? "Saving..." : "Save Vision"}</Button>
            </div>
          </form>
        </CardContent>
      </Card>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">AI Refinement</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3">
          <div className="flex items-center gap-3">
            <Input
              value={refineFeedback}
              onChange={(e) => setRefineFeedback(e.target.value)}
              placeholder="Optional focus area for refinement..."
              className="flex-1"
            />
            <Button variant="secondary" onClick={onRefine} disabled={refining}>
              {refining ? "Refining..." : "Refine Vision"}
            </Button>
          </div>
          <p className="text-[10px] text-muted-foreground/40 mt-2">Uses AI to expand and improve the current vision based on your feedback.</p>
        </CardContent>
      </Card>

      {message && (
        <Alert variant={message.startsWith("Error") ? "destructive" : "default"} role={message.startsWith("Error") ? "alert" : "status"} className="border-border/30">
          <AlertDescription className="text-xs">{message}</AlertDescription>
        </Alert>
      )}
    </div>
  );
}

function ListFieldEditor({
  label,
  items,
  newValue,
  setNewValue,
  placeholder,
  onAdd,
  onRemove,
}: {
  label: string;
  items: string[];
  newValue: string;
  setNewValue: (v: string) => void;
  placeholder: string;
  onAdd: () => void;
  onRemove: (i: number) => void;
}) {
  return (
    <div>
      <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">{label}</label>
      {items.length > 0 && (
        <ul className="mt-1.5 space-y-1">
          {items.map((item, i) => (
            <li key={i} className="flex items-center gap-2 group">
              <span className="text-[10px] font-mono text-muted-foreground/40 w-4 text-right shrink-0">{i + 1}</span>
              <span className="text-sm flex-1">{item}</span>
              <button
                type="button"
                onClick={() => onRemove(i)}
                className="text-[10px] text-destructive/60 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
              >
                remove
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-center gap-2 mt-1.5">
        <Input
          value={newValue}
          onChange={(e) => setNewValue(e.target.value)}
          placeholder={placeholder}
          className="text-sm"
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); onAdd(); } }}
        />
        <Button type="button" variant="outline" size="sm" onClick={onAdd}>Add</Button>
      </div>
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
          <h1 className="text-2xl font-semibold tracking-tight">Planning Requirements</h1>
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
        <h1 className="text-2xl font-semibold tracking-tight">New Requirement</h1>
        <p className="text-sm text-muted-foreground">Create a requirement entry for the active project.</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Requirement Details</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3">
          <form onSubmit={onSubmit} className="space-y-4">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</label>
              <Input required value={title} onChange={(e) => setTitle(e.target.value)} />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
              <Textarea rows={3} value={description} onChange={(e) => setDescription(e.target.value)} />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label htmlFor="create-req-priority" className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Priority</label>
                <select id="create-req-priority" value={priority} onChange={(e) => setPriority(e.target.value)} className="w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                  {PRIORITY_OPTIONS.map((p) => <option key={p} value={p}>{p}</option>)}
                </select>
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</label>
                <Input value={reqType} onChange={(e) => setReqType(e.target.value)} placeholder="e.g., functional, non-functional" />
              </div>
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Acceptance Criteria</label>
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

      {errorMsg && <Alert variant="destructive" role="alert"><AlertDescription>{errorMsg}</AlertDescription></Alert>}
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
  const [, executeRequirements] = useMutation(EXECUTE_REQUIREMENTS);

  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("should");
  const [status, setStatus] = useState("draft");
  const [reqType, setReqType] = useState("");
  const [detailCriteria, setDetailCriteria] = useState<string[]>([]);
  const [detailNewCriterion, setDetailNewCriterion] = useState("");
  const [refineFeedback, setRefineFeedback] = useState("");
  const [rejectFeedback, setRejectFeedback] = useState("");
  const [operating, setOperating] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [executionResult, setExecutionResult] = useState<{ requirementsProcessed: number; tasksCreated: number; tasksReused: number; workflowsStarted: number } | null>(null);

  const req = data?.requirement;
  const reqKey = req ? `${req.id}-${req.title}-${req.statusRaw}-${req.priorityRaw}` : "";
  const currentStatus = req?.statusRaw ?? "draft";
  const isEditable = EDITABLE_STATUSES.includes(currentStatus);

  useEffect(() => {
    if (!req) return;
    setTitle(req.title);
    setDescription(req.description);
    setPriority(req.priorityRaw);
    setStatus(req.statusRaw);
    setReqType(req.requirementType ?? "");
    setDetailCriteria(req.acceptanceCriteria ?? []);
  }, [reqKey]);

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
      toast.success("Requirement refined.");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onTransition = async (newStatus: string) => {
    setOperating("transitioning");
    setMessage(null);
    const result = await updateRequirement({ id: requirementId, status: newStatus });
    setOperating(null);
    if (result.error) {
      toast.error(result.error.message);
    } else {
      toast.success(`Status updated to ${newStatus}.`);
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onReject = async () => {
    setOperating("rejecting");
    setMessage(null);
    const result = await updateRequirement({ id: requirementId, status: "needs-rework", description: rejectFeedback ? `${req?.description ?? ""}\n\n---\nReview feedback: ${rejectFeedback}` : undefined });
    setOperating(null);
    if (result.error) {
      toast.error(result.error.message);
    } else {
      setRejectFeedback("");
      toast.success("Returned for rework.");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onRefineAndResubmit = async () => {
    setOperating("refining");
    setMessage(null);
    const refineResult = await refineRequirement({ id: requirementId, feedback: refineFeedback || null });
    if (refineResult.error) {
      setOperating(null);
      toast.error(refineResult.error.message);
      return;
    }
    const statusResult = await updateRequirement({ id: requirementId, status: "refined" });
    setOperating(null);
    if (statusResult.error) {
      toast.error(statusResult.error.message);
    } else {
      toast.success("Refined and resubmitted.");
      reexecute({ requestPolicy: "network-only" });
    }
  };

  const onExecute = async (startWorkflows: boolean) => {
    setOperating(startWorkflows ? "executing-full" : "planning");
    setMessage(null);
    setExecutionResult(null);
    const result = await executeRequirements({ ids: [requirementId], startWorkflows });
    setOperating(null);
    if (result.error) {
      toast.error(result.error.message);
    } else {
      const d = result.data?.executeRequirements;
      if (d) setExecutionResult(d);
      toast.success(startWorkflows ? "Execution started." : "Tasks planned.");
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
          <h1 className="text-2xl font-semibold tracking-tight">{req.id}</h1>
          <p className="text-sm text-muted-foreground">{req.title}</p>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={priorityColor(req.priorityRaw)}>{req.priorityRaw}</Badge>
          <Badge variant={statusColor(req.statusRaw)}>{req.statusRaw}</Badge>
        </div>
      </div>

      <StatusPipeline currentStatus={currentStatus} />

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Actions</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3">
          {currentStatus === "draft" && (
            <div className="flex items-center gap-2">
              <Input
                value={refineFeedback}
                onChange={(e) => setRefineFeedback(e.target.value)}
                placeholder="Optional refinement feedback..."
                className="flex-1 text-sm"
              />
              <Button variant="secondary" onClick={onRefine} disabled={operating !== null}>
                {operating === "refining" ? "Refining..." : "Refine with AI"}
              </Button>
            </div>
          )}
          {currentStatus === "refined" && (
            <Button onClick={() => onTransition("po-review")} disabled={operating !== null}>
              {operating === "transitioning" ? "Submitting..." : "Submit for PO Review"}
            </Button>
          )}
          {currentStatus === "needs-rework" && (
            <div className="flex items-center gap-2">
              <Input
                value={refineFeedback}
                onChange={(e) => setRefineFeedback(e.target.value)}
                placeholder="Optional refinement feedback..."
                className="flex-1 text-sm"
              />
              <Button variant="secondary" onClick={onRefineAndResubmit} disabled={operating !== null}>
                {operating === "refining" ? "Refining..." : "Refine & Resubmit"}
              </Button>
            </div>
          )}
          {currentStatus === "po-review" && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Button onClick={() => onTransition("em-review")} disabled={operating !== null}>
                  {operating === "transitioning" ? "Approving..." : "PO Approve"}
                </Button>
              </div>
              <div className="flex items-center gap-2">
                <Input
                  value={rejectFeedback}
                  onChange={(e) => setRejectFeedback(e.target.value)}
                  placeholder="Rejection feedback..."
                  className="flex-1 text-sm"
                />
                <Button variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={onReject} disabled={operating !== null}>
                  {operating === "rejecting" ? "Rejecting..." : "PO Reject"}
                </Button>
              </div>
            </div>
          )}
          {currentStatus === "em-review" && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Button onClick={() => onTransition("approved")} disabled={operating !== null}>
                  {operating === "transitioning" ? "Approving..." : "EM Approve"}
                </Button>
              </div>
              <div className="flex items-center gap-2">
                <Input
                  value={rejectFeedback}
                  onChange={(e) => setRejectFeedback(e.target.value)}
                  placeholder="Rejection feedback..."
                  className="flex-1 text-sm"
                />
                <Button variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={onReject} disabled={operating !== null}>
                  {operating === "rejecting" ? "Rejecting..." : "EM Reject"}
                </Button>
              </div>
            </div>
          )}
          {currentStatus === "approved" && (
            <div className="flex items-center gap-2">
              <Button onClick={() => onExecute(false)} disabled={operating !== null}>
                {operating === "planning" ? "Planning..." : "Plan Tasks"}
              </Button>
              <Button variant="secondary" onClick={() => onExecute(true)} disabled={operating !== null}>
                {operating === "executing-full" ? "Executing..." : "Execute Full"}
              </Button>
            </div>
          )}
          {TERMINAL_STATUSES.includes(currentStatus) && currentStatus !== "approved" && (
            <p className="text-xs text-muted-foreground/50">This requirement is in a terminal state ({currentStatus}).</p>
          )}
        </CardContent>
      </Card>

      {isEditable ? (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Edit Requirement</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            <form onSubmit={onSave} className="space-y-4">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</label>
                <Input required value={title} onChange={(e) => setTitle(e.target.value)} className="mt-1" />
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
                <Textarea rows={4} value={description} onChange={(e) => setDescription(e.target.value)} className="mt-1" />
              </div>
              <div className="grid grid-cols-3 gap-4">
                <div>
                  <label htmlFor="edit-req-priority" className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Priority</label>
                  <select id="edit-req-priority" value={priority} onChange={(e) => setPriority(e.target.value)} className="mt-1 w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                    {PRIORITY_OPTIONS.map((p) => <option key={p} value={p}>{p}</option>)}
                  </select>
                </div>
                <div>
                  <label htmlFor="edit-req-status" className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Status</label>
                  <select id="edit-req-status" value={status} onChange={(e) => setStatus(e.target.value)} className="mt-1 w-full h-9 rounded-md border border-input bg-background px-3 text-sm">
                    {STATUS_OPTIONS.map((s) => <option key={s} value={s}>{s}</option>)}
                  </select>
                </div>
                <div>
                  <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</label>
                  <Input value={reqType} onChange={(e) => setReqType(e.target.value)} className="mt-1" />
                </div>
              </div>
              <Button type="submit" disabled={operating !== null}>
                {operating === "saving" ? "Saving..." : "Save Changes"}
              </Button>
            </form>
          </CardContent>
        </Card>
      ) : (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Requirement Details</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3 space-y-3">
            <div>
              <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Title</span>
              <p className="text-sm mt-0.5">{req.title}</p>
            </div>
            {req.description && (
              <div>
                <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</span>
                <p className="text-sm mt-0.5 whitespace-pre-wrap">{req.description}</p>
              </div>
            )}
            <div className="flex gap-6">
              <div>
                <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Priority</span>
                <p className="text-sm mt-0.5">{req.priorityRaw}</p>
              </div>
              {req.requirementType && (
                <div>
                  <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</span>
                  <p className="text-sm mt-0.5">{req.requirementType}</p>
                </div>
              )}
            </div>
          </CardContent>
        </Card>
      )}

      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Acceptance Criteria</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-3">
          {detailCriteria.length > 0 ? (
            <ol className="space-y-1 mb-3">
              {detailCriteria.map((c, i) => (
                <li key={i} className="flex items-center gap-2 text-sm group">
                  <span className="text-[10px] font-mono text-muted-foreground/40 w-4 text-right shrink-0">{i + 1}</span>
                  <span className="flex-1">{c}</span>
                  {isEditable && (
                    <button type="button" onClick={() => removeDetailCriterion(i)} className="text-[10px] text-destructive/60 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">remove</button>
                  )}
                </li>
              ))}
            </ol>
          ) : (
            <p className="text-xs text-muted-foreground/50 mb-3">No acceptance criteria defined.</p>
          )}
          {isEditable && (
            <>
              <div className="flex items-center gap-2">
                <Input
                  value={detailNewCriterion}
                  onChange={(e) => setDetailNewCriterion(e.target.value)}
                  placeholder="Add acceptance criterion..."
                  className="text-sm"
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addDetailCriterion(); } }}
                />
                <Button type="button" variant="outline" size="sm" onClick={addDetailCriterion}>Add</Button>
              </div>
              <p className="text-[10px] text-muted-foreground/40 mt-2">Saved with &quot;Save Changes&quot; above.</p>
            </>
          )}
        </CardContent>
      </Card>

      {req.linkedTaskIds?.length > 0 && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Linked Tasks</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            <div className="flex flex-wrap gap-2">
              {req.linkedTaskIds.map((id: string) => (
                <Link key={id} to={`/tasks/${id}`}>
                  <Badge variant="outline" className="font-mono text-[10px] hover:bg-accent/50 transition-colors cursor-pointer">{id}</Badge>
                </Link>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {executionResult && (
        <Card className="border-border/40 bg-card/60">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Execution Results</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-3">
            <div className="grid grid-cols-4 gap-4 text-center">
              <div>
                <p className="text-lg font-semibold">{executionResult.requirementsProcessed}</p>
                <p className="text-[10px] text-muted-foreground/50">Processed</p>
              </div>
              <div>
                <p className="text-lg font-semibold">{executionResult.tasksCreated}</p>
                <p className="text-[10px] text-muted-foreground/50">Tasks Created</p>
              </div>
              <div>
                <p className="text-lg font-semibold">{executionResult.tasksReused}</p>
                <p className="text-[10px] text-muted-foreground/50">Tasks Reused</p>
              </div>
              <div>
                <p className="text-lg font-semibold">{executionResult.workflowsStarted}</p>
                <p className="text-[10px] text-muted-foreground/50">Workflows Started</p>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      <div className="flex items-center gap-3">
        <Link to="/planning/requirements"><Button variant="outline" size="sm">Back to List</Button></Link>
        {confirmDelete ? (
          <>
            <Button size="sm" variant="destructive" onClick={onDelete} disabled={operating !== null}>
              {operating === "deleting" ? "Deleting..." : "Confirm Delete"}
            </Button>
            <Button size="sm" variant="outline" onClick={() => setConfirmDelete(false)}>Cancel</Button>
          </>
        ) : (
          <Button size="sm" variant="ghost" className="text-destructive/60 hover:text-destructive" onClick={() => setConfirmDelete(true)} disabled={operating !== null}>
            Delete
          </Button>
        )}
      </div>

      {message && (
        <Alert variant={message.startsWith("Error") ? "destructive" : "default"} role={message.startsWith("Error") ? "alert" : "status"} className="border-border/30">
          <AlertDescription className="text-xs">{message}</AlertDescription>
        </Alert>
      )}
    </div>
  );
}
