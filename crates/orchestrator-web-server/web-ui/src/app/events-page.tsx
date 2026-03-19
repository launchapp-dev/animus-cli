import { useMemo, useState } from "react";
import { useSubscription } from "@/lib/graphql/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DaemonEventsDocument } from "@/lib/graphql/generated/graphql";
import { StatusDot, StatCard } from "./shared";

type EventRecord = {
  id: string;
  seq: number;
  timestamp: string;
  eventType: string;
  data: string;
};

const MAX_EVENTS = 200;

export function EventsPage() {
  const [paused, setPaused] = useState(false);
  const [events, setEvents] = useState<EventRecord[]>([]);

  const [subscriptionResult] = useSubscription(
    { query: DaemonEventsDocument, pause: paused },
    (_prev, response) => {
      if (response.daemonEvents) {
        setEvents((current) => {
          const next = [...current, response.daemonEvents];
          return next.length > MAX_EVENTS ? next.slice(next.length - MAX_EVENTS) : next;
        });
      }
      return response;
    },
  );

  const connectionState = subscriptionResult.fetching
    ? "live"
    : subscriptionResult.error
      ? "error"
      : paused
        ? "paused"
        : "connecting";

  const mostRecent = useMemo(() => [...events].reverse().slice(0, 50), [events]);

  const typeCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const evt of events) {
      counts[evt.eventType] = (counts[evt.eventType] ?? 0) + 1;
    }
    return counts;
  }, [events]);

  const topTypes = useMemo(() =>
    Object.entries(typeCounts).sort((a, b) => b[1] - a[1]).slice(0, 4),
  [typeCounts]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Events</h1>
          <StatusDot status={connectionState === "live" ? "running" : connectionState === "error" ? "error" : "idle"} />
          <span className="text-[11px] text-muted-foreground/50">{connectionState}</span>
        </div>
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            variant={paused ? "default" : "outline"}
            onClick={() => setPaused(!paused)}
          >
            {paused ? "Resume" : "Pause"}
          </Button>
          {events.length > 0 && (
            <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setEvents([])}>Clear</Button>
          )}
        </div>
      </div>

      {connectionState === "error" && (
        <div className="rounded-lg border border-destructive/40 bg-destructive/5 px-4 py-3 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-2">
            <Badge variant="destructive" className="shrink-0">Connection Error</Badge>
            <span className="text-sm text-muted-foreground">
              Could not connect to the event stream. Ensure the daemon is running and reload the page.
            </span>
          </div>
          <Button
            size="sm"
            variant="outline"
            className="shrink-0 self-start sm:self-auto"
            onClick={() => { setPaused(true); setTimeout(() => setPaused(false), 0); }}
          >
            Retry
          </Button>
        </div>
      )}

      {topTypes.length > 0 && (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          {topTypes.map(([type, count]) => (
            <StatCard key={type} label={type} value={count} />
          ))}
        </div>
      )}

      {mostRecent.length === 0 ? (
        <p className="text-sm text-muted-foreground py-8 text-center">No events received yet.</p>
      ) : (
        <Card className="border-border/40 bg-card/60 overflow-hidden">
          <CardContent className="p-0">
            <div className="max-h-[600px] overflow-y-auto">
              {mostRecent.map((evt, i) => (
                <div key={evt.id ?? i} className="border-b border-border/20 last:border-0 px-4 py-2 hover:bg-accent/20 transition-colors">
                  <div className="flex items-center gap-2">
                    <Badge variant="outline" className="text-[10px] shrink-0 font-mono">{evt.eventType ?? "event"}</Badge>
                    <span className="text-[10px] text-muted-foreground/40 font-mono ml-auto shrink-0">{evt.timestamp ?? ""}</span>
                  </div>
                  <pre className="text-[11px] mt-1 overflow-x-auto text-foreground/60">{(() => {
                    try { return JSON.stringify(JSON.parse(evt.data), null, 2); } catch { return evt.data; }
                  })()}</pre>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
