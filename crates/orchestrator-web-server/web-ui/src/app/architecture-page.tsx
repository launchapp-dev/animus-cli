import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { SectionHeading } from "./shared";

const CLI_COMMANDS = [
  { command: "ao architecture entities list", description: "List architecture entities and their types" },
  { command: "ao architecture edges list", description: "List entity relationships and dependencies" },
  { command: "ao architecture decisions list", description: "List architecture decision records (ADRs)" },
  { command: "ao architecture decisions create", description: "Record a new architecture decision" },
];

const ENTITY_PLACEHOLDERS = [
  { name: "Services", count: null },
  { name: "Data Stores", count: null },
  { name: "APIs", count: null },
  { name: "Libraries", count: null },
];

export function ArchitecturePage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Architecture</h1>
        <p className="text-sm text-muted-foreground/60 mt-1">
          Architecture decisions and entity relationships
        </p>
      </div>

      <Alert className="border-primary/20 bg-primary/5">
        <AlertDescription className="text-sm text-foreground/80">
          Architecture data is managed via <code className="px-1 py-0.5 rounded bg-muted text-[12px] font-mono">ao architecture</code> CLI commands.
          The web viewer will display entities, edges, and decision records once the GraphQL integration is available.
        </AlertDescription>
      </Alert>

      <Card className="border-border/40 bg-card/60">
        <CardContent className="pt-4 pb-4 px-5">
          <p className="text-[11px] text-muted-foreground/70 uppercase tracking-wider font-medium mb-3">Available Commands</p>
          <div className="space-y-2">
            {CLI_COMMANDS.map((cmd) => (
              <div key={cmd.command} className="flex items-start gap-3 py-1.5 border-b border-border/20 last:border-0">
                <code className="px-2 py-1 rounded bg-muted/50 text-[11px] font-mono text-foreground/90 shrink-0">
                  {cmd.command}
                </code>
                <span className="text-sm text-muted-foreground/60 pt-0.5">{cmd.description}</span>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>

      <div className="space-y-3">
        <SectionHeading>Entities</SectionHeading>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          {ENTITY_PLACEHOLDERS.map((entity) => (
            <Card key={entity.name} className="border-border/40 bg-card/60 border-dashed">
              <CardContent className="pt-3 pb-3 px-4 flex flex-col items-center justify-center min-h-[80px]">
                <p className="text-sm font-medium text-muted-foreground/40">{entity.name}</p>
                <p className="text-[10px] text-muted-foreground/30 mt-1">coming soon</p>
              </CardContent>
            </Card>
          ))}
        </div>
        <p className="text-[11px] text-muted-foreground/40 font-mono">
          Run <code className="px-1 py-0.5 rounded bg-muted/30 text-[11px]">ao architecture entities list</code> to view entities in the CLI
        </p>
      </div>

      <div className="space-y-3">
        <SectionHeading>Decisions</SectionHeading>
        <Card className="border-border/40 bg-card/60 border-dashed">
          <CardContent className="pt-6 pb-6 px-4">
            <div className="flex flex-col items-center justify-center text-center">
              <Badge variant="outline" className="text-[10px] font-mono border-border/30 text-muted-foreground/40 mb-2">ADR</Badge>
              <p className="text-sm text-muted-foreground/40">Architecture Decision Records will appear here</p>
              <p className="text-[10px] text-muted-foreground/30 mt-1">coming soon</p>
            </div>
          </CardContent>
        </Card>
        <p className="text-[11px] text-muted-foreground/40 font-mono">
          Run <code className="px-1 py-0.5 rounded bg-muted/30 text-[11px]">ao architecture decisions list</code> to view decisions in the CLI
        </p>
      </div>
    </div>
  );
}
