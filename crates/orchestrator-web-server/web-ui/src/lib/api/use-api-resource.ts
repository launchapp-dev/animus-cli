import { DependencyList, useEffect, useState } from "react";

import { ApiError, ApiResult } from "./envelope";

export type ResourceState<TData> =
  | {
      status: "loading";
      data: null;
      error: null;
    }
  | {
      status: "ready";
      data: TData;
      error: null;
    }
  | {
      status: "empty";
      data: TData;
      error: null;
    }
  | {
      status: "error";
      data: null;
      error: ApiError;
    };

export function useApiResource<TData>(
  request: () => Promise<ApiResult<TData>>,
  dependencies: DependencyList,
  options: {
    isEmpty?: (data: TData) => boolean;
  } = {},
): ResourceState<TData> {
  const [state, setState] = useState<ResourceState<TData>>({
    status: "loading",
    data: null,
    error: null,
  });

  useEffect(() => {
    let isCancelled = false;

    setState({ status: "loading", data: null, error: null });

    void request().then((result) => {
      if (isCancelled) {
        return;
      }

      if (result.kind === "error") {
        setState({
          status: "error",
          data: null,
          error: result,
        });
        return;
      }

      if (options.isEmpty?.(result.data)) {
        setState({
          status: "empty",
          data: result.data,
          error: null,
        });
        return;
      }

      setState({
        status: "ready",
        data: result.data,
        error: null,
      });
    });

    return () => {
      isCancelled = true;
    };
  }, dependencies);

  return state;
}
