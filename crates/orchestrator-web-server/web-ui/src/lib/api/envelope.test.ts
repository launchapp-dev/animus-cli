import { describe, expect, it } from "vitest";

import { parseAoEnvelope } from "./envelope";

describe("parseAoEnvelope", () => {
  it("returns ok variant for ao success envelope", () => {
    const result = parseAoEnvelope<{ id: string }>({
      schema: "ao.cli.v1",
      ok: true,
      data: { id: "TASK-011" },
    });

    expect(result).toEqual({
      kind: "ok",
      data: { id: "TASK-011" },
    });
  });

  it("maps error envelope fields into normalized api error", () => {
    const result = parseAoEnvelope({
      schema: "ao.cli.v1",
      ok: false,
      error: {
        code: "not_found",
        message: "task not found",
        exit_code: 3,
      },
    });

    expect(result).toEqual({
      kind: "error",
      code: "not_found",
      message: "task not found",
      exitCode: 3,
    });
  });

  it("rejects payloads without envelope schema", () => {
    const result = parseAoEnvelope({ ok: true, data: {} });

    expect(result).toEqual({
      kind: "error",
      code: "invalid_envelope",
      message: "Expected ao.cli.v1 envelope response.",
      exitCode: 1,
    });
  });
});
