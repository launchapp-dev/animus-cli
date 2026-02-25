import { describe, expect, it } from "vitest";

import {
  NON_JSON_PAYLOAD_PLACEHOLDER,
  REDACTED_VALUE,
  sanitizeHeadersForTelemetry,
  sanitizeJsonValueForTelemetry,
  sanitizePathForTelemetry,
  sanitizeRequestBodyForTelemetry,
} from "./redaction";

describe("telemetry redaction", () => {
  it("redacts sensitive headers and preserves safe allowlist values", () => {
    const result = sanitizeHeadersForTelemetry({
      Accept: "application/json",
      Authorization: "Bearer super-secret",
      Cookie: "session=abc",
      "X-AO-Correlation-ID": "cid-1234",
      "X-Trace-Name": "trace-value",
    });

    expect(result).toEqual({
      accept: "application/json",
      authorization: REDACTED_VALUE,
      cookie: REDACTED_VALUE,
      "x-ao-correlation-id": "cid-1234",
      "x-trace-name": "trace-value",
    });
  });

  it("redacts nested payload keys and preserves structure", () => {
    const result = sanitizeJsonValueForTelemetry({
      context: {
        token: "123",
        nested: {
          password: "abc",
          retry: 2,
        },
      },
      items: [{ api_key: "key" }, { name: "safe" }],
    });

    expect(result).toEqual({
      context: {
        nested: {
          password: REDACTED_VALUE,
          retry: 2,
        },
        token: REDACTED_VALUE,
      },
      items: [{ api_key: REDACTED_VALUE }, { name: "safe" }],
    });
  });

  it("redacts query values for sensitive keys", () => {
    const result = sanitizePathForTelemetry(
      "/api/v1/reviews/handoff?token=abc123&mode=sync&session=some-id",
    );

    expect(result.path).toBe("/api/v1/reviews/handoff");
    expect(result.query).toEqual({
      mode: "sync",
      session: REDACTED_VALUE,
      token: REDACTED_VALUE,
    });
  });

  it("uses non-json placeholder for opaque request payloads", () => {
    const result = sanitizeRequestBodyForTelemetry("this is not json");
    expect(result).toBe(NON_JSON_PAYLOAD_PLACEHOLDER);
  });
});
