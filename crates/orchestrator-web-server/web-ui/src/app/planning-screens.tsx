import { FormEvent, ReactNode, useEffect, useMemo, useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router-dom";

import { api } from "../lib/api/client";
import type { ApiError } from "../lib/api/envelope";
import type {
  PlanningRequirementCreateInput,
  PlanningRequirementItem,
  PlanningRequirementUpdateInput,
  PlanningRequirementsRefineResult,
  PlanningVisionDocument,
} from "../lib/api/contracts/models";
import { useApiResource } from "../lib/api/use-api-resource";

type RequirementFormValues = {
  title: string;
  description: string;
  body: string;
  acceptanceCriteria: string;
  priority: "must" | "should" | "could" | "wont";
  status:
    | "draft"
    | "refined"
    | "planned"
    | "in-progress"
    | "done"
    | "po-review"
    | "em-review"
    | "needs-rework"
    | "approved"
    | "implemented"
    | "deprecated";
  source: string;
};

type VisionFormValues = {
  projectName: string;
  problemStatement: string;
  targetUsers: string;
  goals: string;
  constraints: string;
  valueProposition: string;
};

const REQUIREMENT_PRIORITY_OPTIONS = [
  { value: "must", label: "Must" },
  { value: "should", label: "Should" },
  { value: "could", label: "Could" },
  { value: "wont", label: "Won't" },
] as const;

const REQUIREMENT_STATUS_OPTIONS = [
  { value: "draft", label: "Draft" },
  { value: "refined", label: "Refined" },
  { value: "planned", label: "Planned" },
  { value: "in-progress", label: "In Progress" },
  { value: "done", label: "Done" },
  { value: "po-review", label: "PO Review" },
  { value: "em-review", label: "EM Review" },
  { value: "needs-rework", label: "Needs Rework" },
  { value: "approved", label: "Approved" },
  { value: "implemented", label: "Implemented" },
  { value: "deprecated", label: "Deprecated" },
] as const;

export function PlanningEntryRedirectPage() {
  return <Navigate to="/planning/vision" replace />;
}

export function PlanningVisionPage() {
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [formValues, setFormValues] = useState<VisionFormValues>(defaultVisionFormValues());
  const [validationErrors, setValidationErrors] = useState<Record<string, string>>({});
  const [saveError, setSaveError] = useState<ApiError | null>(null);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [refineError, setRefineError] = useState<ApiError | null>(null);
  const [refineMessage, setRefineMessage] = useState<string | null>(null);
  const [refineFocus, setRefineFocus] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const [isRefining, setIsRefining] = useState(false);

  const visionState = useApiResource(
    async () => api.visionGet(),
    [refreshNonce],
    {
      isEmpty: (data) => data === null,
    },
  );

  useEffect(() => {
    if (visionState.status === "ready") {
      setFormValues(visionToFormValues(visionState.data));
      setValidationErrors({});
      setSaveError(null);
      setRefineError(null);
      return;
    }

    if (visionState.status === "empty") {
      setFormValues(defaultVisionFormValues());
      setValidationErrors({});
      setSaveError(null);
      setRefineError(null);
    }
  }, [
    visionState.status,
    visionState.status === "ready" ? visionState.data.updated_at : "",
  ]);

  const onSave = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setSaveMessage(null);
    setSaveError(null);

    const errors = validateVisionForm(formValues);
    setValidationErrors(errors);
    if (Object.keys(errors).length > 0) {
      return;
    }

    const payload = visionFormToPayload(formValues);
    setIsSaving(true);
    void api.visionSave(payload).then((result) => {
      setIsSaving(false);

      if (result.kind === "error") {
        setSaveError(result);
        return;
      }

      setFormValues(visionToFormValues(result.data));
      setSaveMessage("Vision saved.");
      setRefreshNonce((current) => current + 1);
    });
  };

  const onRefine = () => {
    setRefineError(null);
    setRefineMessage(null);
    setIsRefining(true);

    const focus = normalizeOptionalText(refineFocus);
    void api.visionRefine({ focus }).then((result) => {
      setIsRefining(false);

      if (result.kind === "error") {
        setRefineError(result);
        return;
      }

      setFormValues(visionToFormValues(result.data.updated_vision));
      setRefineMessage(
        result.data.refinement.rationale ?? "Vision refined heuristically.",
      );
      setRefreshNonce((current) => current + 1);
    });
  };

  if (visionState.status === "loading") {
    return (
      <PlanningRouteSection
        title="Planning Vision"
        description="Author product vision and refine it iteratively."
      >
        <LoadingState message="Loading vision..." />
      </PlanningRouteSection>
    );
  }

  if (visionState.status === "error") {
    return (
      <PlanningRouteSection
        title="Planning Vision"
        description="Author product vision and refine it iteratively."
      >
        <ErrorState error={visionState.error} />
      </PlanningRouteSection>
    );
  }

  return (
    <PlanningRouteSection
      title="Planning Vision"
      description="Author product vision and refine it iteratively."
    >
      {visionState.status === "empty" ? (
        <EmptyState message="No vision exists yet. Fill in the form to create your first draft." />
      ) : null}

      <form className="planning-form" onSubmit={onSave}>
        <div className="planning-form-grid">
          <label>
            Project Name
            <input
              value={formValues.projectName}
              onChange={(event) =>
                setFormValues((current) => ({
                  ...current,
                  projectName: event.target.value,
                }))
              }
            />
            {validationErrors.projectName ? (
              <span role="alert" className="field-error">
                {validationErrors.projectName}
              </span>
            ) : null}
          </label>

          <label>
            Value Proposition
            <input
              value={formValues.valueProposition}
              onChange={(event) =>
                setFormValues((current) => ({
                  ...current,
                  valueProposition: event.target.value,
                }))
              }
            />
            {validationErrors.valueProposition ? (
              <span role="alert" className="field-error">
                {validationErrors.valueProposition}
              </span>
            ) : null}
          </label>
        </div>

        <label>
          Problem Statement
          <textarea
            rows={3}
            value={formValues.problemStatement}
            onChange={(event) =>
              setFormValues((current) => ({
                ...current,
                problemStatement: event.target.value,
              }))
            }
          />
          {validationErrors.problemStatement ? (
            <span role="alert" className="field-error">
              {validationErrors.problemStatement}
            </span>
          ) : null}
        </label>

        <div className="planning-form-grid">
          <label>
            Target Users (one per line)
            <textarea
              rows={5}
              value={formValues.targetUsers}
              onChange={(event) =>
                setFormValues((current) => ({
                  ...current,
                  targetUsers: event.target.value,
                }))
              }
            />
            {validationErrors.targetUsers ? (
              <span role="alert" className="field-error">
                {validationErrors.targetUsers}
              </span>
            ) : null}
          </label>

          <label>
            Goals (one per line)
            <textarea
              rows={5}
              value={formValues.goals}
              onChange={(event) =>
                setFormValues((current) => ({
                  ...current,
                  goals: event.target.value,
                }))
              }
            />
            {validationErrors.goals ? (
              <span role="alert" className="field-error">
                {validationErrors.goals}
              </span>
            ) : null}
          </label>

          <label>
            Constraints (one per line)
            <textarea
              rows={5}
              value={formValues.constraints}
              onChange={(event) =>
                setFormValues((current) => ({
                  ...current,
                  constraints: event.target.value,
                }))
              }
            />
            {validationErrors.constraints ? (
              <span role="alert" className="field-error">
                {validationErrors.constraints}
              </span>
            ) : null}
          </label>
        </div>

        <div className="planning-inline">
          <button type="submit" disabled={isSaving}>
            {isSaving ? "Saving..." : "Save Vision"}
          </button>
          <label className="compact-field">
            <span>Refine Focus</span>
            <input
              value={refineFocus}
              onChange={(event) => setRefineFocus(event.target.value)}
              placeholder="Optional focus area"
            />
          </label>
          <button type="button" onClick={onRefine} disabled={isRefining}>
            {isRefining ? "Refining..." : "Refine Vision"}
          </button>
        </div>
      </form>

      {saveMessage ? <StatusState message={saveMessage} /> : null}
      {saveError ? <ErrorState error={saveError} /> : null}
      {refineMessage ? <StatusState message={refineMessage} /> : null}
      {refineError ? <ErrorState error={refineError} /> : null}
    </PlanningRouteSection>
  );
}

export function PlanningRequirementsPage() {
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [refineFocus, setRefineFocus] = useState("");
  const [isRefining, setIsRefining] = useState(false);
  const [isDrafting, setIsDrafting] = useState(false);
  const [refineError, setRefineError] = useState<ApiError | null>(null);
  const [refineResult, setRefineResult] =
    useState<PlanningRequirementsRefineResult | null>(null);
  const [draftError, setDraftError] = useState<ApiError | null>(null);
  const [draftMessage, setDraftMessage] = useState<string | null>(null);
  const [confirmRefineAll, setConfirmRefineAll] = useState(false);

  const requirementsState = useApiResource(
    async () => api.requirementsList(),
    [refreshNonce],
    {
      isEmpty: (data) => data.length === 0,
    },
  );

  const sortedRequirements = useMemo(() => {
    if (requirementsState.status !== "ready") {
      return [] as PlanningRequirementItem[];
    }

    return [...requirementsState.data].sort((left, right) =>
      left.id.localeCompare(right.id),
    );
  }, [requirementsState]);

  useEffect(() => {
    if (requirementsState.status !== "ready") {
      if (requirementsState.status === "empty") {
        setSelectedIds([]);
      }
      return;
    }

    const available = new Set(requirementsState.data.map((requirement) => requirement.id));
    setSelectedIds((current) => current.filter((id) => available.has(id)));
  }, [requirementsState]);

  const toggleSelection = (requirementId: string, checked: boolean) => {
    setSelectedIds((current) => {
      if (checked) {
        if (current.includes(requirementId)) {
          return current;
        }
        return [...current, requirementId];
      }

      return current.filter((id) => id !== requirementId);
    });
  };

  const runRefine = (requirementIds: string[]) => {
    setRefineError(null);
    setRefineResult(null);
    setIsRefining(true);
    setConfirmRefineAll(false);

    void api
      .requirementsRefine({
        requirement_ids: requirementIds,
        focus: normalizeOptionalText(refineFocus),
      })
      .then((result) => {
        setIsRefining(false);
        if (result.kind === "error") {
          setRefineError(result);
          return;
        }

        setRefineResult(result.data);
        setRefreshNonce((current) => current + 1);
      });
  };

  const runDraft = () => {
    setDraftError(null);
    setDraftMessage(null);
    setIsDrafting(true);

    void api
      .requirementsDraft({
        append_only: true,
      })
      .then((result) => {
        setIsDrafting(false);
        if (result.kind === "error") {
          setDraftError(result);
          return;
        }

        setDraftMessage(
          `Drafted ${result.data.appended_count} requirement(s).`,
        );
        setRefreshNonce((current) => current + 1);
      });
  };

  return (
    <PlanningRouteSection
      title="Planning Requirements"
      description="Browse, draft, and refine requirements with stable deep links."
    >
      <div className="planning-inline">
        <Link to="/planning/requirements/new" className="action-link">
          New Requirement
        </Link>
        <button type="button" onClick={runDraft} disabled={isDrafting || isRefining}>
          {isDrafting ? "Drafting..." : "Draft Suggestions"}
        </button>
        <label className="compact-field">
          <span>Refine Focus</span>
          <input
            value={refineFocus}
            onChange={(event) => setRefineFocus(event.target.value)}
            placeholder="Optional focus area"
          />
        </label>
        <button
          type="button"
          onClick={() => runRefine(selectedIds)}
          disabled={selectedIds.length === 0 || isRefining || isDrafting}
        >
          {isRefining ? "Refining..." : "Refine Selected"}
        </button>
        <button
          type="button"
          onClick={() => setConfirmRefineAll(true)}
          disabled={isRefining || isDrafting}
        >
          Refine All
        </button>
      </div>

      {confirmRefineAll ? (
        <div className="confirmation-box" role="alertdialog" aria-modal="false">
          <strong>Refine all requirements?</strong>
          <p>
            This will run refinement across the full list. Continue only if the
            current vision baseline is ready.
          </p>
          <div className="planning-inline">
            <button type="button" onClick={() => runRefine([])} disabled={isRefining}>
              Confirm Refine All
            </button>
            <button
              type="button"
              onClick={() => setConfirmRefineAll(false)}
              disabled={isRefining}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      {draftMessage ? <StatusState message={draftMessage} /> : null}
      {draftError ? <ErrorState error={draftError} /> : null}
      {refineError ? <ErrorState error={refineError} /> : null}
      {refineResult ? (
        <StatusState
          message={`Refined ${refineResult.updated_ids.length} requirement(s) in ${refineResult.scope} scope.`}
        />
      ) : null}

      {requirementsState.status === "loading" ? (
        <LoadingState message="Loading requirements..." />
      ) : null}
      {requirementsState.status === "error" ? (
        <ErrorState error={requirementsState.error} />
      ) : null}
      {requirementsState.status === "empty" ? (
        <EmptyState message="No requirements yet. Create one or run draft suggestions." />
      ) : null}

      {requirementsState.status === "ready" ? (
        <ul className="planning-requirement-list">
          {sortedRequirements.map((requirement) => (
            <li key={requirement.id} className="planning-requirement-row">
              <label className="row-select">
                <span className="visually-hidden">Select {requirement.id}</span>
                <input
                  type="checkbox"
                  checked={selectedIds.includes(requirement.id)}
                  onChange={(event) =>
                    toggleSelection(requirement.id, event.target.checked)
                  }
                />
              </label>

              <div className="row-content">
                <p className="row-title">
                  <Link to={planningRequirementPath(requirement.id)}>
                    {requirement.id} · {requirement.title}
                  </Link>
                </p>
                <p className="row-description">{requirement.description}</p>
                <p className="row-meta">
                  status <code>{requirement.status}</code> · priority{" "}
                  <code>{requirement.priority}</code>
                </p>
              </div>

              <Link to={planningRequirementPath(requirement.id)} className="action-link">
                Open
              </Link>
            </li>
          ))}
        </ul>
      ) : null}
    </PlanningRouteSection>
  );
}

export function PlanningRequirementCreatePage() {
  const navigate = useNavigate();
  const [formValues, setFormValues] = useState<RequirementFormValues>(
    defaultRequirementFormValues(),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<ApiError | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setValidationError(null);
    setSubmitError(null);

    const payload = requirementCreatePayloadFromForm(formValues);
    if (!payload.title || payload.title.trim().length === 0) {
      setValidationError("Title is required.");
      return;
    }

    setIsSubmitting(true);
    void api.requirementsCreate(payload).then((result) => {
      setIsSubmitting(false);

      if (result.kind === "error") {
        setSubmitError(result);
        return;
      }

      navigate(planningRequirementPath(result.data.id), { replace: true });
    });
  };

  return (
    <PlanningRouteSection
      title="New Requirement"
      description="Create a requirement entry for the active project context."
    >
      <RequirementEditorForm
        formValues={formValues}
        onChange={setFormValues}
        onSubmit={onSubmit}
        submitLabel={isSubmitting ? "Creating..." : "Create Requirement"}
        submitDisabled={isSubmitting}
      />

      <div className="planning-inline">
        <Link to="/planning/requirements" className="action-link">
          Back to Requirements
        </Link>
      </div>

      {validationError ? (
        <div className="error-box" role="alert">
          {validationError}
        </div>
      ) : null}
      {submitError ? <ErrorState error={submitError} /> : null}
    </PlanningRouteSection>
  );
}

export function PlanningRequirementDetailPage() {
  const navigate = useNavigate();
  const params = useParams();
  const requirementId = params.requirementId ?? "";

  const [refreshNonce, setRefreshNonce] = useState(0);
  const [formValues, setFormValues] = useState<RequirementFormValues>(
    defaultRequirementFormValues(),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<ApiError | null>(null);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [refineError, setRefineError] = useState<ApiError | null>(null);
  const [refineMessage, setRefineMessage] = useState<string | null>(null);
  const [refineFocus, setRefineFocus] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [isRefining, setIsRefining] = useState(false);

  const requirementState = useApiResource(
    async () => api.requirementsById(requirementId),
    [requirementId, refreshNonce],
  );

  useEffect(() => {
    if (requirementState.status !== "ready") {
      return;
    }

    setFormValues(requirementToFormValues(requirementState.data));
    setValidationError(null);
    setSaveError(null);
    setRefineError(null);
    setDeleteError(null);
  }, [
    requirementState.status,
    requirementState.status === "ready" ? requirementState.data.updated_at : "",
  ]);

  const onSave = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setValidationError(null);
    setSaveError(null);
    setSaveMessage(null);

    const payload = requirementUpdatePayloadFromForm(formValues);
    if (!payload.title || payload.title.trim().length === 0) {
      setValidationError("Title is required.");
      return;
    }

    setIsSaving(true);
    void api.requirementsUpdate(requirementId, payload).then((result) => {
      setIsSaving(false);

      if (result.kind === "error") {
        setSaveError(result);
        return;
      }

      setFormValues(requirementToFormValues(result.data));
      setSaveMessage("Requirement updated.");
      setRefreshNonce((current) => current + 1);
    });
  };

  const onDelete = () => {
    setDeleteError(null);
    setIsDeleting(true);

    void api.requirementsDelete(requirementId).then((result) => {
      setIsDeleting(false);

      if (result.kind === "error") {
        setDeleteError(result);
        return;
      }

      navigate("/planning/requirements", { replace: true });
    });
  };

  const onRefineSingle = () => {
    setRefineError(null);
    setRefineMessage(null);
    setIsRefining(true);

    void api
      .requirementsRefine({
        requirement_ids: [requirementId],
        focus: normalizeOptionalText(refineFocus),
      })
      .then((result) => {
        setIsRefining(false);

        if (result.kind === "error") {
          setRefineError(result);
          return;
        }

        setRefineMessage(
          `Refined ${result.data.updated_ids.length} requirement(s).`,
        );
        setRefreshNonce((current) => current + 1);
      });
  };

  if (requirementState.status === "loading") {
    return (
      <PlanningRouteSection
        title="Requirement Detail"
        description={`Requirement ${requirementId}`}
      >
        <LoadingState message="Loading requirement..." />
      </PlanningRouteSection>
    );
  }

  if (requirementState.status === "error" && requirementState.error.code === "not_found") {
    return (
      <PlanningRouteSection
        title="Requirement Detail"
        description={`Requirement ${requirementId}`}
      >
        <EmptyState message="Requirement not found. It may have been deleted or moved." />
        <p>
          <Link to="/planning/requirements" className="action-link">
            Back to Requirements List
          </Link>
        </p>
      </PlanningRouteSection>
    );
  }

  if (requirementState.status === "error") {
    return (
      <PlanningRouteSection
        title="Requirement Detail"
        description={`Requirement ${requirementId}`}
      >
        <ErrorState error={requirementState.error} />
      </PlanningRouteSection>
    );
  }

  return (
    <PlanningRouteSection
      title="Requirement Detail"
      description={`Requirement ${requirementId}`}
    >
      <RequirementEditorForm
        formValues={formValues}
        onChange={setFormValues}
        onSubmit={onSave}
        submitLabel={isSaving ? "Saving..." : "Save Requirement"}
        submitDisabled={isSaving || isDeleting}
      />

      <div className="planning-inline">
        <label className="compact-field">
          <span>Refine Focus</span>
          <input
            value={refineFocus}
            onChange={(event) => setRefineFocus(event.target.value)}
            placeholder="Optional focus area"
          />
        </label>
        <button type="button" onClick={onRefineSingle} disabled={isRefining || isDeleting}>
          {isRefining ? "Refining..." : "Refine Requirement"}
        </button>
        <Link to="/planning/requirements" className="action-link">
          Back to List
        </Link>
      </div>

      {confirmDelete ? (
        <div className="danger-box" role="alertdialog" aria-modal="false">
          <strong>Delete requirement {requirementId}?</strong>
          <p>This operation cannot be undone.</p>
          <div className="planning-inline">
            <button type="button" onClick={onDelete} disabled={isDeleting}>
              {isDeleting ? "Deleting..." : "Confirm Delete"}
            </button>
            <button
              type="button"
              onClick={() => setConfirmDelete(false)}
              disabled={isDeleting}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <div className="planning-inline">
          <button
            type="button"
            onClick={() => setConfirmDelete(true)}
            className="danger-button"
            disabled={isSaving || isDeleting}
          >
            Delete Requirement
          </button>
        </div>
      )}

      {validationError ? (
        <div className="error-box" role="alert">
          {validationError}
        </div>
      ) : null}
      {saveMessage ? <StatusState message={saveMessage} /> : null}
      {saveError ? <ErrorState error={saveError} /> : null}
      {refineMessage ? <StatusState message={refineMessage} /> : null}
      {refineError ? <ErrorState error={refineError} /> : null}
      {deleteError ? <ErrorState error={deleteError} /> : null}
    </PlanningRouteSection>
  );
}

function PlanningRouteSection(props: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <section className="panel planning-surface" aria-label={props.title}>
      <h1>{props.title}</h1>
      <p>{props.description}</p>
      {props.children}
    </section>
  );
}

function RequirementEditorForm(props: {
  formValues: RequirementFormValues;
  onChange: (nextValues: RequirementFormValues) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  submitLabel: string;
  submitDisabled: boolean;
}) {
  return (
    <form className="planning-form" onSubmit={props.onSubmit}>
      <label>
        Title
        <input
          required
          value={props.formValues.title}
          onChange={(event) =>
            props.onChange({
              ...props.formValues,
              title: event.target.value,
            })
          }
        />
      </label>

      <label>
        Description
        <textarea
          rows={3}
          value={props.formValues.description}
          onChange={(event) =>
            props.onChange({
              ...props.formValues,
              description: event.target.value,
            })
          }
        />
      </label>

      <label>
        Body
        <textarea
          rows={6}
          value={props.formValues.body}
          onChange={(event) =>
            props.onChange({
              ...props.formValues,
              body: event.target.value,
            })
          }
        />
      </label>

      <div className="planning-form-grid">
        <label>
          Priority
          <select
            value={props.formValues.priority}
            onChange={(event) =>
              props.onChange({
                ...props.formValues,
                priority: event.target.value as RequirementFormValues["priority"],
              })
            }
          >
            {REQUIREMENT_PRIORITY_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>

        <label>
          Status
          <select
            value={props.formValues.status}
            onChange={(event) =>
              props.onChange({
                ...props.formValues,
                status: event.target.value as RequirementFormValues["status"],
              })
            }
          >
            {REQUIREMENT_STATUS_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>

        <label>
          Source
          <input
            value={props.formValues.source}
            onChange={(event) =>
              props.onChange({
                ...props.formValues,
                source: event.target.value,
              })
            }
          />
        </label>
      </div>

      <label>
        Acceptance Criteria (one per line)
        <textarea
          rows={5}
          value={props.formValues.acceptanceCriteria}
          onChange={(event) =>
            props.onChange({
              ...props.formValues,
              acceptanceCriteria: event.target.value,
            })
          }
        />
      </label>

      <div className="planning-inline">
        <button type="submit" disabled={props.submitDisabled}>
          {props.submitLabel}
        </button>
      </div>
    </form>
  );
}

function LoadingState(props: { message: string }) {
  return <div className="loading-box">{props.message}</div>;
}

function EmptyState(props: { message: string }) {
  return <div className="empty-box">{props.message}</div>;
}

function StatusState(props: { message: string }) {
  return (
    <div className="status-box" role="status" aria-live="polite">
      {props.message}
    </div>
  );
}

function ErrorState(props: { error: ApiError }) {
  return (
    <div className="error-box" role="alert">
      <strong>Error:</strong> {props.error.code}
      <div>{props.error.message}</div>
      <div>exit code {props.error.exitCode}</div>
    </div>
  );
}

function planningRequirementPath(requirementId: string): string {
  return `/planning/requirements/${encodeURIComponent(requirementId)}`;
}

function parseMultiline(value: string): string[] {
  return value
    .split(/\r?\n/g)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function joinMultiline(values: string[]): string {
  return values.join("\n");
}

function normalizeOptionalText(value: string): string | undefined {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function defaultVisionFormValues(): VisionFormValues {
  return {
    projectName: "",
    problemStatement: "",
    targetUsers: "",
    goals: "",
    constraints: "",
    valueProposition: "",
  };
}

function visionToFormValues(vision: PlanningVisionDocument): VisionFormValues {
  return {
    projectName: extractVisionProjectName(vision),
    problemStatement: vision.problem_statement,
    targetUsers: joinMultiline(vision.target_users),
    goals: joinMultiline(vision.goals),
    constraints: joinMultiline(vision.constraints),
    valueProposition: vision.value_proposition ?? "",
  };
}

function extractVisionProjectName(vision: PlanningVisionDocument): string {
  const markdownLines = vision.markdown.split(/\r?\n/g);
  const nameLine = markdownLines.find((line) => line.trim().startsWith("- Name:"));
  if (!nameLine) {
    return "";
  }

  return nameLine.replace("- Name:", "").trim();
}

function visionFormToPayload(values: VisionFormValues) {
  return {
    project_name: normalizeOptionalText(values.projectName),
    problem_statement: values.problemStatement.trim(),
    target_users: parseMultiline(values.targetUsers),
    goals: parseMultiline(values.goals),
    constraints: parseMultiline(values.constraints),
    value_proposition: normalizeOptionalText(values.valueProposition),
  };
}

function validateVisionForm(values: VisionFormValues): Record<string, string> {
  const errors: Record<string, string> = {};

  if (!normalizeOptionalText(values.projectName)) {
    errors.projectName = "Project name is required.";
  }
  if (!normalizeOptionalText(values.problemStatement)) {
    errors.problemStatement = "Problem statement is required.";
  }
  if (parseMultiline(values.targetUsers).length === 0) {
    errors.targetUsers = "At least one target user is required.";
  }
  if (parseMultiline(values.goals).length === 0) {
    errors.goals = "At least one goal is required.";
  }
  if (parseMultiline(values.constraints).length === 0) {
    errors.constraints = "At least one constraint is required.";
  }
  if (!normalizeOptionalText(values.valueProposition)) {
    errors.valueProposition = "Value proposition is required.";
  }

  return errors;
}

function defaultRequirementFormValues(): RequirementFormValues {
  return {
    title: "",
    description: "",
    body: "",
    acceptanceCriteria: "",
    priority: "should",
    status: "draft",
    source: "ao-web",
  };
}

function requirementToFormValues(
  requirement: PlanningRequirementItem,
): RequirementFormValues {
  return {
    title: requirement.title,
    description: requirement.description,
    body: requirement.body ?? "",
    acceptanceCriteria: joinMultiline(requirement.acceptance_criteria),
    priority:
      requirement.priority === "must" ||
      requirement.priority === "could" ||
      requirement.priority === "wont"
        ? requirement.priority
        : "should",
    status:
      requirement.status === "refined" ||
      requirement.status === "planned" ||
      requirement.status === "in-progress" ||
      requirement.status === "done" ||
      requirement.status === "po-review" ||
      requirement.status === "em-review" ||
      requirement.status === "needs-rework" ||
      requirement.status === "approved" ||
      requirement.status === "implemented" ||
      requirement.status === "deprecated"
        ? requirement.status
        : "draft",
    source: requirement.source,
  };
}

function requirementCreatePayloadFromForm(
  values: RequirementFormValues,
): PlanningRequirementCreateInput {
  return {
    title: values.title.trim(),
    description: values.description.trim(),
    body: normalizeOptionalText(values.body),
    acceptance_criteria: parseMultiline(values.acceptanceCriteria),
    priority: values.priority,
    status: values.status,
    source: normalizeOptionalText(values.source),
  };
}

function requirementUpdatePayloadFromForm(
  values: RequirementFormValues,
): PlanningRequirementUpdateInput {
  return requirementCreatePayloadFromForm(values);
}
