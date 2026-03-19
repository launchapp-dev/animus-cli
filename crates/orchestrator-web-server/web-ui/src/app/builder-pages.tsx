import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { WorkflowDefinitionsDocument, WorkflowConfigDocument } from "@/lib/graphql/generated/graphql";
import { PageLoading, PageError } from "./shared";
import {
  Plus,
  ChevronRight,
  ChevronLeft,
  ArrowLeft,
  X,
  Pencil,
  Copy,
  Trash2,
  Eye,
  CheckCircle2,
  AlertCircle,
  Save,
  Layers,
  PaintBucket,
  FileText,
  Settings,
  Server,
  Clock,
  Users,
  Terminal,
  Hand,
  Bot,
  ChevronDown,
  ChevronUp,
} from "lucide-react";

interface PhaseEntry {
  id: string;
  maxReworkAttempts: number;
  skipIf: string[];
  onVerdict: {
    advance: { target: string | null };
    rework: { target: string | null; allowAgentTarget: boolean };
    fail: { target: string | null };
  };
}

interface VariableEntry {
  name: string;
  description: string;
  required: boolean;
  default: string;
}

interface PostSuccessConfig {
  mergeStrategy: string;
  targetBranch: string;
  createPr: boolean;
  autoMerge: boolean;
  cleanupWorktree: boolean;
}

interface WorkflowDef {
  id: string;
  name: string;
  description: string;
  phases: PhaseEntry[];
  variables: VariableEntry[];
  postSuccess: PostSuccessConfig;
}

interface AgentProfileEntry {
  name: string;
  description: string;
  role: string;
  systemPrompt: string;
  model: string;
  tool: string;
  mcpServers: string[];
  skills: string[];
}

interface ContractField {
  name: string;
  type: "string" | "array" | "integer" | "boolean";
  description: string;
  itemsType: string;
  enumValues: string[];
}

interface PhaseDefinitionEntry {
  id: string;
  mode: "agent" | "command" | "manual";
  agent: string;
  directive: string;
  decisionContract: {
    minConfidence: number;
    maxRisk: string;
    allowMissingDecision: boolean;
    requiredEvidence: string[];
    fields: ContractField[];
  };
  outputContract: {
    kind: string;
    requiredFields: string[];
    fields: ContractField[];
  };
  command: {
    program: string;
    args: string[];
    cwdMode: string;
    timeoutSecs: number;
  };
  manualInstructions: string;
  approvalNoteRequired: boolean;
}

interface McpServerEntry {
  name: string;
  command: string;
  args: string[];
  transport: string;
  tools: string[];
  env: Array<{ key: string; value: string }>;
}

interface ScheduleEntry {
  id: string;
  cron: string;
  workflowRef: string;
  enabled: boolean;
}

function makePhaseEntry(id: string): PhaseEntry {
  return {
    id,
    maxReworkAttempts: 3,
    skipIf: [],
    onVerdict: {
      advance: { target: null },
      rework: { target: null, allowAgentTarget: false },
      fail: { target: null },
    },
  };
}

function makeVariableEntry(): VariableEntry {
  return { name: "", description: "", required: false, default: "" };
}

function makePostSuccess(): PostSuccessConfig {
  return { mergeStrategy: "squash", targetBranch: "main", createPr: true, autoMerge: false, cleanupWorktree: true };
}

function makeAgentProfile(): AgentProfileEntry {
  return { name: "", description: "", role: "", systemPrompt: "", model: "", tool: "", mcpServers: [], skills: [] };
}

function makePhaseDefinition(id?: string): PhaseDefinitionEntry {
  return {
    id: id ?? "",
    mode: "agent",
    agent: "",
    directive: "",
    decisionContract: { minConfidence: 0.7, maxRisk: "medium", allowMissingDecision: false, requiredEvidence: [], fields: [] },
    outputContract: { kind: "", requiredFields: [], fields: [] },
    command: { program: "", args: [], cwdMode: "project_root", timeoutSecs: 300 },
    manualInstructions: "",
    approvalNoteRequired: false,
  };
}

function makeMcpServer(): McpServerEntry {
  return { name: "", command: "", args: [], transport: "stdio", tools: [], env: [] };
}

function makeSchedule(): ScheduleEntry {
  return { id: "", cron: "", workflowRef: "", enabled: true };
}

const TEMPLATES: Record<string, { name: string; description: string; phases: string[] }> = {
  standard: {
    name: "Standard",
    description: "A typical development workflow with requirements analysis, implementation, code review, and testing phases.",
    phases: ["requirements", "implementation", "code-review", "testing"],
  },
  "ui-ux": {
    name: "UI/UX",
    description: "Extended workflow for user interface work including research, wireframing, and mockup review before implementation.",
    phases: ["requirements", "ux-research", "wireframe", "mockup-review", "implementation", "code-review", "testing"],
  },
  blank: {
    name: "Blank",
    description: "Start from scratch with an empty workflow definition. Add phases as needed.",
    phases: [],
  },
};

const ID_PATTERN = /^[a-z0-9][a-z0-9-]*$/;

interface ValidationResult {
  valid: boolean;
  errors: { message: string; phaseId?: string }[];
  warnings: { message: string }[];
}

function validateDef(def: WorkflowDef): ValidationResult {
  const errors: { message: string; phaseId?: string }[] = [];
  const warnings: { message: string }[] = [];

  if (!def.id.trim()) {
    errors.push({ message: "Workflow ID is required" });
  } else if (!ID_PATTERN.test(def.id)) {
    errors.push({ message: "ID must start with a lowercase letter or digit and contain only lowercase letters, digits, and hyphens" });
  }

  if (!def.name.trim()) {
    errors.push({ message: "Workflow name is required" });
  }

  if (def.phases.length === 0) {
    errors.push({ message: "At least one phase is required" });
  }

  const seen = new Set<string>();
  const phaseIds = new Set(def.phases.map((p) => p.id));
  for (const phase of def.phases) {
    if (!phase.id.trim()) {
      errors.push({ message: "Phase ID cannot be empty", phaseId: phase.id });
    } else if (!ID_PATTERN.test(phase.id)) {
      errors.push({ message: `Phase "${phase.id}" has an invalid ID format`, phaseId: phase.id });
    }
    if (seen.has(phase.id)) {
      errors.push({ message: `Duplicate phase ID "${phase.id}"`, phaseId: phase.id });
    }
    seen.add(phase.id);

    if (phase.maxReworkAttempts < 1) {
      errors.push({ message: `Phase "${phase.id}" must have max rework attempts > 0`, phaseId: phase.id });
    }

    for (const [verdict, cfg] of Object.entries(phase.onVerdict)) {
      const target = (cfg as { target: string | null }).target;
      if (target && !phaseIds.has(target)) {
        errors.push({ message: `Phase "${phase.id}" ${verdict} target "${target}" does not exist`, phaseId: phase.id });
      }
    }
  }

  if (def.phases.length > 0 && !errors.some((e) => e.phaseId)) {
    warnings.push({ message: `${def.phases.length} phase(s) configured` });
  }

  return { valid: errors.length === 0, errors, warnings };
}

function defToPreview(def: WorkflowDef): string {
  const obj: Record<string, unknown> = {
    id: def.id,
    name: def.name,
  };
  if (def.description) obj.description = def.description;
  obj.phases = def.phases.map((p) => {
    const phase: Record<string, unknown> = { id: p.id };
    if (p.maxReworkAttempts !== 3) phase.max_rework_attempts = p.maxReworkAttempts;
    if (p.skipIf.length > 0) phase.skip_if = p.skipIf;
    const onVerdict: Record<string, unknown> = {};
    if (p.onVerdict.advance.target) onVerdict.advance = { target: p.onVerdict.advance.target };
    if (p.onVerdict.rework.target || p.onVerdict.rework.allowAgentTarget) {
      const rw: Record<string, unknown> = {};
      if (p.onVerdict.rework.target) rw.target = p.onVerdict.rework.target;
      if (p.onVerdict.rework.allowAgentTarget) rw.allow_agent_target = true;
      onVerdict.rework = rw;
    }
    if (p.onVerdict.fail.target) onVerdict.fail = { target: p.onVerdict.fail.target };
    if (Object.keys(onVerdict).length > 0) phase.on_verdict = onVerdict;
    return phase;
  });
  if (def.variables.length > 0) {
    obj.variables = def.variables.map((v) => {
      const ve: Record<string, unknown> = { name: v.name };
      if (v.description) ve.description = v.description;
      if (v.required) ve.required = true;
      if (v.default) ve.default = v.default;
      return ve;
    });
  }
  return JSON.stringify(obj, null, 2);
}

function PhaseNode({
  phase,
  index,
  total,
  selected,
  hasError,
  onSelect,
  onMoveLeft,
  onMoveRight,
  onRemove,
}: {
  phase: PhaseEntry;
  index: number;
  total: number;
  selected: boolean;
  hasError: boolean;
  onSelect: () => void;
  onMoveLeft: () => void;
  onMoveRight: () => void;
  onRemove: () => void;
}) {
  return (
    <div className="flex items-center gap-0">
      <button
        type="button"
        onClick={onSelect}
        className={`group relative flex items-center gap-2 rounded-lg border px-3 py-2 transition-colors ${
          selected
            ? "border-primary/40 bg-primary/5"
            : hasError
              ? "border-destructive/40 bg-destructive/5"
              : "border-border/40 bg-card/60 hover:border-border/60"
        }`}
      >
        <span
          className={`inline-block h-2.5 w-2.5 rounded-full shrink-0 ${hasError ? "bg-destructive" : "bg-primary/60"}`}
        />
        <span className="font-mono text-xs">{phase.id}</span>
        <div className="absolute -top-2 -right-1 hidden group-hover:flex items-center gap-0.5">
          {index > 0 && (
            <button
              type="button"
              onClick={(e) => { e.stopPropagation(); onMoveLeft(); }}
              className="h-4 w-4 rounded bg-muted/80 flex items-center justify-center hover:bg-muted"
            >
              <ChevronLeft className="h-3 w-3" />
            </button>
          )}
          {index < total - 1 && (
            <button
              type="button"
              onClick={(e) => { e.stopPropagation(); onMoveRight(); }}
              className="h-4 w-4 rounded bg-muted/80 flex items-center justify-center hover:bg-muted"
            >
              <ChevronRight className="h-3 w-3" />
            </button>
          )}
          <button
            type="button"
            onClick={(e) => { e.stopPropagation(); onRemove(); }}
            className="h-4 w-4 rounded bg-destructive/20 flex items-center justify-center hover:bg-destructive/40"
          >
            <X className="h-3 w-3 text-destructive" />
          </button>
        </div>
      </button>
      {index < total - 1 && <ChevronRight className="h-4 w-4 text-muted-foreground/40 mx-1 shrink-0" />}
    </div>
  );
}

function PhaseDetailPanel({
  phase,
  allPhaseIds,
  onChange,
}: {
  phase: PhaseEntry;
  allPhaseIds: string[];
  onChange: (updated: PhaseEntry) => void;
}) {
  const otherPhases = allPhaseIds.filter((id) => id !== phase.id);
  const [newSkipGuard, setNewSkipGuard] = useState("");

  const updateField = <K extends keyof PhaseEntry>(key: K, value: PhaseEntry[K]) => {
    onChange({ ...phase, [key]: value });
  };

  const updateVerdict = (
    verdict: "advance" | "rework" | "fail",
    field: string,
    value: unknown,
  ) => {
    onChange({
      ...phase,
      onVerdict: {
        ...phase.onVerdict,
        [verdict]: { ...phase.onVerdict[verdict], [field]: value },
      },
    });
  };

  return (
    <div className="w-full md:w-72 shrink-0 space-y-4 ao-fade-in">
      <Card className="border-border/40 bg-card/60">
        <CardHeader className="pb-2 pt-3 px-4">
          <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Phase Config</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4 space-y-4">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Phase ID</label>
            <Input
              value={phase.id}
              onChange={(e) => updateField("id", e.target.value)}
              className="mt-1 font-mono text-xs h-8"
            />
          </div>

          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Max Rework Attempts</label>
            <Input
              type="number"
              min={1}
              value={phase.maxReworkAttempts}
              onChange={(e) => updateField("maxReworkAttempts", Math.max(1, parseInt(e.target.value) || 1))}
              className="mt-1 text-xs h-8"
            />
          </div>

          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Skip If Guards</label>
            <div className="mt-1 space-y-1">
              {phase.skipIf.map((guard, i) => (
                <div key={i} className="flex items-center gap-1">
                  <span className="text-xs font-mono flex-1 truncate">{guard}</span>
                  <button
                    type="button"
                    onClick={() => updateField("skipIf", phase.skipIf.filter((_, j) => j !== i))}
                    className="text-muted-foreground hover:text-destructive"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </div>
              ))}
              <div className="flex gap-1">
                <Input
                  value={newSkipGuard}
                  onChange={(e) => setNewSkipGuard(e.target.value)}
                  placeholder="Guard condition"
                  className="text-xs h-7 flex-1"
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && newSkipGuard.trim()) {
                      updateField("skipIf", [...phase.skipIf, newSkipGuard.trim()]);
                      setNewSkipGuard("");
                    }
                  }}
                />
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 px-2"
                  disabled={!newSkipGuard.trim()}
                  onClick={() => {
                    if (newSkipGuard.trim()) {
                      updateField("skipIf", [...phase.skipIf, newSkipGuard.trim()]);
                      setNewSkipGuard("");
                    }
                  }}
                >
                  <Plus className="h-3 w-3" />
                </Button>
              </div>
            </div>
          </div>

          <Separator className="opacity-30" />

          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">On Advance</label>
            <Select
              value={phase.onVerdict.advance.target ?? "__none__"}
              onValueChange={(v) => updateVerdict("advance", "target", v === "__none__" ? null : v)}
            >
              <SelectTrigger size="sm" className="mt-1 w-full text-xs">
                <SelectValue placeholder="Next phase (auto)" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">Auto (next phase)</SelectItem>
                {otherPhases.map((id) => (
                  <SelectItem key={id} value={id}>{id}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">On Rework</label>
            <Select
              value={phase.onVerdict.rework.target ?? "__none__"}
              onValueChange={(v) => updateVerdict("rework", "target", v === "__none__" ? null : v)}
            >
              <SelectTrigger size="sm" className="mt-1 w-full text-xs">
                <SelectValue placeholder="Same phase (default)" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">Same phase (default)</SelectItem>
                {otherPhases.map((id) => (
                  <SelectItem key={id} value={id}>{id}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <label className="flex items-center gap-2 mt-2 text-xs text-muted-foreground cursor-pointer">
              <input
                type="checkbox"
                checked={phase.onVerdict.rework.allowAgentTarget}
                onChange={(e) => updateVerdict("rework", "allowAgentTarget", e.target.checked)}
                className="rounded"
              />
              Allow agent to override target
            </label>
          </div>

          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">On Fail</label>
            <Select
              value={phase.onVerdict.fail.target ?? "__none__"}
              onValueChange={(v) => updateVerdict("fail", "target", v === "__none__" ? null : v)}
            >
              <SelectTrigger size="sm" className="mt-1 w-full text-xs">
                <SelectValue placeholder="Stop workflow (default)" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">Stop workflow (default)</SelectItem>
                {otherPhases.map((id) => (
                  <SelectItem key={id} value={id}>{id}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function VariableCard({
  variable,
  onChange,
  onRemove,
}: {
  variable: VariableEntry;
  onChange: (v: VariableEntry) => void;
  onRemove: () => void;
}) {
  return (
    <Card className="border-border/40 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4 space-y-3">
        <div className="flex items-start justify-between">
          <div className="flex-1 grid grid-cols-2 gap-3">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
              <Input
                value={variable.name}
                onChange={(e) => onChange({ ...variable, name: e.target.value })}
                className="mt-1 font-mono text-xs h-8"
                placeholder="variable_name"
              />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Default</label>
              <Input
                value={variable.default}
                onChange={(e) => onChange({ ...variable, default: e.target.value })}
                className="mt-1 text-xs h-8"
                placeholder="Default value"
              />
            </div>
          </div>
          <button type="button" onClick={onRemove} className="ml-2 mt-4 text-muted-foreground hover:text-destructive">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
          <Input
            value={variable.description}
            onChange={(e) => onChange({ ...variable, description: e.target.value })}
            className="mt-1 text-xs h-8"
            placeholder="What this variable controls"
          />
        </div>
        <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
          <input
            type="checkbox"
            checked={variable.required}
            onChange={(e) => onChange({ ...variable, required: e.target.checked })}
            className="rounded"
          />
          Required
        </label>
      </CardContent>
    </Card>
  );
}

function TransitionsTable({ phases }: { phases: PhaseEntry[] }) {
  if (phases.length === 0) {
    return <p className="text-sm text-muted-foreground/60 py-4 text-center">No phases configured</p>;
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border/30">
            <th className="text-left py-2 pr-4 text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Phase</th>
            <th className="text-left py-2 pr-4 text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Advance</th>
            <th className="text-left py-2 pr-4 text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Rework</th>
            <th className="text-left py-2 text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Fail</th>
          </tr>
        </thead>
        <tbody>
          {phases.map((p, i) => (
            <tr key={p.id} className="border-b border-border/20">
              <td className="py-2 pr-4 font-mono font-medium">{p.id}</td>
              <td className="py-2 pr-4 text-muted-foreground font-mono">
                {p.onVerdict.advance.target ?? (i < phases.length - 1 ? `${phases[i + 1].id} (auto)` : "end")}
              </td>
              <td className="py-2 pr-4 text-muted-foreground font-mono">
                {p.onVerdict.rework.target ?? `${p.id} (self)`}
                {p.onVerdict.rework.allowAgentTarget && <Badge variant="outline" className="ml-1 text-[9px]">agent</Badge>}
              </td>
              <td className="py-2 text-muted-foreground font-mono">
                {p.onVerdict.fail.target ?? "stop"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function TagInput({
  values,
  onChange,
  placeholder,
}: {
  values: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
}) {
  const [input, setInput] = useState("");
  return (
    <div className="space-y-1">
      <div className="flex flex-wrap gap-1">
        {values.map((v, i) => (
          <Badge key={i} variant="secondary" className="text-xs font-mono gap-1">
            {v}
            <button type="button" onClick={() => onChange(values.filter((_, j) => j !== i))}>
              <X className="h-2.5 w-2.5" />
            </button>
          </Badge>
        ))}
      </div>
      <Input
        value={input}
        onChange={(e) => setInput(e.target.value)}
        placeholder={placeholder ?? "Type and press Enter"}
        className="text-xs h-7 font-mono"
        onKeyDown={(e) => {
          if (e.key === "Enter" && input.trim()) {
            e.preventDefault();
            onChange([...values, input.trim()]);
            setInput("");
          }
        }}
      />
    </div>
  );
}

function ListInput({
  values,
  onChange,
  placeholder,
}: {
  values: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
}) {
  const [input, setInput] = useState("");
  return (
    <div className="space-y-1">
      {values.map((v, i) => (
        <div key={i} className="flex items-center gap-1">
          <span className="text-xs font-mono flex-1 truncate">{v}</span>
          <button type="button" onClick={() => onChange(values.filter((_, j) => j !== i))} className="text-muted-foreground hover:text-destructive">
            <X className="h-3 w-3" />
          </button>
        </div>
      ))}
      <div className="flex gap-1">
        <Input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder={placeholder ?? "Add item"}
          className="text-xs h-7 font-mono flex-1"
          onKeyDown={(e) => {
            if (e.key === "Enter" && input.trim()) {
              e.preventDefault();
              onChange([...values, input.trim()]);
              setInput("");
            }
          }}
        />
        <Button size="sm" variant="outline" className="h-7 px-2" disabled={!input.trim()} onClick={() => { if (input.trim()) { onChange([...values, input.trim()]); setInput(""); } }}>
          <Plus className="h-3 w-3" />
        </Button>
      </div>
    </div>
  );
}

function KeyValueEditor({
  entries,
  onChange,
}: {
  entries: Array<{ key: string; value: string }>;
  onChange: (v: Array<{ key: string; value: string }>) => void;
}) {
  return (
    <div className="space-y-1">
      {entries.map((entry, i) => (
        <div key={i} className="flex items-center gap-1">
          <Input
            value={entry.key}
            onChange={(e) => onChange(entries.map((en, j) => j === i ? { ...en, key: e.target.value } : en))}
            placeholder="KEY"
            className="text-xs h-7 font-mono flex-1"
          />
          <span className="text-muted-foreground text-xs">=</span>
          <Input
            value={entry.value}
            onChange={(e) => onChange(entries.map((en, j) => j === i ? { ...en, value: e.target.value } : en))}
            placeholder="value"
            className="text-xs h-7 font-mono flex-1"
          />
          <button type="button" onClick={() => onChange(entries.filter((_, j) => j !== i))} className="text-muted-foreground hover:text-destructive">
            <X className="h-3 w-3" />
          </button>
        </div>
      ))}
      <Button size="sm" variant="outline" className="h-7" onClick={() => onChange([...entries, { key: "", value: "" }])}>
        <Plus className="h-3 w-3 mr-1" />
        Add
      </Button>
    </div>
  );
}

function AgentProfileCard({
  profile,
  onEdit,
  onDelete,
}: {
  profile: AgentProfileEntry;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <Card className="border-border/40 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4">
        <div className="flex items-start justify-between">
          <div className="min-w-0 flex-1">
            <p className="font-mono text-sm text-primary font-medium">{profile.name || "Unnamed"}</p>
            {profile.description && <p className="text-xs text-muted-foreground/60 mt-0.5">{profile.description}</p>}
            <div className="flex items-center gap-2 mt-2 flex-wrap">
              {profile.role && <Badge variant="outline" className="text-[10px]">{profile.role}</Badge>}
              {profile.model && <span className="text-[10px] text-muted-foreground font-mono">{profile.model}</span>}
              {profile.tool && <span className="text-[10px] text-muted-foreground font-mono">{profile.tool}</span>}
            </div>
          </div>
          <div className="flex items-center gap-1 shrink-0 ml-2">
            <Button size="sm" variant="ghost" className="h-7 px-2" onClick={onEdit}>
              <Pencil className="h-3 w-3" />
            </Button>
            <Button size="sm" variant="ghost" className="h-7 px-2 text-destructive/60 hover:text-destructive" onClick={onDelete}>
              <Trash2 className="h-3 w-3" />
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function AgentProfileEditor({
  profile,
  onSave,
  onCancel,
}: {
  profile: AgentProfileEntry;
  onSave: (p: AgentProfileEntry) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<AgentProfileEntry>({ ...profile, mcpServers: [...profile.mcpServers], skills: [...profile.skills] });
  return (
    <Card className="border-primary/30 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4 space-y-3">
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
            <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="agent-name" disabled={!!profile.name} />
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Role</label>
            <Input value={draft.role} onChange={(e) => setDraft({ ...draft, role: e.target.value })} className="mt-1 text-xs h-8" placeholder="implementer" />
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
          <Input value={draft.description} onChange={(e) => setDraft({ ...draft, description: e.target.value })} className="mt-1 text-xs h-8" placeholder="What this agent does" />
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">System Prompt</label>
          <Textarea value={draft.systemPrompt} onChange={(e) => setDraft({ ...draft, systemPrompt: e.target.value })} className="mt-1 font-mono text-xs" rows={8} placeholder="System prompt for this agent profile" />
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Model</label>
            <select value={draft.model} onChange={(e) => setDraft({ ...draft, model: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
              <option value="">Auto</option>
              <option value="claude-sonnet-4-6">claude-sonnet-4-6</option>
              <option value="claude-opus-4-6">claude-opus-4-6</option>
              <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview</option>
            </select>
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Tool</label>
            <select value={draft.tool} onChange={(e) => setDraft({ ...draft, tool: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
              <option value="">Auto</option>
              <option value="claude">claude</option>
              <option value="codex">codex</option>
              <option value="gemini">gemini</option>
            </select>
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">MCP Servers</label>
          <div className="mt-1">
            <TagInput values={draft.mcpServers} onChange={(v) => setDraft({ ...draft, mcpServers: v })} placeholder="Type server name + Enter" />
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Skills</label>
          <div className="mt-1">
            <TagInput values={draft.skills} onChange={(v) => setDraft({ ...draft, skills: v })} placeholder="Type skill name + Enter" />
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onCancel}>Cancel</Button>
          <Button size="sm" onClick={() => onSave(draft)} disabled={!draft.name.trim()}>Save</Button>
        </div>
      </CardContent>
    </Card>
  );
}

function AgentsTab({
  agents,
  onChange,
}: {
  agents: AgentProfileEntry[];
  onChange: (agents: AgentProfileEntry[]) => void;
}) {
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);

  return (
    <div className="mt-4 space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Agent Profiles</p>
        <Button size="sm" variant="outline" onClick={() => { setAdding(true); setEditingIdx(null); }}>
          <Plus className="h-3.5 w-3.5 mr-1" />
          Add Agent
        </Button>
      </div>
      {adding && (
        <AgentProfileEditor
          profile={makeAgentProfile()}
          onSave={(p) => { onChange([...agents, p]); setAdding(false); }}
          onCancel={() => setAdding(false)}
        />
      )}
      {agents.map((agent, i) =>
        editingIdx === i ? (
          <AgentProfileEditor
            key={i}
            profile={agent}
            onSave={(p) => { onChange(agents.map((a, j) => j === i ? p : a)); setEditingIdx(null); }}
            onCancel={() => setEditingIdx(null)}
          />
        ) : (
          <AgentProfileCard
            key={i}
            profile={agent}
            onEdit={() => { setEditingIdx(i); setAdding(false); }}
            onDelete={() => onChange(agents.filter((_, j) => j !== i))}
          />
        )
      )}
      {agents.length === 0 && !adding && (
        <p className="text-sm text-muted-foreground/60 text-center py-4">No agent profiles defined</p>
      )}
    </div>
  );
}

function FieldCard({ field, onEdit, onRemove }: { field: ContractField; onEdit: () => void; onRemove: () => void }) {
  return (
    <div className="border border-border/30 rounded-md p-2.5 group">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-xs font-mono font-medium">{field.name}</span>
          <Badge variant="outline" className="text-[9px] h-4 px-1">{field.type}</Badge>
          {field.type === "array" && field.itemsType && (
            <span className="text-[9px] text-muted-foreground/40">of {field.itemsType}</span>
          )}
        </div>
        <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <button onClick={onEdit} className="text-[10px] text-muted-foreground hover:text-foreground">edit</button>
          <button onClick={onRemove} className="text-[10px] text-destructive/60 hover:text-destructive">✕</button>
        </div>
      </div>
      {field.description && (
        <p className="text-[10px] text-muted-foreground/50 mt-1 truncate">{field.description}</p>
      )}
      {field.enumValues.length > 0 && (
        <div className="flex gap-1 mt-1 flex-wrap">
          {field.enumValues.map(v => <Badge key={v} variant="outline" className="text-[8px] h-3.5 px-1">{v}</Badge>)}
        </div>
      )}
    </div>
  );
}

function FieldEditorForm({ draft, setDraft, onSave, onCancel }: { draft: ContractField; setDraft: (d: ContractField) => void; onSave: () => void; onCancel: () => void }) {
  return (
    <div className="border border-primary/30 bg-primary/5 rounded-md p-3 space-y-2">
      <div className="grid grid-cols-2 gap-2">
        <div>
          <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
          <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} className="mt-0.5 h-7 text-xs font-mono" placeholder="field_name" />
        </div>
        <div>
          <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</label>
          <select value={draft.type} onChange={(e) => setDraft({ ...draft, type: e.target.value as ContractField["type"] })} className="mt-0.5 h-7 w-full rounded-md border border-input bg-background px-2 text-xs">
            <option value="string">string</option>
            <option value="array">array</option>
            <option value="integer">integer</option>
            <option value="boolean">boolean</option>
          </select>
        </div>
      </div>
      <div>
        <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
        <Input value={draft.description} onChange={(e) => setDraft({ ...draft, description: e.target.value })} className="mt-0.5 h-7 text-xs" placeholder="What this field represents..." />
      </div>
      {draft.type === "array" && (
        <div>
          <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Items Type</label>
          <select value={draft.itemsType} onChange={(e) => setDraft({ ...draft, itemsType: e.target.value })} className="mt-0.5 h-7 w-full rounded-md border border-input bg-background px-2 text-xs">
            <option value="string">string</option>
            <option value="integer">integer</option>
            <option value="boolean">boolean</option>
          </select>
        </div>
      )}
      <div>
        <label className="text-[10px] uppercase tracking-wider text-muted-foreground/60 font-medium">Enum Values (optional)</label>
        <TagInput values={draft.enumValues} onChange={(v) => setDraft({ ...draft, enumValues: v })} placeholder="value + Enter" />
      </div>
      <div className="flex gap-2 pt-1">
        <Button size="sm" className="h-6 text-[10px]" onClick={onSave} disabled={!draft.name.trim()}>Save Field</Button>
        <Button size="sm" variant="outline" className="h-6 text-[10px]" onClick={onCancel}>Cancel</Button>
      </div>
    </div>
  );
}

function ContractFieldEditor({ fields, onChange }: { fields: ContractField[]; onChange: (fields: ContractField[]) => void }) {
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [draft, setDraft] = useState<ContractField | null>(null);

  const addField = () => {
    const newField: ContractField = { name: "", type: "string", description: "", itemsType: "string", enumValues: [] };
    setDraft(newField);
    setEditingIndex(fields.length);
  };

  const saveField = () => {
    if (!draft || !draft.name.trim()) return;
    const next = [...fields];
    if (editingIndex !== null && editingIndex < fields.length) {
      next[editingIndex] = draft;
    } else {
      next.push(draft);
    }
    onChange(next);
    setEditingIndex(null);
    setDraft(null);
  };

  const removeField = (i: number) => {
    onChange(fields.filter((_, j) => j !== i));
  };

  const startEdit = (i: number) => {
    setEditingIndex(i);
    setDraft({ ...fields[i], enumValues: [...fields[i].enumValues] });
  };

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Fields</label>
        <Button size="sm" variant="outline" className="h-5 text-[10px] px-1.5" onClick={addField}>
          + Add Field
        </Button>
      </div>

      {fields.map((field, i) => (
        editingIndex === i && draft ? (
          <FieldEditorForm key={i} draft={draft} setDraft={setDraft} onSave={saveField} onCancel={() => { setEditingIndex(null); setDraft(null); }} />
        ) : (
          <FieldCard key={i} field={field} onEdit={() => startEdit(i)} onRemove={() => removeField(i)} />
        )
      ))}

      {editingIndex !== null && editingIndex >= fields.length && draft && (
        <FieldEditorForm draft={draft} setDraft={setDraft} onSave={saveField} onCancel={() => { setEditingIndex(null); setDraft(null); }} />
      )}

      {fields.length === 0 && editingIndex === null && (
        <p className="text-[10px] text-muted-foreground/40 py-2">No fields defined.</p>
      )}
    </div>
  );
}

function PhaseDefinitionEditor({
  phase,
  agentNames,
  onSave,
  onCancel,
}: {
  phase: PhaseDefinitionEntry;
  agentNames: string[];
  onSave: (p: PhaseDefinitionEntry) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<PhaseDefinitionEntry>({
    ...phase,
    decisionContract: { ...phase.decisionContract, requiredEvidence: [...phase.decisionContract.requiredEvidence], fields: phase.decisionContract.fields.map(f => ({ ...f, enumValues: [...f.enumValues] })) },
    outputContract: { ...phase.outputContract, requiredFields: [...phase.outputContract.requiredFields], fields: phase.outputContract.fields.map(f => ({ ...f, enumValues: [...f.enumValues] })) },
    command: { ...phase.command, args: [...phase.command.args] },
  });

  return (
    <Card className="border-primary/30 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4 space-y-3">
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Phase ID</label>
          <Input value={draft.id} onChange={(e) => setDraft({ ...draft, id: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="phase-id" disabled={!!phase.id} />
        </div>

        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium mb-1 block">Mode</label>
          <div className="flex gap-1">
            {(["agent", "command", "manual"] as const).map((m) => (
              <button
                key={m}
                type="button"
                onClick={() => setDraft({ ...draft, mode: m })}
                className={`flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium transition-colors ${
                  draft.mode === m ? "border-primary/40 bg-primary/10 text-primary" : "border-border/40 bg-card/60 text-muted-foreground hover:border-border/60"
                }`}
              >
                {m === "agent" && <Bot className="h-3 w-3" />}
                {m === "command" && <Terminal className="h-3 w-3" />}
                {m === "manual" && <Hand className="h-3 w-3" />}
                {m.charAt(0).toUpperCase() + m.slice(1)}
              </button>
            ))}
          </div>
        </div>

        {draft.mode === "agent" && (
          <>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Agent</label>
              <select value={draft.agent} onChange={(e) => setDraft({ ...draft, agent: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
                <option value="">Select agent</option>
                {agentNames.map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Directive</label>
              <Textarea value={draft.directive} onChange={(e) => setDraft({ ...draft, directive: e.target.value })} className="mt-1 font-mono text-xs" rows={4} placeholder="Instructions for the agent" />
            </div>
            <Separator className="opacity-30" />
            <p className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Decision Contract</p>
            <div className="grid grid-cols-3 gap-3">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Min Confidence</label>
                <Input type="number" min={0} max={1} step={0.1} value={draft.decisionContract.minConfidence} onChange={(e) => setDraft({ ...draft, decisionContract: { ...draft.decisionContract, minConfidence: parseFloat(e.target.value) || 0 } })} className="mt-1 text-xs h-8" />
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Max Risk</label>
                <select value={draft.decisionContract.maxRisk} onChange={(e) => setDraft({ ...draft, decisionContract: { ...draft.decisionContract, maxRisk: e.target.value } })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
                  <option value="low">low</option>
                  <option value="medium">medium</option>
                  <option value="high">high</option>
                </select>
              </div>
              <div className="flex items-end pb-1">
                <label className="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer">
                  <input type="checkbox" checked={draft.decisionContract.allowMissingDecision} onChange={(e) => setDraft({ ...draft, decisionContract: { ...draft.decisionContract, allowMissingDecision: e.target.checked } })} className="rounded" />
                  Allow Missing
                </label>
              </div>
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Required Evidence</label>
              <div className="mt-1">
                <TagInput values={draft.decisionContract.requiredEvidence} onChange={(v) => setDraft({ ...draft, decisionContract: { ...draft.decisionContract, requiredEvidence: v } })} placeholder="evidence + Enter" />
              </div>
            </div>
            <ContractFieldEditor
              fields={draft.decisionContract.fields}
              onChange={(fields) => setDraft({ ...draft, decisionContract: { ...draft.decisionContract, fields } })}
            />
            <Separator className="opacity-30" />
            <p className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Output Contract</p>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Kind</label>
                <Input value={draft.outputContract.kind} onChange={(e) => setDraft({ ...draft, outputContract: { ...draft.outputContract, kind: e.target.value } })} className="mt-1 text-xs h-8 font-mono" placeholder="e.g. implementation_result" />
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Required Fields</label>
                <div className="mt-1">
                  <TagInput values={draft.outputContract.requiredFields} onChange={(v) => setDraft({ ...draft, outputContract: { ...draft.outputContract, requiredFields: v } })} placeholder="field + Enter" />
                </div>
              </div>
            </div>
            <ContractFieldEditor
              fields={draft.outputContract.fields}
              onChange={(fields) => setDraft({ ...draft, outputContract: { ...draft.outputContract, fields } })}
            />
          </>
        )}

        {draft.mode === "command" && (
          <>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Program</label>
              <Input value={draft.command.program} onChange={(e) => setDraft({ ...draft, command: { ...draft.command, program: e.target.value } })} className="mt-1 font-mono text-xs h-8" placeholder="/usr/bin/program" />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Args</label>
              <div className="mt-1">
                <ListInput values={draft.command.args} onChange={(v) => setDraft({ ...draft, command: { ...draft.command, args: v } })} placeholder="Argument" />
              </div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">CWD Mode</label>
                <select value={draft.command.cwdMode} onChange={(e) => setDraft({ ...draft, command: { ...draft.command, cwdMode: e.target.value } })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
                  <option value="project_root">project_root</option>
                  <option value="task_root">task_root</option>
                </select>
              </div>
              <div>
                <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Timeout (secs)</label>
                <Input type="number" min={1} value={draft.command.timeoutSecs} onChange={(e) => setDraft({ ...draft, command: { ...draft.command, timeoutSecs: parseInt(e.target.value) || 300 } })} className="mt-1 text-xs h-8" />
              </div>
            </div>
          </>
        )}

        {draft.mode === "manual" && (
          <>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Instructions</label>
              <Textarea value={draft.manualInstructions} onChange={(e) => setDraft({ ...draft, manualInstructions: e.target.value })} className="mt-1 text-xs" rows={6} placeholder="Instructions for the manual step" />
            </div>
            <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
              <input type="checkbox" checked={draft.approvalNoteRequired} onChange={(e) => setDraft({ ...draft, approvalNoteRequired: e.target.checked })} className="rounded" />
              Approval Note Required
            </label>
          </>
        )}

        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onCancel}>Cancel</Button>
          <Button size="sm" onClick={() => onSave(draft)} disabled={!draft.id.trim()}>Save</Button>
        </div>
      </CardContent>
    </Card>
  );
}

function PhaseConfigTab({
  phases,
  agentNames,
  onChange,
}: {
  phases: PhaseDefinitionEntry[];
  agentNames: string[];
  onChange: (phases: PhaseDefinitionEntry[]) => void;
}) {
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);

  return (
    <div className="mt-4 space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Phase Definitions</p>
        <Button size="sm" variant="outline" onClick={() => { setAdding(true); setEditingIdx(null); }}>
          <Plus className="h-3.5 w-3.5 mr-1" />
          Add Phase Definition
        </Button>
      </div>
      {adding && (
        <PhaseDefinitionEditor
          phase={makePhaseDefinition()}
          agentNames={agentNames}
          onSave={(p) => { onChange([...phases, p]); setAdding(false); }}
          onCancel={() => setAdding(false)}
        />
      )}
      {phases.map((phase, i) =>
        editingIdx === i ? (
          <PhaseDefinitionEditor
            key={i}
            phase={phase}
            agentNames={agentNames}
            onSave={(p) => { onChange(phases.map((ph, j) => j === i ? p : ph)); setEditingIdx(null); }}
            onCancel={() => setEditingIdx(null)}
          />
        ) : (
          <Card key={i} className="border-border/40 bg-card/60">
            <CardContent className="pt-3 pb-3 px-4">
              <div className="flex items-start justify-between">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <p className="font-mono text-sm font-medium">{phase.id || "Unnamed"}</p>
                    <Badge variant="outline" className="text-[10px]">
                      {phase.mode === "agent" && <Bot className="h-2.5 w-2.5 mr-0.5" />}
                      {phase.mode === "command" && <Terminal className="h-2.5 w-2.5 mr-0.5" />}
                      {phase.mode === "manual" && <Hand className="h-2.5 w-2.5 mr-0.5" />}
                      {phase.mode}
                    </Badge>
                  </div>
                  {phase.mode === "agent" && phase.agent && (
                    <p className="text-xs text-muted-foreground/60 mt-0.5 font-mono">{phase.agent}</p>
                  )}
                  {phase.directive && (
                    <p className="text-xs text-muted-foreground/60 mt-0.5 truncate max-w-md">{phase.directive}</p>
                  )}
                </div>
                <div className="flex items-center gap-1 shrink-0 ml-2">
                  <Button size="sm" variant="ghost" className="h-7 px-2" onClick={() => { setEditingIdx(i); setAdding(false); }}>
                    <Pencil className="h-3 w-3" />
                  </Button>
                  <Button size="sm" variant="ghost" className="h-7 px-2 text-destructive/60 hover:text-destructive" onClick={() => onChange(phases.filter((_, j) => j !== i))}>
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        )
      )}
      {phases.length === 0 && !adding && (
        <p className="text-sm text-muted-foreground/60 text-center py-4">No phase definitions configured</p>
      )}
    </div>
  );
}

function McpServerEditor({
  server,
  onSave,
  onCancel,
}: {
  server: McpServerEntry;
  onSave: (s: McpServerEntry) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<McpServerEntry>({ ...server, args: [...server.args], tools: [...server.tools], env: server.env.map((e) => ({ ...e })) });
  return (
    <Card className="border-primary/30 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4 space-y-3">
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
            <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="server-name" />
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Transport</label>
            <select value={draft.transport} onChange={(e) => setDraft({ ...draft, transport: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
              <option value="stdio">stdio</option>
              <option value="sse">sse</option>
              <option value="streamable-http">streamable-http</option>
            </select>
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Command</label>
          <Input value={draft.command} onChange={(e) => setDraft({ ...draft, command: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="/path/to/server" />
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Args</label>
          <div className="mt-1">
            <ListInput values={draft.args} onChange={(v) => setDraft({ ...draft, args: v })} placeholder="Argument" />
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Tools</label>
          <div className="mt-1">
            <TagInput values={draft.tools} onChange={(v) => setDraft({ ...draft, tools: v })} placeholder="Tool name + Enter" />
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Environment Variables</label>
          <div className="mt-1">
            <KeyValueEditor entries={draft.env} onChange={(v) => setDraft({ ...draft, env: v })} />
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onCancel}>Cancel</Button>
          <Button size="sm" onClick={() => onSave(draft)} disabled={!draft.name.trim()}>Save</Button>
        </div>
      </CardContent>
    </Card>
  );
}

function McpServersTab({
  servers,
  onChange,
}: {
  servers: McpServerEntry[];
  onChange: (servers: McpServerEntry[]) => void;
}) {
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);

  return (
    <div className="mt-4 space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">MCP Servers</p>
        <Button size="sm" variant="outline" onClick={() => { setAdding(true); setEditingIdx(null); }}>
          <Plus className="h-3.5 w-3.5 mr-1" />
          Add Server
        </Button>
      </div>
      {adding && (
        <McpServerEditor
          server={makeMcpServer()}
          onSave={(s) => { onChange([...servers, s]); setAdding(false); }}
          onCancel={() => setAdding(false)}
        />
      )}
      {servers.map((server, i) =>
        editingIdx === i ? (
          <McpServerEditor
            key={i}
            server={server}
            onSave={(s) => { onChange(servers.map((sv, j) => j === i ? s : sv)); setEditingIdx(null); }}
            onCancel={() => setEditingIdx(null)}
          />
        ) : (
          <Card key={i} className="border-border/40 bg-card/60">
            <CardContent className="pt-3 pb-3 px-4">
              <div className="flex items-start justify-between">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <p className="font-mono text-sm font-medium">{server.name || "Unnamed"}</p>
                    {server.transport && <Badge variant="outline" className="text-[10px]">{server.transport}</Badge>}
                  </div>
                  <p className="text-xs text-muted-foreground/60 mt-0.5 font-mono truncate">{server.command} {server.args.join(" ")}</p>
                  {server.tools.length > 0 && (
                    <div className="flex flex-wrap gap-1 mt-1.5">
                      {server.tools.map((t, ti) => (
                        <Badge key={ti} variant="secondary" className="text-[10px] font-mono">{t}</Badge>
                      ))}
                    </div>
                  )}
                </div>
                <div className="flex items-center gap-1 shrink-0 ml-2">
                  <Button size="sm" variant="ghost" className="h-7 px-2" onClick={() => { setEditingIdx(i); setAdding(false); }}>
                    <Pencil className="h-3 w-3" />
                  </Button>
                  <Button size="sm" variant="ghost" className="h-7 px-2 text-destructive/60 hover:text-destructive" onClick={() => onChange(servers.filter((_, j) => j !== i))}>
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        )
      )}
      {servers.length === 0 && !adding && (
        <p className="text-sm text-muted-foreground/60 text-center py-4">No MCP servers configured</p>
      )}
    </div>
  );
}

function ScheduleEditor({
  schedule,
  workflowIds,
  onSave,
  onCancel,
}: {
  schedule: ScheduleEntry;
  workflowIds: string[];
  onSave: (s: ScheduleEntry) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<ScheduleEntry>({ ...schedule });
  return (
    <Card className="border-primary/30 bg-card/60">
      <CardContent className="pt-3 pb-3 px-4 space-y-3">
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">ID</label>
            <Input value={draft.id} onChange={(e) => setDraft({ ...draft, id: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="schedule-id" />
          </div>
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Workflow Ref</label>
            <select value={draft.workflowRef} onChange={(e) => setDraft({ ...draft, workflowRef: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
              <option value="">Select workflow</option>
              {workflowIds.map((id) => <option key={id} value={id}>{id}</option>)}
            </select>
          </div>
        </div>
        <div>
          <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Cron Expression</label>
          <Input value={draft.cron} onChange={(e) => setDraft({ ...draft, cron: e.target.value })} className="mt-1 font-mono text-xs h-8" placeholder="0 */6 * * *" />
          <p className="text-[10px] text-muted-foreground/40 mt-0.5">Format: min hour day month weekday</p>
        </div>
        <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
          <input type="checkbox" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} className="rounded" />
          Enabled
        </label>
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onCancel}>Cancel</Button>
          <Button size="sm" onClick={() => onSave(draft)} disabled={!draft.id.trim()}>Save</Button>
        </div>
      </CardContent>
    </Card>
  );
}

function SchedulesTab({
  schedules,
  workflowIds,
  onChange,
}: {
  schedules: ScheduleEntry[];
  workflowIds: string[];
  onChange: (schedules: ScheduleEntry[]) => void;
}) {
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);

  return (
    <div className="mt-4 space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Schedules</p>
        <Button size="sm" variant="outline" onClick={() => { setAdding(true); setEditingIdx(null); }}>
          <Plus className="h-3.5 w-3.5 mr-1" />
          Add Schedule
        </Button>
      </div>
      {adding && (
        <ScheduleEditor
          schedule={makeSchedule()}
          workflowIds={workflowIds}
          onSave={(s) => { onChange([...schedules, s]); setAdding(false); }}
          onCancel={() => setAdding(false)}
        />
      )}
      {schedules.map((sched, i) =>
        editingIdx === i ? (
          <ScheduleEditor
            key={i}
            schedule={sched}
            workflowIds={workflowIds}
            onSave={(s) => { onChange(schedules.map((sc, j) => j === i ? s : sc)); setEditingIdx(null); }}
            onCancel={() => setEditingIdx(null)}
          />
        ) : (
          <Card key={i} className="border-border/40 bg-card/60">
            <CardContent className="pt-3 pb-3 px-4">
              <div className="flex items-start justify-between">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <p className="font-mono text-sm font-medium">{sched.id || "Unnamed"}</p>
                    <Badge variant={sched.enabled ? "default" : "secondary"} className="text-[10px]">
                      {sched.enabled ? "enabled" : "disabled"}
                    </Badge>
                  </div>
                  <p className="text-xs text-muted-foreground/60 mt-0.5 font-mono">{sched.cron}</p>
                  {sched.workflowRef && <p className="text-xs text-muted-foreground/60 font-mono">{sched.workflowRef}</p>}
                </div>
                <div className="flex items-center gap-1 shrink-0 ml-2">
                  <Button size="sm" variant="ghost" className="h-7 px-2" onClick={() => { setEditingIdx(i); setAdding(false); }}>
                    <Pencil className="h-3 w-3" />
                  </Button>
                  <Button size="sm" variant="ghost" className="h-7 px-2 text-destructive/60 hover:text-destructive" onClick={() => onChange(schedules.filter((_, j) => j !== i))}>
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        )
      )}
      {schedules.length === 0 && !adding && (
        <p className="text-sm text-muted-foreground/60 text-center py-4">No schedules configured</p>
      )}
    </div>
  );
}

function PostSuccessSection({
  config,
  onChange,
}: {
  config: PostSuccessConfig;
  onChange: (c: PostSuccessConfig) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  return (
    <Card className="border-border/40 bg-card/60">
      <button type="button" onClick={() => setExpanded(!expanded)} className="w-full px-4 py-3 flex items-center justify-between">
        <span className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Post-Success Config</span>
        {expanded ? <ChevronUp className="h-4 w-4 text-muted-foreground" /> : <ChevronDown className="h-4 w-4 text-muted-foreground" />}
      </button>
      {expanded && (
        <CardContent className="px-4 pb-4 pt-0 space-y-3">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Merge Strategy</label>
              <select value={config.mergeStrategy} onChange={(e) => onChange({ ...config, mergeStrategy: e.target.value })} className="mt-1 w-full h-8 rounded-md border border-input bg-background px-2 text-xs">
                <option value="squash">squash</option>
                <option value="merge">merge</option>
                <option value="rebase">rebase</option>
              </select>
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Target Branch</label>
              <Input value={config.targetBranch} onChange={(e) => onChange({ ...config, targetBranch: e.target.value })} className="mt-1 text-xs h-8 font-mono" placeholder="main" />
            </div>
          </div>
          <div className="flex flex-wrap gap-4">
            <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
              <input type="checkbox" checked={config.createPr} onChange={(e) => onChange({ ...config, createPr: e.target.checked })} className="rounded" />
              Create PR
            </label>
            <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
              <input type="checkbox" checked={config.autoMerge} onChange={(e) => onChange({ ...config, autoMerge: e.target.checked })} className="rounded" />
              Auto Merge
            </label>
            <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
              <input type="checkbox" checked={config.cleanupWorktree} onChange={(e) => onChange({ ...config, cleanupWorktree: e.target.checked })} className="rounded" />
              Cleanup Worktree
            </label>
          </div>
        </CardContent>
      )}
    </Card>
  );
}

const SAVE_CONFIG = `mutation SaveWorkflowConfig($configJson: String!) { saveWorkflowConfig(configJson: $configJson) }`;

const VALIDATE_CONFIG = `mutation ValidateWorkflowConfig($configJson: String!) { validateWorkflowConfig(configJson: $configJson) }`;

const UPSERT_MUTATION = `mutation UpsertWorkflowDefinition($id: String!, $name: String!, $description: String, $phases: String!, $variables: String) { upsertWorkflowDefinition(id: $id, name: $name, description: $description, phases: $phases, variables: $variables) }`;

function EditorCore({
  initial,
  isNew,
}: {
  initial: WorkflowDef;
  isNew: boolean;
}) {
  const [def, setDef] = useState<WorkflowDef>(initial);
  const [dirty, setDirty] = useState(isNew);
  const [selectedPhaseIdx, setSelectedPhaseIdx] = useState<number | null>(null);
  const [showPreview, setShowPreview] = useState(false);
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [saveMsg, setSaveMsg] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState(0);
  const [, upsertDef] = useMutation(UPSERT_MUTATION);
  const [, saveConfig] = useMutation(SAVE_CONFIG);
  const [{ fetching: validating }, validateConfig] = useMutation(VALIDATE_CONFIG);

  const [agents, setAgents] = useState<AgentProfileEntry[]>([]);
  const [phaseDefinitions, setPhaseDefinitions] = useState<PhaseDefinitionEntry[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServerEntry[]>([]);
  const [schedules, setSchedules] = useState<ScheduleEntry[]>([]);

  const [configResult] = useQuery({ query: WorkflowConfigDocument });

  useEffect(() => {
    if (configResult.data?.workflowConfig) {
      const cfg = configResult.data.workflowConfig;
      setAgents(cfg.agentProfiles.map((a) => ({
        name: a.name,
        description: a.description,
        role: a.role ?? "",
        systemPrompt: "",
        model: a.model ?? "",
        tool: a.tool ?? "",
        mcpServers: [...a.mcpServers],
        skills: [...a.skills],
      })));
      setMcpServers(cfg.mcpServers.map((s) => ({
        name: s.name,
        command: s.command,
        args: [...s.args],
        transport: s.transport ?? "stdio",
        tools: [...s.tools],
        env: s.env.map((e) => ({ key: e.key, value: e.value })),
      })));
      setSchedules(cfg.schedules.map((s) => ({
        id: s.id,
        cron: s.cron,
        workflowRef: s.workflowRef ?? "",
        enabled: s.enabled,
      })));
      if (cfg.phaseCatalog.length > 0 && phaseDefinitions.length === 0) {
        setPhaseDefinitions(cfg.phaseCatalog.map((p) => makePhaseDefinition(p.id)));
      }
    }
  }, [configResult.data]);

  const errorPhaseIds = useMemo(() => {
    if (!validation) return new Set<string>();
    return new Set(validation.errors.filter((e) => e.phaseId).map((e) => e.phaseId!));
  }, [validation]);

  const updateDef = useCallback((updater: (prev: WorkflowDef) => WorkflowDef) => {
    setDef((prev) => {
      const next = updater(prev);
      setDirty(true);
      return next;
    });
  }, []);

  const addPhase = () => {
    const idx = def.phases.length;
    const id = `new-phase-${idx + 1}`;
    updateDef((d) => ({ ...d, phases: [...d.phases, makePhaseEntry(id)] }));
    setSelectedPhaseIdx(idx);
  };

  const removePhase = (idx: number) => {
    updateDef((d) => ({ ...d, phases: d.phases.filter((_, i) => i !== idx) }));
    if (selectedPhaseIdx === idx) setSelectedPhaseIdx(null);
    else if (selectedPhaseIdx !== null && selectedPhaseIdx > idx) setSelectedPhaseIdx(selectedPhaseIdx - 1);
  };

  const movePhase = (from: number, to: number) => {
    updateDef((d) => {
      const next = [...d.phases];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return { ...d, phases: next };
    });
    if (selectedPhaseIdx === from) setSelectedPhaseIdx(to);
  };

  const updatePhase = (idx: number, updated: PhaseEntry) => {
    updateDef((d) => ({
      ...d,
      phases: d.phases.map((p, i) => (i === idx ? updated : p)),
    }));
  };

  const buildConfigJson = () => JSON.stringify({
    agents: Object.fromEntries(agents.map((a) => [a.name, {
      description: a.description || undefined,
      role: a.role || undefined,
      system_prompt: a.systemPrompt || undefined,
      model: a.model || undefined,
      tool: a.tool || undefined,
      mcp_servers: a.mcpServers.length > 0 ? a.mcpServers : undefined,
      skills: a.skills.length > 0 ? a.skills : undefined,
    }])),
    phases: Object.fromEntries(phaseDefinitions.map((p) => [p.id, {
      mode: p.mode,
      agent: p.mode === "agent" ? p.agent || undefined : undefined,
      directive: p.mode === "agent" ? p.directive || undefined : undefined,
      decision_contract: p.mode === "agent" ? {
        min_confidence: p.decisionContract.minConfidence,
        max_risk: p.decisionContract.maxRisk,
        allow_missing_decision: p.decisionContract.allowMissingDecision || undefined,
        required_evidence: p.decisionContract.requiredEvidence.length > 0 ? p.decisionContract.requiredEvidence : undefined,
        fields: p.decisionContract.fields.length > 0 ? Object.fromEntries(
          p.decisionContract.fields.map(f => [f.name, {
            type: f.type,
            description: f.description || undefined,
            ...(f.type === "array" && f.itemsType ? { items: { type: f.itemsType } } : {}),
            ...(f.enumValues.length > 0 ? { enum: f.enumValues } : {}),
          }])
        ) : undefined,
      } : undefined,
      output_contract: p.mode === "agent" && p.outputContract.kind ? {
        kind: p.outputContract.kind,
        required_fields: p.outputContract.requiredFields.length > 0 ? p.outputContract.requiredFields : undefined,
        fields: p.outputContract.fields.length > 0 ? Object.fromEntries(
          p.outputContract.fields.map(f => [f.name, {
            type: f.type,
            description: f.description || undefined,
            ...(f.type === "array" && f.itemsType ? { items: { type: f.itemsType } } : {}),
            ...(f.enumValues.length > 0 ? { enum: f.enumValues } : {}),
          }])
        ) : undefined,
      } : undefined,
      command: p.mode === "command" ? {
        program: p.command.program,
        args: p.command.args.length > 0 ? p.command.args : undefined,
        cwd_mode: p.command.cwdMode,
        timeout_secs: p.command.timeoutSecs,
      } : undefined,
      instructions: p.mode === "manual" ? p.manualInstructions || undefined : undefined,
      approval_note_required: p.mode === "manual" ? p.approvalNoteRequired || undefined : undefined,
    }])),
    mcp_servers: Object.fromEntries(mcpServers.map((s) => [s.name, {
      command: s.command,
      args: s.args.length > 0 ? s.args : undefined,
      transport: s.transport !== "stdio" ? s.transport : undefined,
      tools: s.tools.length > 0 ? s.tools : undefined,
      env: s.env.length > 0 ? Object.fromEntries(s.env.map((e) => [e.key, e.value])) : undefined,
    }])),
    schedules: schedules.length > 0 ? schedules.map((s) => ({
      id: s.id,
      cron: s.cron,
      workflow_ref: s.workflowRef || undefined,
      enabled: s.enabled,
    })) : undefined,
    workflows: {
      [def.id]: {
        post_success: {
          merge_strategy: def.postSuccess.mergeStrategy,
          target_branch: def.postSuccess.targetBranch,
          create_pr: def.postSuccess.createPr,
          auto_merge: def.postSuccess.autoMerge,
          cleanup_worktree: def.postSuccess.cleanupWorktree,
        },
      },
    },
  });

  const onValidate = async () => {
    const clientResult = validateDef(def);
    setValidation(clientResult);
    if (!clientResult.valid) return;
    setSaveError(null);
    const configJson = buildConfigJson();
    const { data, error } = await validateConfig({ configJson });
    const serverErrors: string[] = data?.validateWorkflowConfig ?? [];
    if (error) {
      setSaveError(error.message);
    } else if (serverErrors.length > 0) {
      setValidation({
        valid: false,
        errors: serverErrors.map((msg) => ({ message: msg })),
        warnings: [],
      });
    } else {
      setValidation(null);
      setSaveMsg("Configuration is valid");
      setTimeout(() => setSaveMsg(null), 3000);
    }
  };

  const agentNames = useMemo(() => agents.map((a) => a.name).filter(Boolean), [agents]);
  const workflowIds = useMemo(() => {
    const ids: string[] = [];
    if (def.id) ids.push(def.id);
    return ids;
  }, [def.id]);

  const onSave = async () => {
    const result = validateDef(def);
    setValidation(result);
    if (!result.valid) return;
    setSaveError(null);
    const phasesJson = JSON.stringify(def.phases.map((p) => ({
      id: p.id,
      max_rework_attempts: p.maxReworkAttempts,
      skip_if: p.skipIf.length > 0 ? p.skipIf : undefined,
      on_verdict: {
        advance: p.onVerdict.advance.target ? { target: p.onVerdict.advance.target } : undefined,
        rework: p.onVerdict.rework.target || p.onVerdict.rework.allowAgentTarget ? {
          target: p.onVerdict.rework.target ?? undefined,
          allow_agent_target: p.onVerdict.rework.allowAgentTarget || undefined,
        } : undefined,
        fail: p.onVerdict.fail.target ? { target: p.onVerdict.fail.target } : undefined,
      },
    })));
    const variablesJson = def.variables.length > 0 ? JSON.stringify(def.variables.map((v) => ({
      name: v.name,
      description: v.description || undefined,
      required: v.required || undefined,
      default: v.default || undefined,
    }))) : undefined;
    const { error: err } = await upsertDef({
      id: def.id,
      name: def.name,
      description: def.description || null,
      phases: phasesJson,
      variables: variablesJson,
    });
    if (err) {
      setSaveError(err.message);
      return;
    }

    const configJson = buildConfigJson();

    const { error: configErr } = await saveConfig({ configJson });
    if (configErr) {
      setSaveError(configErr.message);
    } else {
      setDirty(false);
      setSaveMsg("Workflow saved");
      setTimeout(() => setSaveMsg(null), 3000);
    }
  };

  const selectedPhase = selectedPhaseIdx !== null ? def.phases[selectedPhaseIdx] : null;
  const allPhaseIds = def.phases.map((p) => p.id);

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-3">
        <div className="flex items-center gap-3 min-w-0">
          <Link to="/workflows/builder" className="text-muted-foreground hover:text-foreground transition-colors">
            <ArrowLeft className="h-4 w-4" />
          </Link>
          <div className="flex items-center gap-2 min-w-0">
            <h1 className="text-2xl font-semibold tracking-tight truncate">{def.name || "Untitled Workflow"}</h1>
            {dirty && <span className="h-2 w-2 rounded-full bg-amber-500 shrink-0" title="Unsaved changes" />}
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <Button size="sm" variant="outline" onClick={() => setShowPreview(!showPreview)}>
            <Eye className="h-3.5 w-3.5 mr-1.5" />
            Preview
          </Button>
          <Button size="sm" variant="outline" onClick={onValidate} disabled={validating}>
            <CheckCircle2 className="h-3.5 w-3.5 mr-1.5" />
            {validating ? "Validating…" : "Validate"}
          </Button>
          <Button size="sm" onClick={onSave}>
            <Save className="h-3.5 w-3.5 mr-1.5" />
            Save
          </Button>
        </div>
      </div>

      {saveMsg && (
        <Alert className="ao-fade-in">
          <AlertDescription>{saveMsg}</AlertDescription>
        </Alert>
      )}

      {saveError && (
        <Alert variant="destructive" role="alert" className="ao-fade-in">
          <AlertDescription>{saveError}</AlertDescription>
        </Alert>
      )}

      {validation && !validation.valid && (
        <Card className="border-destructive/40 bg-destructive/5 ao-fade-in">
          <CardContent className="pt-3 pb-3 px-4">
            <p className="text-xs uppercase tracking-wider text-destructive/80 font-medium mb-2">Validation Errors</p>
            <ul className="space-y-1">
              {validation.errors.map((e, i) => (
                <li key={i} className="text-xs text-destructive flex items-start gap-1.5">
                  <AlertCircle className="h-3 w-3 mt-0.5 shrink-0" />
                  {e.message}
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      <Card className="border-border/40 bg-card/60">
        <CardContent className="pt-4 pb-4 px-4">
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Workflow ID</label>
              <Input
                value={def.id}
                onChange={(e) => updateDef((d) => ({ ...d, id: e.target.value }))}
                disabled={!isNew}
                className="mt-1 font-mono text-xs h-8"
                placeholder="my-workflow"
              />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Name</label>
              <Input
                value={def.name}
                onChange={(e) => updateDef((d) => ({ ...d, name: e.target.value }))}
                className="mt-1 text-xs h-8"
                placeholder="My Workflow"
              />
            </div>
            <div>
              <label className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Description</label>
              <Input
                value={def.description}
                onChange={(e) => updateDef((d) => ({ ...d, description: e.target.value }))}
                className="mt-1 text-xs h-8"
                placeholder="What this workflow does"
              />
            </div>
          </div>
        </CardContent>
      </Card>

      <PostSuccessSection config={def.postSuccess} onChange={(c) => updateDef((d) => ({ ...d, postSuccess: c }))} />

      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList>
          <TabsTrigger value={0}>Phases</TabsTrigger>
          <TabsTrigger value={1}><Users className="h-3 w-3 mr-1" />Agents</TabsTrigger>
          <TabsTrigger value={2}><Settings className="h-3 w-3 mr-1" />Phase Config</TabsTrigger>
          <TabsTrigger value={3}><Server className="h-3 w-3 mr-1" />MCP Servers</TabsTrigger>
          <TabsTrigger value={4}><Clock className="h-3 w-3 mr-1" />Schedules</TabsTrigger>
          <TabsTrigger value={5}>Variables</TabsTrigger>
          <TabsTrigger value={6}>Transitions</TabsTrigger>
        </TabsList>

        <TabsContent value={0}>
          <div className="flex flex-col md:flex-row gap-4 mt-4">
            <div className="flex-1 space-y-4">
              <div className="flex items-center flex-wrap gap-0">
                {def.phases.map((phase, i) => (
                  <PhaseNode
                    key={`${phase.id}-${i}`}
                    phase={phase}
                    index={i}
                    total={def.phases.length}
                    selected={selectedPhaseIdx === i}
                    hasError={errorPhaseIds.has(phase.id)}
                    onSelect={() => setSelectedPhaseIdx(selectedPhaseIdx === i ? null : i)}
                    onMoveLeft={() => movePhase(i, i - 1)}
                    onMoveRight={() => movePhase(i, i + 1)}
                    onRemove={() => removePhase(i)}
                  />
                ))}
                <Button size="sm" variant="outline" onClick={addPhase} className="ml-2">
                  <Plus className="h-3.5 w-3.5 mr-1" />
                  Add Phase
                </Button>
              </div>

              {def.phases.length === 0 && (
                <div className="flex flex-col items-center justify-center py-8 gap-3">
                  <p className="text-sm text-muted-foreground/60">No phases yet</p>
                  <Button variant="outline" onClick={addPhase}>
                    <Plus className="h-3.5 w-3.5 mr-1.5" />
                    Add First Phase
                  </Button>
                </div>
              )}
            </div>

            {selectedPhase && selectedPhaseIdx !== null && (
              <PhaseDetailPanel
                phase={selectedPhase}
                allPhaseIds={allPhaseIds}
                onChange={(updated) => updatePhase(selectedPhaseIdx, updated)}
              />
            )}
          </div>
        </TabsContent>

        <TabsContent value={1}>
          <AgentsTab agents={agents} onChange={(a) => { setAgents(a); setDirty(true); }} />
        </TabsContent>

        <TabsContent value={2}>
          <PhaseConfigTab phases={phaseDefinitions} agentNames={agentNames} onChange={(p) => { setPhaseDefinitions(p); setDirty(true); }} />
        </TabsContent>

        <TabsContent value={3}>
          <McpServersTab servers={mcpServers} onChange={(s) => { setMcpServers(s); setDirty(true); }} />
        </TabsContent>

        <TabsContent value={4}>
          <SchedulesTab schedules={schedules} workflowIds={workflowIds} onChange={(s) => { setSchedules(s); setDirty(true); }} />
        </TabsContent>

        <TabsContent value={5}>
          <div className="mt-4 space-y-3">
            {def.variables.map((v, i) => (
              <VariableCard
                key={i}
                variable={v}
                onChange={(updated) =>
                  updateDef((d) => ({
                    ...d,
                    variables: d.variables.map((vv, j) => (j === i ? updated : vv)),
                  }))
                }
                onRemove={() =>
                  updateDef((d) => ({
                    ...d,
                    variables: d.variables.filter((_, j) => j !== i),
                  }))
                }
              />
            ))}
            <Button
              size="sm"
              variant="outline"
              onClick={() => updateDef((d) => ({ ...d, variables: [...d.variables, makeVariableEntry()] }))}
            >
              <Plus className="h-3.5 w-3.5 mr-1" />
              Add Variable
            </Button>
            {def.variables.length === 0 && (
              <p className="text-sm text-muted-foreground/60 text-center py-4">No variables defined</p>
            )}
          </div>
        </TabsContent>

        <TabsContent value={6}>
          <div className="mt-4">
            <Card className="border-border/40 bg-card/60">
              <CardHeader className="pb-2 pt-3 px-4">
                <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Transition Map</CardTitle>
              </CardHeader>
              <CardContent className="px-4 pb-4">
                <TransitionsTable phases={def.phases} />
              </CardContent>
            </Card>
          </div>
        </TabsContent>
      </Tabs>

      {showPreview && (
        <Card className="border-border/40 bg-card/60 ao-fade-in">
          <CardHeader className="pb-2 pt-3 px-4">
            <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">Preview (JSON)</CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-4">
            <pre className="text-xs font-mono overflow-auto max-h-96 p-3 rounded bg-muted/20">
              {defToPreview(def)}
            </pre>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

const DELETE_MUTATION = `mutation DeleteWorkflowDefinition($id: ID!) { deleteWorkflowDefinition(id: $id) }`;

export function WorkflowBuilderBrowsePage() {
  const navigate = useNavigate();
  const [result, reexecute] = useQuery({ query: WorkflowDefinitionsDocument });
  const [, deleteDef] = useMutation(DELETE_MUTATION);
  const [duplicateTarget, setDuplicateTarget] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const { data, fetching, error } = result;

  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const definitions = data?.workflowDefinitions ?? [];

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Workflow Builder</h1>
          <p className="text-sm text-muted-foreground">Create and manage workflow definitions</p>
        </div>
        <Button onClick={() => navigate("/workflows/builder/new")}>
          <Plus className="h-4 w-4 mr-1.5" />
          New Workflow
        </Button>
      </div>

      {definitions.length === 0 && (
        <div className="flex flex-col items-center justify-center py-12 gap-3">
          <p className="text-sm text-muted-foreground/60">No workflow definitions yet</p>
          <Button variant="outline" onClick={() => navigate("/workflows/builder/new")}>
            <Plus className="h-4 w-4 mr-1.5" />
            Create Your First Workflow
          </Button>
        </div>
      )}

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {definitions.map((def) => (
          <Card key={def.id} className="border-border/40 bg-card/60 hover:border-border/60 transition-colors">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <div>
                <p className="font-mono text-xs text-muted-foreground/60">{def.id}</p>
                <p className="font-medium mt-0.5">{def.name}</p>
                {def.description && (
                  <p className="text-sm text-muted-foreground line-clamp-2 mt-1">{def.description}</p>
                )}
              </div>

              <div className="flex items-center gap-1">
                {def.phases.map((phaseId, i) => (
                  <div key={phaseId} className="flex items-center gap-1">
                    <span className="h-2 w-2 rounded-full bg-primary/50" />
                    {i < def.phases.length - 1 && <span className="w-3 h-px bg-border" />}
                  </div>
                ))}
              </div>

              <div className="flex items-center justify-between">
                <span className="text-xs text-muted-foreground">{def.phases.length} phase{def.phases.length !== 1 ? "s" : ""}</span>
                <div className="flex items-center gap-1">
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2"
                    onClick={() => setDuplicateTarget(def.id)}
                  >
                    <Copy className="h-3 w-3" />
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2 text-destructive/60 hover:text-destructive"
                    onClick={() => setDeleteTarget(def.id)}
                  >
                    <Trash2 className="h-3 w-3" />
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => navigate(`/workflows/builder/${def.id}`)}
                  >
                    <Pencil className="h-3 w-3 mr-1" />
                    Edit
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {duplicateTarget && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="border-border/40 bg-card w-80">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <p className="text-sm font-medium">Duplicate Workflow</p>
              <p className="text-xs text-muted-foreground">Coming soon</p>
              <div className="flex justify-end">
                <Button size="sm" variant="outline" onClick={() => setDuplicateTarget(null)}>Close</Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}

      {deleteTarget && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="border-border/40 bg-card w-80">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <p className="text-sm font-medium">Delete Workflow</p>
              <p className="text-xs text-muted-foreground">Are you sure you want to delete <span className="font-mono">{deleteTarget}</span>? This cannot be undone.</p>
              {deleteError && (
                <Alert variant="destructive" role="alert">
                  <AlertDescription>{deleteError}</AlertDescription>
                </Alert>
              )}
              <div className="flex justify-end gap-2">
                <Button size="sm" variant="outline" onClick={() => { setDeleteTarget(null); setDeleteError(null); }}>Cancel</Button>
                <Button size="sm" variant="destructive" onClick={async () => {
                  const { error: err } = await deleteDef({ id: deleteTarget });
                  if (err) {
                    setDeleteError(err.message);
                  } else {
                    setDeleteTarget(null);
                    setDeleteError(null);
                    reexecute();
                  }
                }}>Delete</Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}

export function WorkflowBuilderNewPage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const templateId = searchParams.get("template");

  if (templateId && TEMPLATES[templateId]) {
    const template = TEMPLATES[templateId];
    const initial: WorkflowDef = {
      id: "",
      name: template.name,
      description: template.description,
      phases: template.phases.map((id) => makePhaseEntry(id)),
      variables: [],
      postSuccess: makePostSuccess(),
    };
    return <EditorCore initial={initial} isNew />;
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Link to="/workflows/builder" className="text-muted-foreground hover:text-foreground transition-colors">
          <ArrowLeft className="h-4 w-4" />
        </Link>
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">New Workflow</h1>
          <p className="text-sm text-muted-foreground">Choose a starting template</p>
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        <button
          type="button"
          onClick={() => navigate("/workflows/builder/new?template=standard")}
          className="text-left"
        >
          <Card className="border-border/40 bg-card/60 hover:border-primary/40 hover:bg-primary/5 transition-colors h-full">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <div className="h-10 w-10 rounded-lg bg-primary/10 flex items-center justify-center">
                <Layers className="h-5 w-5 text-primary/70" />
              </div>
              <div>
                <p className="font-medium">Standard</p>
                <p className="text-sm text-muted-foreground mt-1">{TEMPLATES.standard.description}</p>
              </div>
              <div className="flex items-center gap-1">
                {TEMPLATES.standard.phases.map((id, i) => (
                  <div key={id} className="flex items-center gap-1">
                    <span className="h-2 w-2 rounded-full bg-primary/50" />
                    {i < TEMPLATES.standard.phases.length - 1 && <span className="w-3 h-px bg-border" />}
                  </div>
                ))}
              </div>
              <p className="text-xs text-muted-foreground font-mono">
                {TEMPLATES.standard.phases.join(" \u2192 ")}
              </p>
              <Button size="sm" variant="outline" className="w-full pointer-events-none">Use Template</Button>
            </CardContent>
          </Card>
        </button>

        <button
          type="button"
          onClick={() => navigate("/workflows/builder/new?template=ui-ux")}
          className="text-left"
        >
          <Card className="border-border/40 bg-card/60 hover:border-primary/40 hover:bg-primary/5 transition-colors h-full">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <div className="h-10 w-10 rounded-lg bg-primary/10 flex items-center justify-center">
                <PaintBucket className="h-5 w-5 text-primary/70" />
              </div>
              <div>
                <p className="font-medium">UI/UX</p>
                <p className="text-sm text-muted-foreground mt-1">{TEMPLATES["ui-ux"].description}</p>
              </div>
              <div className="flex items-center gap-1">
                {TEMPLATES["ui-ux"].phases.map((id, i) => (
                  <div key={id} className="flex items-center gap-1">
                    <span className="h-2 w-2 rounded-full bg-primary/50" />
                    {i < TEMPLATES["ui-ux"].phases.length - 1 && <span className="w-3 h-px bg-border" />}
                  </div>
                ))}
              </div>
              <p className="text-xs text-muted-foreground font-mono">
                {TEMPLATES["ui-ux"].phases.join(" \u2192 ")}
              </p>
              <Button size="sm" variant="outline" className="w-full pointer-events-none">Use Template</Button>
            </CardContent>
          </Card>
        </button>

        <button
          type="button"
          onClick={() => navigate("/workflows/builder/new?template=blank")}
          className="text-left"
        >
          <Card className="border-border/40 bg-card/60 hover:border-primary/40 hover:bg-primary/5 transition-colors h-full">
            <CardContent className="pt-4 pb-4 px-4 space-y-3">
              <div className="h-10 w-10 rounded-lg bg-muted/30 flex items-center justify-center">
                <FileText className="h-5 w-5 text-muted-foreground/70" />
              </div>
              <div>
                <p className="font-medium">Blank</p>
                <p className="text-sm text-muted-foreground mt-1">{TEMPLATES.blank.description}</p>
              </div>
              <div className="h-2" />
              <p className="text-xs text-muted-foreground">No phases — start from scratch</p>
              <Button size="sm" variant="outline" className="w-full pointer-events-none">Start Blank</Button>
            </CardContent>
          </Card>
        </button>
      </div>
    </div>
  );
}

export function WorkflowBuilderEditPage() {
  const { definitionId } = useParams<{ definitionId: string }>();

  const [result] = useQuery({
    query: WorkflowDefinitionsDocument,
  });

  const { data, fetching, error } = result;
  if (fetching) return <PageLoading />;
  if (error) return <PageError message={error.message} />;

  const definitions = data?.workflowDefinitions ?? [];
  const found = definitions.find((d) => d.id === definitionId);
  if (!found) return <PageError message={`Workflow definition "${definitionId}" not found.`} />;

  const initial: WorkflowDef = {
    id: found.id,
    name: found.name,
    description: found.description ?? "",
    phases: found.phases.map((id) => makePhaseEntry(id)),
    variables: [],
    postSuccess: makePostSuccess(),
  };

  return <EditorCore key={definitionId} initial={initial} isNew={false} />;
}
