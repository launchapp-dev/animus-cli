import type { TelemetryJsonValue } from "./types";

export const REDACTED_VALUE = "[REDACTED]";
export const NON_JSON_PAYLOAD_PLACEHOLDER = "[NON_JSON_PAYLOAD]";

const SAFE_HEADER_ALLOWLIST = new Set(["accept", "content-type", "x-ao-correlation-id"]);
const SENSITIVE_KEY_FRAGMENTS = [
  "token",
  "secret",
  "password",
  "passphrase",
  "apikey",
  "credential",
  "privatekey",
  "session",
  "cookie",
  "authorization",
];
const MAX_PREVIEW_LENGTH = 256;

export function sanitizeHeadersForTelemetry(headers?: HeadersInit): Record<string, string> {
  const pairs = extractHeaderPairs(headers).sort(([left], [right]) => left.localeCompare(right));
  const redacted: Record<string, string> = {};

  for (const [rawKey, rawValue] of pairs) {
    const normalizedKey = rawKey.toLowerCase();
    if (SAFE_HEADER_ALLOWLIST.has(normalizedKey)) {
      redacted[normalizedKey] = clampPreview(rawValue);
      continue;
    }

    if (isSensitiveKey(normalizedKey)) {
      redacted[normalizedKey] = REDACTED_VALUE;
      continue;
    }

    redacted[normalizedKey] = clampPreview(rawValue);
  }

  return redacted;
}

export function sanitizePathForTelemetry(path: string): {
  path: string;
  query: Record<string, TelemetryJsonValue>;
} {
  const fallback = {
    path: path.split("?")[0],
    query: {} as Record<string, TelemetryJsonValue>,
  };

  try {
    const url = new URL(path, "http://localhost");
    const query: Record<string, TelemetryJsonValue> = {};
    const keys = [...new Set(url.searchParams.keys())].sort();

    for (const key of keys) {
      const values = url.searchParams.getAll(key).map((value) => sanitizeScalarValue(value));
      const queryValue = isSensitiveKey(key) ? REDACTED_VALUE : values.length > 1 ? values : values[0];
      query[key] = queryValue;
    }

    return {
      path: `${url.pathname}${url.hash}`,
      query,
    };
  } catch {
    return fallback;
  }
}

export function sanitizeRequestBodyForTelemetry(
  body: BodyInit | null | undefined,
): TelemetryJsonValue | string | undefined {
  if (body === undefined) {
    return undefined;
  }

  if (body === null) {
    return null;
  }

  if (typeof body === "string") {
    const trimmed = body.trim();
    if (trimmed.length === 0) {
      return "";
    }

    try {
      const parsed = JSON.parse(trimmed) as unknown;
      return sanitizeJsonValueForTelemetry(parsed);
    } catch {
      return NON_JSON_PAYLOAD_PLACEHOLDER;
    }
  }

  if (body instanceof URLSearchParams) {
    const query: Record<string, TelemetryJsonValue> = {};
    for (const [key, value] of [...body.entries()].sort(([left], [right]) => left.localeCompare(right))) {
      query[key] = isSensitiveKey(key) ? REDACTED_VALUE : sanitizeScalarValue(value);
    }
    return query;
  }

  return NON_JSON_PAYLOAD_PLACEHOLDER;
}

export function sanitizeResponseBodyForTelemetry(
  value: unknown,
): TelemetryJsonValue | string | undefined {
  if (value === undefined) {
    return undefined;
  }

  return sanitizeJsonValueForTelemetry(value);
}

export function sanitizeJsonValueForTelemetry(value: unknown): TelemetryJsonValue | string {
  if (value === null || typeof value === "boolean" || typeof value === "number") {
    return value;
  }

  if (typeof value === "string") {
    return sanitizeScalarValue(value);
  }

  if (Array.isArray(value)) {
    return value.map((entry) => sanitizeJsonValueForTelemetry(entry));
  }

  if (!isRecord(value)) {
    return NON_JSON_PAYLOAD_PLACEHOLDER;
  }

  const sanitized: Record<string, TelemetryJsonValue | string> = {};
  for (const key of Object.keys(value).sort()) {
    const fieldValue = value[key];
    if (isSensitiveKey(key)) {
      sanitized[key] = REDACTED_VALUE;
      continue;
    }

    sanitized[key] = sanitizeJsonValueForTelemetry(fieldValue);
  }

  return sanitized;
}

function extractHeaderPairs(headers?: HeadersInit): Array<[string, string]> {
  if (!headers) {
    return [];
  }

  if (headers instanceof Headers) {
    return [...headers.entries()];
  }

  if (Array.isArray(headers)) {
    return headers.map(([key, value]) => [key, String(value)]);
  }

  return Object.entries(headers).map(([key, value]) => [key, String(value)]);
}

function isSensitiveKey(key: string): boolean {
  const normalized = key.toLowerCase().replace(/[^a-z0-9]/g, "");
  return SENSITIVE_KEY_FRAGMENTS.some((fragment) => normalized.includes(fragment));
}

function sanitizeScalarValue(value: string): string {
  return clampPreview(value);
}

function clampPreview(value: string): string {
  if (value.length <= MAX_PREVIEW_LENGTH) {
    return value;
  }

  const omitted = value.length - MAX_PREVIEW_LENGTH;
  return `${value.slice(0, MAX_PREVIEW_LENGTH)}...[${omitted} chars truncated]`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
