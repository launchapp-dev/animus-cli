import { useMemo, useState, type ReactNode } from "react";

type GateStatus = "queued" | "running" | "passed" | "failed" | "blocked" | "cancelled";
type ChecklistStatus = "draft" | "ready-for-go" | "blocked" | "signed-off";
type RollbackOutcome =
  | "idle"
  | "validation-error"
  | "submitted"
  | "candidate-failed"
  | "rollback-failed"
  | "both-passed";
type EvidenceState = "missing" | "linked" | "downloaded";
type TriggerEvent = "pull_request" | "push";
type TraceabilityId =
  | "AC-01"
  | "AC-02"
  | "AC-03"
  | "AC-04"
  | "AC-05"
  | "AC-06"
  | "AC-07"
  | "AC-08"
  | "AC-09"
  | "AC-10"
  | "AC-11"
  | "AC-12"
  | "AC-13"
  | "AC-14";

type GateJob = {
  id: string;
  name: string;
  nodeVersion?: "20.x" | "22.x";
  runtime: string;
  status: GateStatus;
  details: string;
};

type SmokeAssertion = {
  id: string;
  label: string;
  status: "passed" | "failed";
  details: string;
};

type ArtifactReference = {
  name: "web-ui-smoke-e2e-server-log" | "web-ui-smoke-e2e-assertions";
  note: string;
  retentionDays: number;
  state: EvidenceState;
};

type ReleaseGateSnapshot = {
  ref: string;
  gateJob: GateJob;
  buildMatrixStatus: GateStatus;
  publishStatus: GateStatus;
};

type ChecklistEvidenceItem = {
  label: string;
  runUrl: string;
  status: GateStatus;
  summary: string;
};

type ChecklistEvidence = {
  matrix: ChecklistEvidenceItem;
  smoke: ChecklistEvidenceItem;
  releaseGate: ChecklistEvidenceItem;
};

type ChecklistModel = {
  releaseRef: string;
  operator: string;
  releaseRunId: string;
  rollbackWorkflow: string;
  evidence: ChecklistEvidence;
  preflightComplete: boolean;
  matrixVerified: boolean;
  smokeVerified: boolean;
  embeddedAssetsVerified: boolean;
  rollbackPreconditionsVerified: boolean;
  postReleaseVerified: boolean;
  decision: "go" | "no-go" | null;
  notes: string;
  signedOff: boolean;
};

type RollbackValidationModel = {
  dispatchState: "idle" | "validation-error" | "submitted";
  candidateRef: string;
  rollbackRef: string;
  candidateStatus: GateStatus;
  rollbackStatus: GateStatus;
  candidateDetails: string;
  rollbackDetails: string;
};

type CommandSurface = "release-topology" | "checks" | "checklist" | "rollback" | "release-decision";
type DecisionDrawerState = "missing-evidence" | "ready-for-go" | "blocked";
type CommandTrigger = "tag_push" | "workflow_dispatch";

type CommandCenterRun = {
  id: string;
  trigger: CommandTrigger;
  startedAtUtc: string;
  status: GateStatus;
  blocker: string | null;
};

type GoDecisionEvidenceItem = {
  lane: "matrix" | "smoke" | "rollback";
  runRef: string;
  status: GateStatus;
  operatorState: EvidenceState;
  details: string;
};

type GoDecisionConfirmations = {
  matrixAndSmokeMatched: boolean;
  rollbackValidated: boolean;
  postReleasePlanPrepared: boolean;
};

const webUiCiJobs: GateJob[] = [
  {
    id: "matrix-20",
    name: "web-ui-matrix (node 20.x)",
    nodeVersion: "20.x",
    runtime: "2m 46s",
    status: "passed",
    details: "npm ci + test + build complete",
  },
  {
    id: "matrix-22",
    name: "web-ui-matrix (node 22.x)",
    nodeVersion: "22.x",
    runtime: "2m 52s",
    status: "passed",
    details: "npm ci + test + build complete",
  },
  {
    id: "smoke",
    name: "web-ui-smoke-e2e",
    nodeVersion: "22.x",
    runtime: "1m 11s",
    status: "failed",
    details: "T-06 api_only=true deep-link rejection mismatch",
  },
];

const workflowTriggerEvents: TriggerEvent[] = ["pull_request", "push"];
const workflowTriggerPaths = [
  "crates/orchestrator-web-server/**",
  ".github/workflows/web-ui-ci.yml",
  ".github/workflows/release.yml",
  ".github/workflows/release-rollback-validation.yml",
  ".github/release-checklists/web-gui-release.md",
];
const gateLifecycleStates: GateStatus[] = [
  "queued",
  "running",
  "passed",
  "failed",
  "blocked",
  "cancelled",
];

const smokeAssertions: SmokeAssertion[] = [
  { id: "t04-root", label: "T-04 / returns HTML 200", status: "passed", details: "text/html" },
  {
    id: "t04-dashboard",
    label: "T-04 /dashboard returns HTML 200",
    status: "passed",
    details: "text/html",
  },
  {
    id: "t04-projects",
    label: "T-04 /projects returns HTML 200",
    status: "passed",
    details: "text/html",
  },
  {
    id: "t04-handoff",
    label: "T-04 /reviews/handoff returns HTML 200",
    status: "passed",
    details: "text/html",
  },
  {
    id: "t05-system-info",
    label: "T-05 /api/v1/system/info returns ao.cli.v1 success envelope",
    status: "passed",
    details: "schema=ao.cli.v1, ok=true",
  },
  {
    id: "t06-api-only",
    label: "T-06 api_only=true rejects /dashboard",
    status: "failed",
    details: "received code=invalid_input, exit_code=2; expected code=not_found, exit_code=3",
  },
];

const smokeFailureArtifacts: ArtifactReference[] = [
  {
    name: "web-ui-smoke-e2e-server-log",
    note: "captured AO web server stdout/stderr",
    retentionDays: 7,
    state: "linked",
  },
  {
    name: "web-ui-smoke-e2e-assertions",
    note: "deterministic smoke assertion report output",
    retentionDays: 7,
    state: "downloaded",
  },
];

const releaseSnapshot: ReleaseGateSnapshot = {
  ref: "refs/tags/v0.18.0",
  gateJob: {
    id: "web-ui-gates",
    name: "Web UI Gates",
    runtime: "6m 55s",
    status: "failed",
    details: "Smoke E2E assertion T-06 failed",
  },
  buildMatrixStatus: "blocked",
  publishStatus: "blocked",
};

const initialChecklist: ChecklistModel = {
  releaseRef: "v0.18.0",
  operator: "sam.i",
  releaseRunId: "#1097",
  rollbackWorkflow: "release-rollback-validation.yml (workflow_dispatch)",
  evidence: {
    matrix: {
      label: "Frontend matrix",
      runUrl: "https://github.com/org/ao/actions/runs/1091",
      status: "passed",
      summary: "Node 20.x and 22.x completed",
    },
    smoke: {
      label: "Smoke E2E",
      runUrl: "https://github.com/org/ao/actions/runs/1091/job/web-ui-smoke-e2e",
      status: "failed",
      summary: "T-06 blocker recorded with artifacts",
    },
    releaseGate: {
      label: "Release gate",
      runUrl: "https://github.com/org/ao/actions/runs/1097/job/web-ui-gates",
      status: "blocked",
      summary: "build/publish blocked by failed smoke",
    },
  },
  preflightComplete: true,
  matrixVerified: true,
  smokeVerified: true,
  embeddedAssetsVerified: true,
  rollbackPreconditionsVerified: false,
  postReleaseVerified: false,
  decision: "no-go",
  notes: "No-Go while smoke assertion T-06 is unresolved.",
  signedOff: false,
};

const rollbackSample: RollbackValidationModel = {
  dispatchState: "submitted",
  candidateRef: "v0.18.0-rc.2",
  rollbackRef: "v0.17.4",
  candidateStatus: "failed",
  rollbackStatus: "passed",
  candidateDetails: "api_only assertion mismatch",
  rollbackDetails: "all smoke assertions passed",
};

const goReadyRun: CommandCenterRun = {
  id: "#1103",
  trigger: "workflow_dispatch",
  startedAtUtc: "2026-02-26 02:44 UTC",
  status: "passed",
  blocker: null,
};

const goDecisionEvidence: GoDecisionEvidenceItem[] = [
  {
    lane: "matrix",
    runRef: "#1103 / web-ui-matrix (node 20.x), web-ui-matrix (node 22.x)",
    status: "passed",
    operatorState: "downloaded",
    details: "both matrix rows passed on same release commit SHA",
  },
  {
    lane: "smoke",
    runRef: "#1103 / web-ui-smoke-e2e",
    status: "passed",
    operatorState: "downloaded",
    details: "all deterministic assertions including T-06 passed",
  },
  {
    lane: "rollback",
    runRef: "#1101 / release-rollback-validation",
    status: "passed",
    operatorState: "linked",
    details: "candidate_ref and rollback_ref both passed smoke",
  },
];

const commandSurfaces: Array<{ id: CommandSurface; label: string }> = [
  { id: "release-topology", label: "Release gate topology" },
  { id: "checks", label: "PR checks + smoke triage" },
  { id: "checklist", label: "Release checklist" },
  { id: "rollback", label: "Rollback validation" },
  { id: "release-decision", label: "Release decision" },
];

const requiredCheckNames = [
  "web-ui-matrix (node 20.x)",
  "web-ui-matrix (node 22.x)",
  "web-ui-smoke-e2e",
  "Web UI Gates",
];

const scopeLockedImplementationFiles = [
  ".github/workflows/web-ui-ci.yml",
  ".github/workflows/release.yml",
  ".github/workflows/release-rollback-validation.yml",
  ".github/release-checklists/web-gui-release.md",
  "crates/orchestrator-web-server/web-ui/package.json",
  "crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs",
];

const commandCenterRuns: CommandCenterRun[] = [
  {
    id: "#1103",
    trigger: "workflow_dispatch",
    startedAtUtc: "2026-02-26 02:44 UTC",
    status: "passed",
    blocker: null,
  },
  {
    id: "#1098",
    trigger: "workflow_dispatch",
    startedAtUtc: "2026-02-25 19:15 UTC",
    status: "running",
    blocker: "rollback validation summary pending",
  },
  {
    id: "#1097",
    trigger: "tag_push",
    startedAtUtc: "2026-02-25 18:03 UTC",
    status: "failed",
    blocker: "T-06 deep-link rejection mismatch",
  },
  {
    id: "#1096",
    trigger: "tag_push",
    startedAtUtc: "2026-02-25 17:12 UTC",
    status: "blocked",
    blocker: "awaiting smoke rerun artifacts",
  },
  {
    id: "#1095",
    trigger: "tag_push",
    startedAtUtc: "2026-02-25 15:41 UTC",
    status: "passed",
    blocker: null,
  },
];

export const acceptanceTraceability: Record<TraceabilityId, string[]> = {
  "AC-01": [
    "Web UI CI workflow screen models pull_request/push trigger context, path filters, and stable job names.",
  ],
  "AC-02": ["Matrix rows include Node 20.x and 22.x outcomes."],
  "AC-03": ["Smoke job represented as required gate in CI and release contexts."],
  "AC-04": ["Smoke assertions include route HTML and /api/v1/system/info envelope checks."],
  "AC-05": ["api_only deep-link rejection assertion included as explicit row."],
  "AC-06": [
    "Release gate topology and command-center screens model blocking web-ui-gates prerequisite.",
  ],
  "AC-07": ["Build and publish statuses derive to blocked when gate fails."],
  "AC-08": ["Passing path note keeps artifact naming/publish behavior unchanged."],
  "AC-09": [
    "Checklist and command-center decision flows model both no-go and go-ready evidence progression before sign-off.",
  ],
  "AC-10": ["Rollback dispatch requires candidate_ref and rollback_ref values."],
  "AC-11": ["Rollback summary exposes per-ref pass/fail state for audit."],
  "AC-12": ["Smoke failure artifact references are deterministic and bounded retention."],
  "AC-13": ["Required checks are modeled with exact branch-protection names, including Web UI Gates."],
  "AC-14": ["Mockups include explicit implementation scope-lock guidance for allowed TASK-018 edit surfaces."],
};

function statusText(status: GateStatus): string {
  return status;
}

function evidenceStateText(state: EvidenceState): string {
  return state;
}

function triggerText(trigger: CommandTrigger): string {
  if (trigger === "workflow_dispatch") {
    return "workflow dispatch";
  }

  return "tag push";
}

function isGateBlocking(status: GateStatus): boolean {
  return status === "failed" || status === "blocked" || status === "cancelled";
}

function toDecisionDrawerState(
  runStatus: GateStatus,
  evidenceStates: EvidenceState[],
): DecisionDrawerState {
  if (isGateBlocking(runStatus)) {
    return "blocked";
  }

  if (evidenceStates.some((state) => state === "missing")) {
    return "missing-evidence";
  }

  return "ready-for-go";
}

function decisionDrawerGateStatus(state: DecisionDrawerState): GateStatus {
  if (state === "ready-for-go") {
    return "passed";
  }

  if (state === "blocked") {
    return "failed";
  }

  return "running";
}

function toChecklistStatus(model: ChecklistModel): ChecklistStatus {
  if (model.signedOff && model.decision === "go") {
    return "signed-off";
  }

  if (model.decision === "no-go") {
    return "blocked";
  }

  if (
    model.preflightComplete &&
    model.matrixVerified &&
    model.smokeVerified &&
    model.embeddedAssetsVerified &&
    model.rollbackPreconditionsVerified &&
    model.postReleaseVerified &&
    model.decision === "go"
  ) {
    return "ready-for-go";
  }

  return "draft";
}

function toRollbackOutcome(model: RollbackValidationModel): RollbackOutcome {
  if (model.dispatchState === "validation-error") {
    return "validation-error";
  }

  if (model.dispatchState === "idle") {
    return "idle";
  }

  if (model.candidateStatus === "passed" && model.rollbackStatus === "passed") {
    return "both-passed";
  }

  if (model.candidateStatus === "failed") {
    return "candidate-failed";
  }

  if (model.rollbackStatus === "failed") {
    return "rollback-failed";
  }

  return "submitted";
}

function toRollbackEvidenceState(status: GateStatus): EvidenceState {
  if (status === "passed") {
    return "downloaded";
  }

  if (status === "queued" || status === "running") {
    return "linked";
  }

  return "missing";
}

function StatusBadge(props: { status: GateStatus }): ReactNode {
  return <span aria-label={`status ${statusText(props.status)}`}>{statusText(props.status)}</span>;
}

export function ReleaseGateCommandCenterScreen(): ReactNode {
  const [activeSurface, setActiveSurface] = useState<CommandSurface>("release-topology");
  const [selectedRunId, setSelectedRunId] = useState<string>("#1097");
  const selectedRun = useMemo(
    () => commandCenterRuns.find((run) => run.id === selectedRunId) ?? commandCenterRuns[0],
    [selectedRunId],
  );

  if (!selectedRun) {
    return (
      <section aria-label="Release gate command center wireframe">
        <h1>Release Gate Command Center</h1>
        <p role="alert">No release runs are available for selection.</p>
      </section>
    );
  }

  const matrixEvidenceState: EvidenceState = "linked";
  const smokeEvidenceState: EvidenceState =
    selectedRun.status === "failed" || selectedRun.status === "passed" ? "downloaded" : "linked";
  const rollbackEvidenceState: EvidenceState = toRollbackEvidenceState(selectedRun.status);
  const decisionState = toDecisionDrawerState(selectedRun.status, [
    matrixEvidenceState,
    smokeEvidenceState,
    rollbackEvidenceState,
  ]);
  const completedGates =
    selectedRun.status === "passed" ? 3 : selectedRun.status === "running" ? 2 : 1;

  return (
    <section aria-label="Release gate command center wireframe">
      <h1>Release Gate Command Center</h1>
      <p>Selected release run: {selectedRun.id}</p>
      <p>
        Surface: <strong>{commandSurfaces.find((surface) => surface.id === activeSurface)?.label}</strong>
      </p>

      <nav aria-label="Command center surfaces">
        <ul>
          {commandSurfaces.map((surface) => (
            <li key={surface.id}>
              <button
                type="button"
                aria-pressed={activeSurface === surface.id}
                onClick={() => setActiveSurface(surface.id)}
              >
                {surface.label}
              </button>
            </li>
          ))}
        </ul>
      </nav>

      <h2>Gate metrics</h2>
      <ul>
        <li>
          required gates complete: {completedGates}/3 [{statusText(selectedRun.status)}]
        </li>
        <li>
          smoke blocker: {selectedRun.blocker ?? "none"}
        </li>
        <li>
          rollback readiness evidence: {evidenceStateText(rollbackEvidenceState)}
        </li>
      </ul>

      <h2>Recent release runs</h2>
      <table>
        <thead>
          <tr>
            <th>run</th>
            <th>trigger</th>
            <th>started</th>
            <th>status</th>
            <th>blocker</th>
            <th>select</th>
          </tr>
        </thead>
        <tbody>
          {commandCenterRuns.map((run) => (
            <tr key={run.id}>
              <td>{run.id}</td>
              <td>{triggerText(run.trigger)}</td>
              <td>{run.startedAtUtc}</td>
              <td>
                <StatusBadge status={run.status} />
              </td>
              <td>{run.blocker ?? "none"}</td>
              <td>
                <button type="button" onClick={() => setSelectedRunId(run.id)}>
                  {selectedRun.id === run.id ? "selected" : "select"}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <h2>Decision drawer</h2>
      <p>
        decision state: <StatusBadge status={decisionDrawerGateStatus(decisionState)} /> ({decisionState})
      </p>
      <ul>
        <li>matrix evidence: {evidenceStateText(matrixEvidenceState)}</li>
        <li>smoke evidence: {evidenceStateText(smokeEvidenceState)}</li>
        <li>rollback evidence: {evidenceStateText(rollbackEvidenceState)}</li>
      </ul>

      {decisionState === "ready-for-go" ? (
        <p role="status">All evidence is attached. Run can move to go decision.</p>
      ) : (
        <p role="alert">
          Decision remains no-go until smoke and rollback evidence are complete.
        </p>
      )}
    </section>
  );
}

export function WebUiCiRunScreen(): ReactNode {
  const failedAssertions = smokeAssertions.filter((assertion) => assertion.status === "failed");

  return (
    <section aria-label="Web UI CI run wireframe">
      <h1>web-ui-ci</h1>
      <p>
        Required checks for deterministic web GUI quality gates before merge and release.
      </p>
      <p>trigger events: {workflowTriggerEvents.join(", ")}</p>
      <p>trigger paths:</p>
      <ul>
        {workflowTriggerPaths.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <p>workflow lifecycle states: {gateLifecycleStates.map(statusText).join(", ")}</p>
      <p>required check names:</p>
      <ul>
        {requiredCheckNames.map((name) => (
          <li key={name}>
            <code>{name}</code>
          </li>
        ))}
      </ul>

      <table>
        <thead>
          <tr>
            <th>job</th>
            <th>node</th>
            <th>runtime</th>
            <th>status</th>
            <th>details</th>
          </tr>
        </thead>
        <tbody>
          {webUiCiJobs.map((job) => (
            <tr key={job.id}>
              <td>{job.name}</td>
              <td>{job.nodeVersion ?? "-"}</td>
              <td>{job.runtime}</td>
              <td>
                <StatusBadge status={job.status} />
              </td>
              <td>{job.details}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h2>Smoke assertion contract</h2>
      <ol>
        {smokeAssertions.map((assertion) => (
          <li key={assertion.id}>
            [{assertion.status}] {assertion.label} ({assertion.details})
          </li>
        ))}
      </ol>

      <h2>Failure artifacts</h2>
      <ul>
        {smokeFailureArtifacts.map((artifact) => (
          <li key={artifact.name}>
            {artifact.name} ({artifact.note}; retention {artifact.retentionDays} days; evidence state{" "}
            {evidenceStateText(artifact.state)})
          </li>
        ))}
      </ul>

      {failedAssertions.length > 0 ? (
        <p role="alert">
          Failed assertions: {failedAssertions.length}. Open artifact logs and rerun smoke.
        </p>
      ) : (
        <p role="status">All smoke assertions passed.</p>
      )}
    </section>
  );
}

export function ReleaseGateTopologyScreen(): ReactNode {
  const gateBlocking = isGateBlocking(releaseSnapshot.gateJob.status);

  return (
    <section aria-label="Release gate topology wireframe">
      <h1>Release Gate Topology</h1>
      <p>ref: {releaseSnapshot.ref}</p>
      <p>
        {releaseSnapshot.gateJob.name} (<code>{releaseSnapshot.gateJob.id}</code> job key):{" "}
        <StatusBadge status={releaseSnapshot.gateJob.status} /> (
        {releaseSnapshot.gateJob.details})
      </p>
      <p>
        build matrix: <StatusBadge status={releaseSnapshot.buildMatrixStatus} />
      </p>
      <p>
        publish: <StatusBadge status={releaseSnapshot.publishStatus} />
      </p>

      {gateBlocking ? (
        <p role="alert">
          Release is fail-closed. Build and publish remain blocked until web-ui-gates passes.
        </p>
      ) : (
        <p role="status">
          Gate passed. Existing artifact naming and publish behavior remains unchanged.
        </p>
      )}

      <h2>TASK-018 scope lock</h2>
      <p>Implementation edits are restricted to these files:</p>
      <ul>
        {scopeLockedImplementationFiles.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function ReleaseChecklistScreen(): ReactNode {
  const [model, setModel] = useState<ChecklistModel>(initialChecklist);
  const checklistStatus = useMemo(() => toChecklistStatus(model), [model]);
  const evidenceItems = [model.evidence.matrix, model.evidence.smoke, model.evidence.releaseGate];

  return (
    <section aria-label="Release checklist wireframe">
      <h1>Web GUI Release Checklist</h1>
      <p>status: {checklistStatus}</p>

      <form>
        <fieldset>
          <legend>Metadata</legend>
          <label>
            Release ref
            <input value={model.releaseRef} readOnly />
          </label>
          <label>
            Operator
            <input value={model.operator} readOnly />
          </label>
          <label>
            Release run
            <input value={model.releaseRunId} readOnly />
          </label>
        </fieldset>

        <fieldset>
          <legend>Preflight</legend>
          <label>
            <input
              type="checkbox"
              checked={model.preflightComplete}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  preflightComplete: event.currentTarget.checked,
                }))
              }
            />
            Release notes and tag metadata reviewed
          </label>
        </fieldset>

        <fieldset>
          <legend>CI Gate Evidence</legend>
          <ul>
            {evidenceItems.map((item) => (
              <li key={item.label}>
                {item.label}: <a href={item.runUrl}>{item.runUrl}</a> [<StatusBadge status={item.status} />] (
                {item.summary})
              </li>
            ))}
          </ul>
          <label>
            <input
              type="checkbox"
              checked={model.matrixVerified}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  matrixVerified: event.currentTarget.checked,
                }))
              }
            />
            Matrix evidence acknowledged
          </label>
          <label>
            <input
              type="checkbox"
              checked={model.smokeVerified}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  smokeVerified: event.currentTarget.checked,
                }))
              }
            />
            Smoke evidence acknowledged
          </label>
        </fieldset>

        <fieldset>
          <legend>Decision</legend>
          <label>
            <input
              type="radio"
              name="decision"
              checked={model.decision === "go"}
              onChange={() => setModel((current) => ({ ...current, decision: "go" }))}
            />
            Go
          </label>
          <label>
            <input
              type="radio"
              name="decision"
              checked={model.decision === "no-go"}
              onChange={() => setModel((current) => ({ ...current, decision: "no-go" }))}
            />
            No-Go
          </label>
          <label>
            Decision notes
            <textarea
              value={model.notes}
              rows={3}
              onChange={(event) =>
                setModel((current) => ({ ...current, notes: event.currentTarget.value }))
              }
            />
          </label>
        </fieldset>

        <fieldset>
          <legend>Rollback Readiness</legend>
          <label>
            Rollback trigger workflow
            <input value={model.rollbackWorkflow} readOnly />
          </label>
          <label>
            <input
              type="checkbox"
              checked={model.rollbackPreconditionsVerified}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  rollbackPreconditionsVerified: event.currentTarget.checked,
                }))
              }
            />
            Rollback preconditions validated
          </label>
        </fieldset>

        <fieldset>
          <legend>Post-release Verification</legend>
          <label>
            <input
              type="checkbox"
              checked={model.embeddedAssetsVerified}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  embeddedAssetsVerified: event.currentTarget.checked,
                }))
              }
            />
            Embedded asset regeneration evidence captured
          </label>
          <label>
            <input
              type="checkbox"
              checked={model.postReleaseVerified}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  postReleaseVerified: event.currentTarget.checked,
                }))
              }
            />
            Post-release route/API smoke verification completed
          </label>
          <label>
            <input
              type="checkbox"
              checked={model.signedOff}
              onChange={(event) =>
                setModel((current) => ({
                  ...current,
                  signedOff: event.currentTarget.checked,
                }))
              }
            />
            Operator sign-off recorded
          </label>
        </fieldset>
      </form>
    </section>
  );
}

export function RollbackValidationScreen(): ReactNode {
  const [model] = useState<RollbackValidationModel>(rollbackSample);
  const outcome = useMemo(() => toRollbackOutcome(model), [model]);
  const deterministicSummary = [
    `candidate_ref=${model.candidateRef} smoke=${statusText(model.candidateStatus)} reason=${model.candidateDetails}`,
    `rollback_ref=${model.rollbackRef} smoke=${statusText(model.rollbackStatus)} reason=${model.rollbackDetails}`,
    "mutation=none publish=disabled",
  ];

  return (
    <section aria-label="Rollback validation wireframe">
      <h1>Rollback Validation</h1>
      <p>
        Input fields: <code>candidate_ref</code> and <code>rollback_ref</code>.
      </p>
      <p>
        dispatch states: <code>idle</code>, <code>validation-error</code>, <code>submitted</code>.
        current: <strong>{model.dispatchState}</strong>
      </p>
      <p>Run compares refs side-by-side and does not mutate tags/releases.</p>

      <table>
        <thead>
          <tr>
            <th>ref</th>
            <th>smoke status</th>
            <th>details</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>{model.candidateRef}</td>
            <td>
              <StatusBadge status={model.candidateStatus} />
            </td>
            <td>{model.candidateDetails}</td>
          </tr>
          <tr>
            <td>{model.rollbackRef}</td>
            <td>
              <StatusBadge status={model.rollbackStatus} />
            </td>
            <td>{model.rollbackDetails}</td>
          </tr>
        </tbody>
      </table>

      <pre>{deterministicSummary.join("\n")}</pre>
      <p role="status">overall outcome: {outcome}</p>
    </section>
  );
}

export function ReleaseGoDecisionScreen(): ReactNode {
  const [operator, setOperator] = useState<string>("sam.i");
  const [notes, setNotes] = useState<string>(
    "All web-ui gates passed on run #1103 after fixing T-06 assertion expectation.",
  );
  const [confirmations, setConfirmations] = useState<GoDecisionConfirmations>({
    matrixAndSmokeMatched: true,
    rollbackValidated: true,
    postReleasePlanPrepared: true,
  });

  const evidenceReady = goDecisionEvidence.every((item) => item.status === "passed");
  const confirmationsComplete =
    confirmations.matrixAndSmokeMatched &&
    confirmations.rollbackValidated &&
    confirmations.postReleasePlanPrepared;
  const canSignOff =
    evidenceReady && confirmationsComplete && operator.trim().length > 0 && notes.trim().length > 0;

  return (
    <section aria-label="Release go decision wireframe">
      <h1>Release Decision (Go Ready)</h1>
      <p>
        selected run: {goReadyRun.id} ({triggerText(goReadyRun.trigger)})
      </p>
      <p>started: {goReadyRun.startedAtUtc}</p>

      <table>
        <thead>
          <tr>
            <th>lane</th>
            <th>run</th>
            <th>status</th>
            <th>operator state</th>
            <th>details</th>
          </tr>
        </thead>
        <tbody>
          {goDecisionEvidence.map((item) => (
            <tr key={item.lane}>
              <td>{item.lane}</td>
              <td>{item.runRef}</td>
              <td>
                <StatusBadge status={item.status} />
              </td>
              <td>{evidenceStateText(item.operatorState)}</td>
              <td>{item.details}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <form>
        <fieldset>
          <legend>Operator Sign-off</legend>
          <label>
            Operator
            <input value={operator} onChange={(event) => setOperator(event.currentTarget.value)} />
          </label>
          <label>
            <input
              type="checkbox"
              checked={confirmations.matrixAndSmokeMatched}
              onChange={(event) =>
                setConfirmations((current) => ({
                  ...current,
                  matrixAndSmokeMatched: event.currentTarget.checked,
                }))
              }
            />
            Matrix + smoke runs mapped to current release commit SHA.
          </label>
          <label>
            <input
              type="checkbox"
              checked={confirmations.rollbackValidated}
              onChange={(event) =>
                setConfirmations((current) => ({
                  ...current,
                  rollbackValidated: event.currentTarget.checked,
                }))
              }
            />
            Rollback comparison confirms candidate and rollback refs are healthy.
          </label>
          <label>
            <input
              type="checkbox"
              checked={confirmations.postReleasePlanPrepared}
              onChange={(event) =>
                setConfirmations((current) => ({
                  ...current,
                  postReleasePlanPrepared: event.currentTarget.checked,
                }))
              }
            />
            Post-release smoke rerun plan prepared for tagged artifact.
          </label>
          <label>
            Go notes
            <textarea value={notes} rows={3} onChange={(event) => setNotes(event.currentTarget.value)} />
          </label>
          <button type="button" disabled={!canSignOff}>
            Record Go Sign-off
          </button>
        </fieldset>
      </form>

      {canSignOff ? (
        <p role="status">All release gates and confirmations are complete. Go sign-off can be recorded.</p>
      ) : (
        <p role="alert">Sign-off is blocked until evidence and required confirmations are complete.</p>
      )}
    </section>
  );
}
