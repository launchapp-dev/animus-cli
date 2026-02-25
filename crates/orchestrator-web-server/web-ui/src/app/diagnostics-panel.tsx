import { useEffect, useId, useMemo, useState } from "react";

import {
  clearTelemetryEvents,
  FAILED_DIAGNOSTICS_CAPACITY,
  listFailedTelemetryEvents,
  subscribeTelemetryEvents,
} from "../lib/telemetry";
import type { FailureTelemetryEventRecord } from "../lib/telemetry";

type DiagnosticsPanelProps = {
  title: string;
  actionPrefixes?: string[];
  maxItems?: number;
};

export function DiagnosticsPanel(props: DiagnosticsPanelProps) {
  const maxItems = props.maxItems ?? FAILED_DIAGNOSTICS_CAPACITY;
  const [failures, setFailures] = useState<FailureTelemetryEventRecord[]>(() =>
    listFailedTelemetryEvents(maxItems),
  );

  useEffect(() => {
    setFailures(listFailedTelemetryEvents(maxItems));

    return subscribeTelemetryEvents(() => {
      setFailures(listFailedTelemetryEvents(maxItems));
    });
  }, [maxItems]);

  const filteredFailures = useMemo(() => {
    if (!props.actionPrefixes || props.actionPrefixes.length === 0) {
      return failures;
    }

    return failures.filter((event) =>
      props.actionPrefixes?.some((prefix) => event.action.startsWith(prefix)),
    );
  }, [failures, props.actionPrefixes]);

  const [expandedIds, setExpandedIds] = useState<Record<string, boolean>>({});
  const [copyState, setCopyState] = useState<{ id: string; status: "ok" | "failed" } | null>(null);
  const panelHeadingId = useId();
  const panelDescriptionId = `${panelHeadingId}-description`;

  const toggleDetails = (eventId: string) => {
    setExpandedIds((current) => ({
      ...current,
      [eventId]: !current[eventId],
    }));
  };

  const clearDiagnostics = () => {
    clearTelemetryEvents();
    setExpandedIds({});
    setCopyState(null);
  };

  const onCopyCorrelationId = async (eventId: string, correlationId: string) => {
    try {
      const clipboard = globalThis.navigator?.clipboard;
      if (!clipboard?.writeText) {
        throw new Error("clipboard API unavailable");
      }

      await clipboard.writeText(correlationId);
      setCopyState({ id: eventId, status: "ok" });
    } catch {
      setCopyState({ id: eventId, status: "failed" });
    }
  };

  return (
    <section
      className="panel diagnostics-panel"
      role="region"
      aria-labelledby={panelHeadingId}
      aria-describedby={panelDescriptionId}
    >
      <div className="diagnostics-panel-header">
        <h2 id={panelHeadingId}>{props.title}</h2>
        <button
          type="button"
          onClick={clearDiagnostics}
          disabled={filteredFailures.length === 0}
          aria-label={`Clear diagnostics from ${props.title}`}
        >
          Clear Diagnostics
        </button>
      </div>

      <p id={panelDescriptionId} className="diagnostics-description">
        Failed actions with correlation IDs and sanitized request/response context.
      </p>
      <p className="diagnostics-summary" aria-live="polite">
        {filteredFailures.length === 0
          ? "No failed actions recorded in this session."
          : `${filteredFailures.length} failed action${filteredFailures.length === 1 ? "" : "s"} recorded.`}
      </p>

      {filteredFailures.length === 0 ? (
        <div className="empty-box">No diagnostics failures available.</div>
      ) : (
        <ul className="diagnostics-list">
          {filteredFailures.map((event) => {
            const detailId = `${panelHeadingId}-${event.id}`;
            const expanded = expandedIds[event.id] === true;
            const copied = copyState?.id === event.id ? copyState.status : null;

            return (
              <li className="diagnostics-item" key={event.id}>
                <button
                  type="button"
                  className="diagnostics-toggle"
                  aria-expanded={expanded}
                  aria-controls={detailId}
                  onClick={() => toggleDetails(event.id)}
                  aria-label={`${expanded ? "Hide" : "Show"} diagnostics for ${event.action} failure`}
                >
                  <span className="diagnostics-action">{event.action}</span>
                  <span className="diagnostics-code">{event.error.code}</span>
                  <span className="diagnostics-meta">
                    {event.method} {event.path}
                  </span>
                </button>

                {expanded ? (
                  <FailureDetail
                    detailId={detailId}
                    event={event}
                    copied={copied}
                    onCopyCorrelationId={() => onCopyCorrelationId(event.id, event.correlationId)}
                  />
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function FailureDetail(props: {
  detailId: string;
  event: FailureTelemetryEventRecord;
  copied: "ok" | "failed" | null;
  onCopyCorrelationId: () => void;
}) {
  return (
    <div id={props.detailId} className="diagnostics-detail">
      <dl className="diagnostics-metadata">
        <div className="diagnostics-metadata-row">
          <dt>Timestamp</dt>
          <dd>{new Date(props.event.timestamp).toLocaleString()}</dd>
        </div>
        <div className="diagnostics-metadata-row">
          <dt>Duration</dt>
          <dd>{props.event.durationMs} ms</dd>
        </div>
        <div className="diagnostics-metadata-row">
          <dt>HTTP status</dt>
          <dd>{props.event.httpStatus ?? "n/a"}</dd>
        </div>
        <div className="diagnostics-metadata-row">
          <dt>Correlation ID</dt>
          <dd className="diagnostics-correlation">
            <code>{props.event.correlationId}</code>
            <button
              type="button"
              onClick={props.onCopyCorrelationId}
              aria-label={`Copy correlation ID for ${props.event.action} failure`}
            >
              Copy
            </button>
          </dd>
        </div>
      </dl>

      {props.copied === "ok" ? <p className="diagnostics-copy-state">Correlation ID copied.</p> : null}
      {props.copied === "failed" ? (
        <p className="diagnostics-copy-state">Copy unavailable. Select the value manually.</p>
      ) : null}

      <p className="diagnostics-error-message">{props.event.error.message}</p>

      <div className="grid two diagnostics-payloads">
        <div>
          <h3>Request</h3>
          <pre>{JSON.stringify(props.event.request, null, 2)}</pre>
        </div>
        <div>
          <h3>Response</h3>
          <pre>{JSON.stringify(props.event.response ?? {}, null, 2)}</pre>
        </div>
      </div>
    </div>
  );
}
