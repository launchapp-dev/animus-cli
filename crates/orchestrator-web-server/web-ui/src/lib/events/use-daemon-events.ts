import { useEffect, useRef, useState } from "react";

export type DaemonConnectionState =
  | "connecting"
  | "live"
  | "reconnecting"
  | "disconnected";

export type DaemonEventRecord = {
  id?: string;
  seq?: number;
  timestamp?: string;
  event_type?: string;
  [key: string]: unknown;
};

const MAX_EVENTS = 200;

export function useDaemonEvents() {
  const [connectionState, setConnectionState] = useState<DaemonConnectionState>("connecting");
  const [events, setEvents] = useState<DaemonEventRecord[]>([]);
  const lastEventId = useRef<string | null>(null);

  useEffect(() => {
    let isCancelled = false;
    let retryBackoffMs = 1_000;
    let retryTimer: number | null = null;
    let activeAbortController: AbortController | null = null;

    const scheduleReconnect = () => {
      if (isCancelled) {
        return;
      }

      setConnectionState("reconnecting");
      retryTimer = window.setTimeout(() => {
        void connect();
      }, retryBackoffMs);

      retryBackoffMs = Math.min(retryBackoffMs * 2, 10_000);
    };

    const connect = async () => {
      activeAbortController = new AbortController();

      setConnectionState(lastEventId.current ? "reconnecting" : "connecting");

      try {
        const headers: Record<string, string> = {
          Accept: "text/event-stream",
        };

        if (lastEventId.current) {
          headers["Last-Event-ID"] = lastEventId.current;
        }

        const response = await fetch("/api/v1/events", {
          method: "GET",
          headers,
          signal: activeAbortController.signal,
        });

        if (!response.ok || !response.body) {
          throw new Error(`SSE request failed with status ${response.status}`);
        }

        setConnectionState("live");
        retryBackoffMs = 1_000;

        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();

        let buffer = "";
        let eventName = "";
        let eventId = "";
        let dataLines: string[] = [];

        const flushEvent = () => {
          if (dataLines.length === 0) {
            eventName = "";
            eventId = "";
            return;
          }

          if (eventName && eventName !== "daemon-event") {
            eventName = "";
            eventId = "";
            dataLines = [];
            return;
          }

          if (eventId) {
            lastEventId.current = eventId;
          }

          const jsonPayload = dataLines.join("\n");

          try {
            const parsed = JSON.parse(jsonPayload) as DaemonEventRecord;
            if (eventId && typeof parsed.id !== "string") {
              parsed.id = eventId;
            }

            setEvents((current) => {
              const next = [...current, parsed];
              return next.length > MAX_EVENTS ? next.slice(next.length - MAX_EVENTS) : next;
            });
          } catch {
            // Ignore malformed payload lines and keep consuming stream.
          }

          eventName = "";
          eventId = "";
          dataLines = [];
        };

        while (!isCancelled) {
          const { value, done } = await reader.read();
          if (done) {
            break;
          }

          buffer += value;

          while (true) {
            const newLineIndex = buffer.indexOf("\n");
            if (newLineIndex < 0) {
              break;
            }

            let line = buffer.slice(0, newLineIndex);
            buffer = buffer.slice(newLineIndex + 1);

            if (line.endsWith("\r")) {
              line = line.slice(0, -1);
            }

            if (line === "") {
              flushEvent();
              continue;
            }

            if (line.startsWith(":")) {
              continue;
            }

            const separator = line.indexOf(":");
            const field = separator >= 0 ? line.slice(0, separator) : line;
            let fieldValue = separator >= 0 ? line.slice(separator + 1) : "";

            if (fieldValue.startsWith(" ")) {
              fieldValue = fieldValue.slice(1);
            }

            switch (field) {
              case "event": {
                eventName = fieldValue;
                break;
              }
              case "id": {
                eventId = fieldValue;
                break;
              }
              case "data": {
                dataLines.push(fieldValue);
                break;
              }
              default:
                break;
            }
          }
        }
      } catch {
        if (isCancelled || activeAbortController.signal.aborted) {
          return;
        }
      }

      scheduleReconnect();
    };

    void connect();

    return () => {
      isCancelled = true;
      setConnectionState("disconnected");

      if (retryTimer !== null) {
        window.clearTimeout(retryTimer);
      }

      activeAbortController?.abort();
    };
  }, []);

  return {
    connectionState,
    events,
  };
}
