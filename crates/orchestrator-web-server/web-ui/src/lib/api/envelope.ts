export type ApiOk<TData> = {
  kind: "ok";
  data: TData;
};

export type ApiError = {
  kind: "error";
  code: string;
  message: string;
  exitCode: number;
  correlationId?: string;
  httpStatus?: number;
  requestPath?: string;
  method?: string;
};

export type ApiResult<TData> = ApiOk<TData> | ApiError;

export function parseAoEnvelope<TData>(payload: unknown): ApiResult<TData> {
  if (!isRecord(payload) || payload["schema"] !== "ao.cli.v1") {
    return {
      kind: "error",
      code: "invalid_envelope",
      message: "Expected ao.cli.v1 envelope response.",
      exitCode: 1,
    };
  }

  if (payload["ok"] === true) {
    if (!("data" in payload)) {
      return {
        kind: "error",
        code: "invalid_envelope",
        message: "Envelope with ok=true is missing data.",
        exitCode: 1,
      };
    }

    return {
      kind: "ok",
      data: payload["data"] as TData,
    };
  }

  if (payload["ok"] === false) {
    const errorObject = isRecord(payload["error"]) ? payload["error"] : {};

    return {
      kind: "error",
      code: typeof errorObject["code"] === "string" ? errorObject["code"] : "unknown_error",
      message:
        typeof errorObject["message"] === "string"
          ? errorObject["message"]
          : "Unknown error envelope.",
      exitCode: typeof errorObject["exit_code"] === "number" ? errorObject["exit_code"] : 1,
    };
  }

  return {
    kind: "error",
    code: "invalid_envelope",
    message: "Envelope ok flag must be boolean.",
    exitCode: 1,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
