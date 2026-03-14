import { ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";

export function statusColor(status: string): "default" | "secondary" | "destructive" | "outline" {
  const s = status.toLowerCase().replace(/[_\s]/g, "-");
  if (["done", "completed", "approved", "implemented"].includes(s)) return "default";
  if (["in-progress", "running", "inprogress"].includes(s)) return "secondary";
  if (["blocked", "failed", "cancelled", "crashed"].includes(s)) return "destructive";
  return "outline";
}

export function priorityColor(p: string): "default" | "secondary" | "destructive" | "outline" {
  const v = (p || "").toLowerCase();
  if (v === "critical") return "destructive";
  if (v === "high") return "secondary";
  return "outline";
}

export function StatusDot({ status }: { status: string }) {
  const s = status.toLowerCase().replace(/[_\s]/g, "-");
  let cls = "ao-status-dot ao-status-dot--idle";
  if (["done", "completed", "approved", "healthy"].includes(s)) cls = "ao-status-dot ao-status-dot--live";
  else if (["in-progress", "running", "inprogress"].includes(s)) cls = "ao-status-dot ao-status-dot--running";
  else if (["blocked", "failed", "cancelled", "crashed", "error"].includes(s)) cls = "ao-status-dot ao-status-dot--error";
  return <span className={cls} />;
}

export function PageLoading() {
  return (
    <div className="space-y-4 ao-fade-in">
      <Skeleton className="h-7 w-44 bg-muted/40" />
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        {[0, 1, 2, 3].map((i) => (
          <Skeleton key={i} className="h-20 bg-muted/30 rounded-lg" />
        ))}
      </div>
      <Skeleton className="h-48 w-full bg-muted/20 rounded-lg" />
    </div>
  );
}

export function PageError({ message }: { message: string }) {
  return (
    <Alert variant="destructive" className="ao-fade-in border-destructive/30 bg-destructive/8">
      <AlertTitle className="text-sm font-medium">Error</AlertTitle>
      <AlertDescription className="text-xs mt-1 font-mono opacity-80">{message}</AlertDescription>
    </Alert>
  );
}

export function StatCard({ label, value, accent }: { label: string; value: number | string; accent?: boolean }) {
  return (
    <Card className={`border-border/40 bg-card/60 backdrop-blur-sm transition-colors hover:border-border/60 ${accent ? "ao-glow-border" : ""}`}>
      <CardContent className="pt-3 pb-3 px-4">
        <p className="text-[11px] text-muted-foreground/70 uppercase tracking-wider font-medium">{label}</p>
        <p className={`text-xl font-semibold font-mono mt-0.5 ${accent ? "text-primary" : "text-foreground/90"}`}>{value}</p>
      </CardContent>
    </Card>
  );
}

export function SectionHeading({ children }: { children: ReactNode }) {
  return <h2 className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">{children}</h2>;
}

export function Markdown({ content, className }: { content: string; className?: string }) {
  return (
    <ReactMarkdown
      className={`max-w-none ${className ?? ""}`}
      components={{
        h1: ({ children }) => <h1 className="text-lg font-semibold mt-4 mb-2">{children}</h1>,
        h2: ({ children }) => <h2 className="text-base font-semibold mt-3 mb-1.5">{children}</h2>,
        h3: ({ children }) => <h3 className="text-sm font-semibold mt-2 mb-1">{children}</h3>,
        p: ({ children }) => <p className="text-sm text-foreground/80 mb-2 leading-relaxed">{children}</p>,
        ul: ({ children }) => <ul className="list-disc list-inside text-sm space-y-0.5 mb-2 text-foreground/80">{children}</ul>,
        ol: ({ children }) => <ol className="list-decimal list-inside text-sm space-y-0.5 mb-2 text-foreground/80">{children}</ol>,
        li: ({ children }) => <li className="text-sm">{children}</li>,
        code: ({ className: codeClassName, children, ...props }) => {
          const isInline = !codeClassName;
          if (isInline) {
            return <code className="px-1 py-0.5 rounded bg-muted text-[12px] font-mono text-foreground/90">{children}</code>;
          }
          return (
            <pre className="overflow-x-auto rounded-md bg-muted/50 border border-border/30 p-3 my-2">
              <code className="text-[11px] font-mono text-foreground/80">{children}</code>
            </pre>
          );
        },
        pre: ({ children }) => <>{children}</>,
        a: ({ href, children }) => (
          <a href={href} className="text-primary hover:underline" target="_blank" rel="noopener noreferrer">{children}</a>
        ),
        blockquote: ({ children }) => (
          <blockquote className="border-l-2 border-primary/30 pl-3 my-2 text-sm text-muted-foreground italic">{children}</blockquote>
        ),
        table: ({ children }) => (
          <div className="overflow-x-auto my-2">
            <table className="text-xs border-collapse w-full">{children}</table>
          </div>
        ),
        th: ({ children }) => <th className="border border-border/30 px-2 py-1 text-left font-medium bg-muted/30">{children}</th>,
        td: ({ children }) => <td className="border border-border/30 px-2 py-1">{children}</td>,
        hr: () => <hr className="border-border/30 my-3" />,
        strong: ({ children }) => <strong className="font-semibold text-foreground">{children}</strong>,
      }}
    >
      {content}
    </ReactMarkdown>
  );
}
