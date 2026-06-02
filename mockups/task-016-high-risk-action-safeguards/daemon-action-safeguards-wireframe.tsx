/**
 * TASK-016 wireframe scaffold.
 * Intent: provide concrete React-oriented guardrail logic and state shape
 * for high-risk daemon actions before build implementation.
 */

import { useMemo, useState } from "react";

type RiskLevel = "low" | "medium" | "high";
type GuardState = "idle" | "confirming-invalid" | "confirming-valid" | "submitting" | "failed-closed";
type GuardedAction = "daemon.start" | "daemon.pause" | "daemon.resume" | "daemon.stop" | "daemon.clear_logs";

type HttpMethod = "POST" | "DELETE";

type GuardDefinition = {
  action: GuardedAction;
  label: string;
  risk: RiskLevel;
  method: HttpMethod;
  path: string;
  typedIntentPhrase: string | null;
  impact: string;
  plannedEffects: string[];
  irreversibleEffects: string[];
  rollbackGuidance: string;
};

type DaemonSnapshot = {
  status: "running" | "paused" | "stopped";
  health: "ok" | "degraded";
  workers: number;
  bufferedLogs: number;
};

type FeedbackRecord = {
  id: string;
  timestamp: string;
  actor: string;
  action: GuardedAction;
  method: HttpMethod;
  path: string;
  outcome: "success" | "failure";
  message: string;
  code: string;
  correlationId: string;
};

const FEEDBACK_CAP = 50;

const guardRegistry: Record<GuardedAction, GuardDefinition> = {
  "daemon.start": {
    action: "daemon.start",
    label: "Start daemon",
    risk: "low",
    method: "POST",
    path: "/api/v1/daemon/start",
    typedIntentPhrase: null,
    impact: "Starts daemon runtime and enables run dispatch.",
    plannedEffects: ["Status transitions to running.", "Worker loops become eligible for dispatch."],
    irreversibleEffects: [],
    rollbackGuidance: "Run daemon.stop if immediate shutdown is required.",
  },
  "daemon.pause": {
    action: "daemon.pause",
    label: "Pause daemon",
    risk: "medium",
    method: "POST",
    path: "/api/v1/daemon/pause",
    typedIntentPhrase: null,
    impact: "Pauses dispatch while preserving active runtime state.",
    plannedEffects: ["New dispatch pauses.", "Current state remains available for resume."],
    irreversibleEffects: [],
    rollbackGuidance: "Run daemon.resume to continue dispatch.",
  },
  "daemon.resume": {
    action: "daemon.resume",
    label: "Resume daemon",
    risk: "low",
    method: "POST",
    path: "/api/v1/daemon/resume",
    typedIntentPhrase: null,
    impact: "Resumes dispatch from paused state.",
    plannedEffects: ["Status remains running.", "Queued work becomes dispatchable."],
    irreversibleEffects: [],
    rollbackGuidance: "Run daemon.pause if dispatch must be halted again.",
  },
  "daemon.stop": {
    action: "daemon.stop",
    label: "Stop daemon",
    risk: "high",
    method: "POST",
    path: "/api/v1/daemon/stop",
    typedIntentPhrase: "STOP DAEMON",
    impact: "Stops daemon and interrupts active workflow processing.",
    plannedEffects: [
      "Status transitions to stopping then stopped.",
      "Dispatch loop is halted for all queued operations.",
      "Diagnostics remains available for failure analysis.",
    ],
    irreversibleEffects: [
      "In-flight operations may require manual restart.",
      "Automation latency increases until daemon.start is run.",
    ],
    rollbackGuidance: "Run daemon.start and verify /api/v1/daemon/health reports ok.",
  },
  "daemon.clear_logs": {
    action: "daemon.clear_logs",
    label: "Clear daemon logs",
    risk: "high",
    method: "DELETE",
    path: "/api/v1/daemon/logs",
    typedIntentPhrase: "CLEAR DAEMON LOGS",
    impact: "Clears currently buffered daemon log lines from local UI history.",
    plannedEffects: [
      "Visible daemon log buffer is emptied.",
      "New logs continue streaming after clear.",
      "Metadata and diagnostics references remain available.",
    ],
    irreversibleEffects: [
      "Removed log body lines are not recoverable from this page.",
      "Only future log entries are visible after clear.",
    ],
    rollbackGuidance: "No direct rollback. Capture diagnostics before repeating clear.",
  },
};

const initialFeedback: FeedbackRecord[] = [
  {
    id: "fb-task016-0008",
    timestamp: "2026-02-25T18:24:12Z",
    actor: "demo.user",
    action: "daemon.clear_logs",
    method: "DELETE",
    path: "/api/v1/daemon/logs",
    outcome: "failure",
    message: "log truncation already in progress",
    code: "conflict",
    correlationId: "ao-corr-task016-0008",
  },
  {
    id: "fb-task016-0007",
    timestamp: "2026-02-25T18:22:07Z",
    actor: "demo.user",
    action: "daemon.resume",
    method: "POST",
    path: "/api/v1/daemon/resume",
    outcome: "success",
    message: "daemon resumed and workers unlocked",
    code: "ok",
    correlationId: "ao-corr-task016-0007",
  },
];

function normalizeIso(timestamp: string): string {
  const parsed = Date.parse(timestamp);
  if (Number.isNaN(parsed)) {
    return timestamp;
  }
  return new Date(parsed).toISOString().slice(0, 19) + " UTC";
}

function withFeedbackCap(records: FeedbackRecord[]): FeedbackRecord[] {
  return records.slice(0, FEEDBACK_CAP);
}

function prependFeedback(current: FeedbackRecord[], next: FeedbackRecord): FeedbackRecord[] {
  return withFeedbackCap([next, ...current]);
}

function typedIntentValid(action: GuardDefinition, rawInput: string): boolean {
  if (!action.typedIntentPhrase) {
    return true;
  }
  return rawInput.trim() === action.typedIntentPhrase;
}

function previewAvailable(action: GuardDefinition): boolean {
  return action.plannedEffects.length > 0 && action.rollbackGuidance.length > 0;
}

function preconditionsSatisfied(snapshot: DaemonSnapshot): boolean {
  return snapshot.health === "ok";
}

function nextCorrelation(action: GuardedAction, seed: number): string {
  return `ao-corr-${action.replace(".", "-")}-${seed.toString().padStart(4, "0")}`;
}

export function DaemonActionSafeguardsWireframe() {
  const [snapshot] = useState<DaemonSnapshot>({
    status: "running",
    health: "ok",
    workers: 3,
    bufferedLogs: 142,
  });

  const [feedback, setFeedback] = useState<FeedbackRecord[]>(initialFeedback);
  const [confirmingAction, setConfirmingAction] = useState<GuardDefinition | null>(null);
  const [typedIntent, setTypedIntent] = useState("");
  const [pendingAction, setPendingAction] = useState<GuardedAction | null>(null);
  const [failClosedMessage, setFailClosedMessage] = useState<string | null>(null);
  const [correlationSeed, setCorrelationSeed] = useState(9);

  const guardState: GuardState = useMemo(() => {
    if (failClosedMessage) {
      return "failed-closed";
    }
    if (pendingAction) {
      return "submitting";
    }
    if (!confirmingAction) {
      return "idle";
    }
    return typedIntentValid(confirmingAction, typedIntent)
      ? "confirming-valid"
      : "confirming-invalid";
  }, [confirmingAction, failClosedMessage, pendingAction, typedIntent]);

  const onRequestAction = (actionKey: GuardedAction) => {
    const action = guardRegistry[actionKey];

    if (!action || !previewAvailable(action)) {
      setFailClosedMessage("Guard metadata is missing. Refresh daemon diagnostics before retry.");
      return;
    }

    if (!preconditionsSatisfied(snapshot)) {
      setFailClosedMessage("Preconditions failed: daemon health is not ok. Resolve health checks first.");
      return;
    }

    if (action.risk === "high") {
      setFailClosedMessage(null);
      setTypedIntent("");
      setConfirmingAction(action);
      return;
    }

    void dispatchAction(action);
  };

  const dispatchAction = async (action: GuardDefinition) => {
    if (pendingAction) {
      return;
    }

    const correlationId = nextCorrelation(action.action, correlationSeed);
    setCorrelationSeed((seed) => seed + 1);
    setPendingAction(action.action);
    setFailClosedMessage(null);

    // Wireframe-only deterministic simulation.
    const simulatedFailure = action.action === "daemon.clear_logs";

    const nextFeedback: FeedbackRecord = simulatedFailure
      ? {
          id: `fb-${correlationId}`,
          timestamp: new Date().toISOString(),
          actor: "demo.user",
          action: action.action,
          method: action.method,
          path: action.path,
          outcome: "failure",
          message: "log truncation already in progress",
          code: "conflict",
          correlationId,
        }
      : {
          id: `fb-${correlationId}`,
          timestamp: new Date().toISOString(),
          actor: "demo.user",
          action: action.action,
          method: action.method,
          path: action.path,
          outcome: "success",
          message: `${action.label} completed after precondition revalidation`,
          code: "ok",
          correlationId,
        };

    setFeedback((current) => prependFeedback(current, nextFeedback));
    setPendingAction(null);
    setConfirmingAction(null);
    setTypedIntent("");
  };

  const onConfirmHighRisk = async () => {
    if (!confirmingAction) {
      return;
    }

    if (!typedIntentValid(confirmingAction, typedIntent)) {
      return;
    }

    await dispatchAction(confirmingAction);
  };

  const onCancelConfirmation = () => {
    setConfirmingAction(null);
    setTypedIntent("");
  };

  return (
    <section aria-label="Daemon guarded action wireframe">
      <h1>Daemon action safeguards</h1>
      <p>
        Guard state: <strong>{guardState}</strong>. Pending lock: {pendingAction ?? "none"}.
      </p>

      <div>
        <button type="button" onClick={() => onRequestAction("daemon.start")}>
          Start
        </button>
        <button type="button" onClick={() => onRequestAction("daemon.pause")}>
          Pause
        </button>
        <button type="button" onClick={() => onRequestAction("daemon.resume")}>
          Resume
        </button>
        <button type="button" onClick={() => onRequestAction("daemon.stop")}>
          Stop daemon
        </button>
        <button type="button" onClick={() => onRequestAction("daemon.clear_logs")}>
          Clear daemon logs
        </button>
      </div>

      <p>
        Snapshot: status={snapshot.status}, health={snapshot.health}, workers={snapshot.workers},
        bufferedLogs={snapshot.bufferedLogs}
      </p>
      <p>Preconditions: UI requires <code>health=ok</code> and revalidates success against server response.</p>

      {failClosedMessage ? (
        <p role="alert">Guardrail fail-closed: {failClosedMessage}</p>
      ) : null}

      {confirmingAction ? (
        <article
          role="dialog"
          aria-modal="true"
          aria-labelledby="risk-dialog-title"
          aria-describedby="risk-dialog-description"
        >
          <h2 id="risk-dialog-title">{confirmingAction.label}</h2>
          <p id="risk-dialog-description">{confirmingAction.impact}</p>
          <p>
            Request preview: <code>{confirmingAction.method}</code>{" "}
            <code>{confirmingAction.path}</code>
          </p>
          <ul>
            {confirmingAction.plannedEffects.map((effect) => (
              <li key={effect}>{effect}</li>
            ))}
          </ul>
          <ul>
            {confirmingAction.irreversibleEffects.map((effect) => (
              <li key={effect}>{effect}</li>
            ))}
          </ul>
          <p>Rollback guidance: {confirmingAction.rollbackGuidance}</p>
          <p>Success messaging must pass server-side state revalidation checks.</p>

          <label htmlFor="typed-intent-input">
            Type <code>{confirmingAction.typedIntentPhrase}</code>
          </label>
          <input
            id="typed-intent-input"
            value={typedIntent}
            onChange={(event) => setTypedIntent(event.target.value)}
            aria-invalid={!typedIntentValid(confirmingAction, typedIntent)}
          />

          <button type="button" onClick={onCancelConfirmation}>
            Cancel
          </button>
          <button
            type="button"
            onClick={() => {
              void onConfirmHighRisk();
            }}
            disabled={!typedIntentValid(confirmingAction, typedIntent)}
          >
            Confirm
          </button>
        </article>
      ) : null}

      <section aria-label="Guarded action feedback">
        <h2>Feedback ({feedback.length}/{FEEDBACK_CAP})</h2>
        <p aria-live="polite">Newest first. Correlation IDs align with diagnostics.</p>
        <ol>
          {feedback.map((row) => (
            <li key={row.id}>
              <strong>{row.outcome}</strong> {row.action} ({normalizeIso(row.timestamp)})
              <br />
              actor: <code>{row.actor}</code>
              <br />
              <code>
                {row.method} {row.path}
              </code>
              <br />
              {row.message} (code: {row.code})
              <br />
              correlation: <code>{row.correlationId}</code>
            </li>
          ))}
        </ol>
      </section>
    </section>
  );
}
