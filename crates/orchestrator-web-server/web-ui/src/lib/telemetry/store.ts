import type {
  FailureTelemetryEventRecord,
  TelemetryEvent,
  TelemetryEventRecord,
} from "./types";

export const TELEMETRY_EVENT_CAPACITY = 200;
export const FAILED_DIAGNOSTICS_CAPACITY = 25;

type TelemetryListener = () => void;

let eventSequence = 0;
let events: TelemetryEventRecord[] = [];
const listeners = new Set<TelemetryListener>();

export function recordTelemetryEvent(event: TelemetryEvent): TelemetryEventRecord {
  eventSequence += 1;

  const record: TelemetryEventRecord = {
    ...event,
    id: `telem-${eventSequence}`,
  };

  events = [...events, record];
  if (events.length > TELEMETRY_EVENT_CAPACITY) {
    events = events.slice(events.length - TELEMETRY_EVENT_CAPACITY);
  }

  notifyListeners();
  return record;
}

export function listTelemetryEvents(): TelemetryEventRecord[] {
  return [...events];
}

export function listFailedTelemetryEvents(
  limit = FAILED_DIAGNOSTICS_CAPACITY,
): FailureTelemetryEventRecord[] {
  const failures = events.filter(isFailureEvent);
  const bounded = failures.slice(Math.max(0, failures.length - limit));
  return [...bounded].reverse();
}

export function clearTelemetryEvents(): void {
  events = [];
  notifyListeners();
}

export function subscribeTelemetryEvents(listener: TelemetryListener): () => void {
  listeners.add(listener);

  return () => {
    listeners.delete(listener);
  };
}

export function resetTelemetryStoreForTests(): void {
  eventSequence = 0;
  events = [];
  listeners.clear();
}

function notifyListeners(): void {
  for (const listener of listeners) {
    listener();
  }
}

function isFailureEvent(event: TelemetryEventRecord): event is FailureTelemetryEventRecord {
  return event.eventType === "request_failure";
}
