import { useEffect, useId, useMemo, useState } from "react";

type LifecycleEventType = "request_start" | "request_success" | "request_failure";

type TelemetryError = {
  code: string;
  message: string;
  exitCode: number;
};

type SanitizedPayload = Record<string, unknown>;

type DiagnosticsFailureRecord = {
  id: string;
  eventType: Extract<LifecycleEventType, "request_failure">;
  action: string;
  method: "POST" | "PUT" | "PATCH" | "DELETE";
  path: string;
  timestamp: string;
  durationMs: number;
  httpStatus?: number;
  correlationId: string;
  serverCorrelationId?: string;
  error: TelemetryError;
  request: SanitizedPayload;
  response?: SanitizedPayload;
};

const DIAGNOSTICS_CAP = 25;
const COPY_FEEDBACK_TIMEOUT_MS = 1200;

function toEpochMs(timestamp: string): number {
  const parsed = Date.parse(timestamp);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function toNewestFirstBounded(
  records: DiagnosticsFailureRecord[],
): DiagnosticsFailureRecord[] {
  return [...records]
    .sort((left, right) => toEpochMs(right.timestamp) - toEpochMs(left.timestamp))
    .slice(0, DIAGNOSTICS_CAP);
}

function formatUtc(timestamp: string): string {
  const parsed = Date.parse(timestamp);
  if (Number.isNaN(parsed)) {
    return timestamp;
  }

  const iso = new Date(parsed).toISOString();
  return `${iso.slice(0, 19)} UTC`;
}

function stringifyPayload(payload: unknown): string {
  return JSON.stringify(payload ?? { bodyPreview: "[NON_JSON_PAYLOAD]" }, null, 2);
}

const daemonFailureSample: DiagnosticsFailureRecord = {
  id: "fail-daemon-stop-20260225T164104Z",
  eventType: "request_failure",
  action: "daemon.stop",
  method: "POST",
  path: "/api/v1/daemon/stop",
  timestamp: "2026-02-25T16:41:04.181Z",
  durationMs: 1280,
  httpStatus: 503,
  correlationId: "ao-corr-20260225-164103-8f1a9d2c",
  error: {
    code: "unavailable",
    message: "Runner socket timeout",
    exitCode: 5,
  },
  request: {
    method: "POST",
    path: "/api/v1/daemon/stop",
    headers: {
      accept: "application/json",
      "content-type": "application/json",
      "x-ao-correlation-id": "ao-corr-20260225-164103-8f1a9d2c",
      authorization: "[REDACTED]",
    },
    query: {},
    body: {},
  },
  response: {
    status: 503,
    headers: {
      "x-ao-correlation-id": "ao-srv-9ba21e56",
    },
    body: {
      schema: "ao.cli.v1",
      ok: false,
      error: {
        code: "unavailable",
        message: "Runner socket timeout",
        exit_code: 5,
      },
    },
  },
  serverCorrelationId: "ao-srv-9ba21e56",
};

const handoffFailureSample: DiagnosticsFailureRecord = {
  id: "fail-handoff-submit-20260225T164813Z",
  eventType: "request_failure",
  action: "review.handoff.submit",
  method: "POST",
  path: "/api/v1/review/handoff",
  timestamp: "2026-02-25T16:48:13.027Z",
  durationMs: 442,
  httpStatus: 409,
  correlationId: "ao-srv-ecf5211b",
  error: {
    code: "conflict",
    message: "active handoff already exists",
    exitCode: 4,
  },
  request: {
    method: "POST",
    path: "/api/v1/review/handoff",
    query: {
      session: "[REDACTED]",
    },
    body: {
      run_id: "run_7kqp12",
      target_role: "reviewer",
      question: "Can QA approve this handoff?",
      context: {
        api_key: "[REDACTED]",
        requester: "ops",
      },
    },
  },
  response: {
    status: 409,
    headers: {
      "x-ao-correlation-id": "ao-srv-ecf5211b",
    },
    bodyPreview: "[NON_JSON_PAYLOAD]",
  },
  serverCorrelationId: "ao-srv-ecf5211b",
};

export function DaemonDiagnosticsScreen() {
  const [records, setRecords] = useState<DiagnosticsFailureRecord[]>([
    daemonFailureSample,
    {
      ...daemonFailureSample,
      id: "fail-daemon-clear-20260225T162051Z",
      action: "daemon.clear_logs",
      path: "/api/v1/daemon/clear-logs",
      timestamp: "2026-02-25T16:20:51.310Z",
      durationMs: 304,
      httpStatus: 409,
      correlationId: "ao-corr-20260225-162050-c102f777",
      error: {
        code: "conflict",
        message: "log clear already in progress",
        exitCode: 4,
      },
    },
  ]);

  const newestFirst = useMemo(() => toNewestFirstBounded(records), [records]);

  return (
    <section aria-label="Daemon diagnostics wireframe">
      <h1>Daemon</h1>
      <p>Control daemon actions and inspect sanitized failure diagnostics.</p>

      <div>
        <button type="button">Start</button>
        <button type="button">Pause</button>
        <button type="button">Resume</button>
        <button type="button">Stop</button>
        <button type="button">Clear Logs</button>
      </div>

      <DiagnosticsPanelWireframe
        title="Diagnostics Panel"
        records={newestFirst}
        onClear={() => setRecords([])}
      />
    </section>
  );
}

export function ReviewHandoffDiagnosticsScreen() {
  const [records, setRecords] = useState<DiagnosticsFailureRecord[]>([
    handoffFailureSample,
    daemonFailureSample,
  ]);
  const newestFirst = useMemo(() => toNewestFirstBounded(records), [records]);

  return (
    <section aria-label="Review handoff diagnostics wireframe">
      <h1>Review Handoff</h1>
      <p>Submit handoff payloads and triage failures without leaving the route.</p>

      <form>
        <label>
          Run ID
          <input value="run_7kqp12" readOnly />
        </label>
        <label>
          Target Role
          <select defaultValue="reviewer">
            <option value="reviewer">reviewer</option>
          </select>
        </label>
        <label>
          Question
          <textarea defaultValue="Can QA approve this handoff?" rows={3} />
        </label>
        <button type="submit">Submit Handoff</button>
      </form>

      <DiagnosticsPanelWireframe
        title="Diagnostics Panel"
        records={newestFirst}
        onClear={() => setRecords([])}
      />
    </section>
  );
}

function DiagnosticsPanelWireframe(props: {
  title: string;
  records: DiagnosticsFailureRecord[];
  onClear: () => void;
}) {
  const panelId = useId();
  const [expandedId, setExpandedId] = useState<string | null>(props.records[0]?.id ?? null);
  const [copyStatus, setCopyStatus] = useState<{
    recordId: string;
    mode: "copied" | "manual";
  } | null>(null);

  useEffect(() => {
    const hasExpandedRecord = props.records.some((record) => record.id === expandedId);
    if (!hasExpandedRecord) {
      setExpandedId(props.records[0]?.id ?? null);
    }
  }, [expandedId, props.records]);

  const onCopyCorrelation = async (record: DiagnosticsFailureRecord) => {
    if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
      setCopyStatus({ recordId: record.id, mode: "manual" });
      return;
    }

    try {
      await navigator.clipboard.writeText(record.correlationId);
      setCopyStatus({ recordId: record.id, mode: "copied" });
      window.setTimeout(() => {
        setCopyStatus((current) =>
          current?.recordId === record.id ? null : current,
        );
      }, COPY_FEEDBACK_TIMEOUT_MS);
    } catch {
      setCopyStatus({ recordId: record.id, mode: "manual" });
    }
  };

  const headingId = `${panelId}-heading`;
  const summaryId = `${panelId}-summary`;
  const mostRecentFailure = props.records[0];
  const liveMessage =
    props.records.length === 0
      ? "Diagnostics cleared for this local session."
      : `Diagnostics updated with ${props.records.length} retained failure ${
          props.records.length === 1 ? "record" : "records"
        }.`;

  return (
    <section aria-labelledby={headingId} aria-describedby={summaryId}>
      <header>
        <h2 id={headingId}>{props.title}</h2>
        <p id={summaryId}>
          Retains latest {DIAGNOSTICS_CAP} failures in memory (newest first).{" "}
          {mostRecentFailure
            ? `Most recent failure: ${formatUtc(mostRecentFailure.timestamp)}.`
            : "No failures recorded yet."}
        </p>
        <button type="button" onClick={props.onClear} disabled={props.records.length === 0}>
          Clear diagnostics
        </button>
      </header>

      <div aria-live="polite">{liveMessage}</div>

      {props.records.length === 0 ? (
        <p>No failed actions in this local session.</p>
      ) : (
        <ol>
          {props.records.map((record) => {
            const isExpanded = expandedId === record.id;
            const detailId = `${panelId}-detail-${record.id}`;
            const activeCopyState =
              copyStatus?.recordId === record.id ? copyStatus.mode : null;

            return (
              <li key={record.id}>
                <button
                  type="button"
                  aria-expanded={isExpanded}
                  aria-controls={detailId}
                  onClick={() => setExpandedId((current) => (current === record.id ? null : record.id))}
                >
                  <span>{record.action}</span>
                  <span>
                    {record.method} {record.path}
                  </span>
                  <span>
                    {record.error.code} | {record.httpStatus ?? "-"} | {record.durationMs}ms
                  </span>
                  <span>{formatUtc(record.timestamp)}</span>
                </button>

                {isExpanded ? (
                  <div id={detailId}>
                    <h3>Request metadata</h3>
                    <dl>
                      <div>
                        <dt>Timestamp</dt>
                        <dd>{formatUtc(record.timestamp)}</dd>
                      </div>
                      <div>
                        <dt>Duration</dt>
                        <dd>{record.durationMs}ms</dd>
                      </div>
                      <div>
                        <dt>HTTP status</dt>
                        <dd>{record.httpStatus ?? "n/a"}</dd>
                      </div>
                      <div>
                        <dt>Request</dt>
                        <dd>
                          {record.method} {record.path}
                        </dd>
                      </div>
                    </dl>

                    <h3>Correlation</h3>
                    <code>{record.correlationId}</code>
                    {record.serverCorrelationId ? (
                      <p>Server canonical correlation: {record.serverCorrelationId}</p>
                    ) : null}
                    <button
                      type="button"
                      aria-label={`Copy correlation ID for ${record.action} failure`}
                      onClick={() => {
                        void onCopyCorrelation(record);
                      }}
                    >
                      Copy ID
                    </button>
                    {activeCopyState === "copied" ? <span role="status">Copied</span> : null}
                    {activeCopyState === "manual" ? (
                      <p role="status">
                        Clipboard unavailable. Select the correlation value above for manual copy.
                      </p>
                    ) : null}

                    <h3>Normalized error</h3>
                    <pre>{JSON.stringify(record.error, null, 2)}</pre>

                    <h3>Request (sanitized)</h3>
                    <pre>{stringifyPayload(record.request)}</pre>

                    <h3>Response (sanitized)</h3>
                    <pre>{stringifyPayload(record.response)}</pre>
                  </div>
                ) : null}
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
