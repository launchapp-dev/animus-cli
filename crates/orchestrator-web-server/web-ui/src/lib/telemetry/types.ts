export type TelemetryEventType = "request_start" | "request_success" | "request_failure";

export type TelemetryJsonValue =
  | null
  | boolean
  | number
  | string
  | TelemetryJsonValue[]
  | { [key: string]: TelemetryJsonValue };

export type TelemetryErrorSummary = {
  code: string;
  message: string;
  exitCode: number;
};

export type TelemetryRequestSummary = {
  headers: Record<string, string>;
  query: Record<string, TelemetryJsonValue>;
  body?: TelemetryJsonValue | string;
};

export type TelemetryResponseSummary = {
  headers: Record<string, string>;
  body?: TelemetryJsonValue | string;
};

type TelemetryEventBase = {
  eventType: TelemetryEventType;
  timestamp: string;
  correlationId: string;
  method: string;
  path: string;
  action: string;
  request: TelemetryRequestSummary;
};

export type RequestStartTelemetryEvent = TelemetryEventBase & {
  eventType: "request_start";
};

export type RequestSuccessTelemetryEvent = TelemetryEventBase & {
  eventType: "request_success";
  durationMs: number;
  httpStatus?: number;
  response?: TelemetryResponseSummary;
};

export type RequestFailureTelemetryEvent = TelemetryEventBase & {
  eventType: "request_failure";
  durationMs: number;
  httpStatus?: number;
  error: TelemetryErrorSummary;
  response?: TelemetryResponseSummary;
};

export type TelemetryEvent =
  | RequestStartTelemetryEvent
  | RequestSuccessTelemetryEvent
  | RequestFailureTelemetryEvent;

export type TelemetryEventRecord = TelemetryEvent & {
  id: string;
};

export type FailureTelemetryEventRecord = Extract<
  TelemetryEventRecord,
  { eventType: "request_failure" }
>;
