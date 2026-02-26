import { useMemo, useState, type ReactNode } from "react";

type SurfaceId =
  | "root-help"
  | "group-help"
  | "command-help"
  | "validation"
  | "destructive"
  | "json-parity";

type TraceabilityId =
  | "AC-01"
  | "AC-02"
  | "AC-03"
  | "AC-04"
  | "AC-05"
  | "AC-06"
  | "AC-07"
  | "AC-08"
  | "AC-09"
  | "AC-10"
  | "AC-11"
  | "AC-12";

type ValidationDomain =
  | "status"
  | "priority"
  | "type"
  | "requirement-status"
  | "requirement-priority";

type DryRunPreview = {
  operation: string;
  target: string;
  destructive: boolean;
  dry_run: boolean;
  requires_confirmation: boolean;
  planned_effects: string[];
  next_step: string;
};

type CliErrorEnvelope = {
  schema: "ao.cli.v1";
  ok: false;
  error: {
    code: string;
    message: string;
    exit_code: number;
  };
};

type CliSuccessEnvelope<TData> = {
  schema: "ao.cli.v1";
  ok: true;
  data: TData;
};

type SurfaceDescriptor = {
  id: SurfaceId;
  label: string;
  acceptance: TraceabilityId[];
};

const SURFACES: SurfaceDescriptor[] = [
  { id: "root-help", label: "Root help", acceptance: ["AC-01", "AC-10", "AC-12"] },
  { id: "group-help", label: "Group help", acceptance: ["AC-01", "AC-10"] },
  { id: "command-help", label: "Command help", acceptance: ["AC-02", "AC-03", "AC-10"] },
  { id: "validation", label: "Validation", acceptance: ["AC-04", "AC-11", "AC-12"] },
  { id: "destructive", label: "Destructive safety", acceptance: ["AC-05", "AC-06", "AC-09"] },
  { id: "json-parity", label: "JSON parity", acceptance: ["AC-07", "AC-08"] },
];

export const ACCEPTED_VALUES: Record<ValidationDomain, string[]> = {
  status: [
    "backlog",
    "todo",
    "ready",
    "in-progress",
    "in_progress",
    "blocked",
    "on-hold",
    "on_hold",
    "done",
    "cancelled",
  ],
  priority: ["critical", "high", "medium", "low"],
  type: ["feature", "bugfix", "hotfix", "refactor", "docs", "test", "chore", "experiment"],
  "requirement-status": ["draft", "refined", "planned", "in-progress", "in_progress", "done"],
  "requirement-priority": ["must", "should", "could", "wont", "won't"],
};

export const SHARED_DRY_RUN_KEYS = [
  "operation",
  "target",
  "destructive",
  "dry_run",
  "requires_confirmation",
  "planned_effects",
  "next_step",
] as const;

const ROOT_HELP_LINES = [
  "ao - Agent Orchestrator control plane CLI",
  "",
  "Purpose:",
  "  Coordinate daemon, project, task, workflow, review, and QA operations.",
  "",
  "Usage:",
  "  ao [OPTIONS] <COMMAND>",
  "",
  "Options:",
  "  --project-root <PATH>    Resolve AO state root for this invocation",
  "  --json                   Emit ao.cli.v1 machine envelope",
  "  -h, --help               Print help",
  "",
  "Core command groups:",
  "  daemon          Manage standalone daemon lifecycle and telemetry",
  "  task            Create, update, and track project tasks",
  "  task-control    Apply task pause/resume/cancel operational controls",
  "  workflow        Run and control workflow phases",
  "  requirements    Draft and refine requirements",
  "  git             Safe repository and worktree operations",
].join("\n");

const GROUP_HELP_LINES = [
  "task - Create, mutate, and inspect AO tasks",
  "",
  "Usage:",
  "  ao task <COMMAND>",
  "",
  "Commands:",
  "  list                 List tasks with optional status filters",
  "  prioritized          Show priority-ranked task queue",
  "  get                  Show full task details by id",
  "  create               Create a new task linked to requirement context",
  "  update               Update title, description, status, type, or deadline",
  "  delete               Remove a task (destructive; confirmation required)",
  "",
  "Next step:",
  "  Run 'ao task update --help' to inspect accepted values and input precedence.",
].join("\n");

const COMMAND_HELP_LINES = [
  "update - Update an existing task using explicit flags or --input-json",
  "",
  "Usage:",
  "  ao task update --id <TASK_ID> [OPTIONS]",
  "",
  "Required:",
  "  --id <TASK_ID>         Task identifier (for example: TASK-002)",
  "",
  "Options:",
  "  --status <STATUS>      Accepted values: backlog|todo, ready, in-progress|in_progress, blocked, on-hold|on_hold, done, cancelled",
  "  --priority <PRIORITY>  Accepted values: critical, high, medium, low",
  "  --type <TYPE>          Accepted values: feature, bugfix, hotfix, refactor, docs, test, chore, experiment",
  "  --deadline <YYYY-MM-DD> Deadline in ISO date format",
  "  --input-json <PATH>    JSON values take precedence over individual flags when present",
  "",
  "Examples:",
  "  ao task update --id TASK-002 --status in-progress --priority high",
  "  ao task update --id TASK-002 --input-json ./task-update.json --json",
].join("\n");

const DESTRUCTIVE_PREVIEW: DryRunPreview = {
  operation: "git.worktree.remove",
  target: "task-task-002",
  destructive: true,
  dry_run: true,
  requires_confirmation: true,
  planned_effects: [
    "validate worktree path exists",
    "verify branch and worktree mapping",
    "remove worktree and branch on confirmed rerun",
  ],
  next_step: "rerun with --confirmation-id CONF-7F3A",
};

export function formatInvalidValueError(
  domain: ValidationDomain,
  invalidValue: string,
  helpCommand: string,
): string {
  const accepted = ACCEPTED_VALUES[domain].join(", ");
  return `invalid ${domain} '${invalidValue}'; expected one of: ${accepted}; run '${helpCommand} --help'`;
}

export function formatConfirmationRequired(
  command: string,
  confirmationFlag: "--confirm" | "--confirmation-id",
  token: string,
): string {
  return `CONFIRMATION_REQUIRED: rerun '${command}' with ${confirmationFlag} ${token}; use --dry-run to preview changes`;
}

export const traceability: Record<TraceabilityId, string[]> = {
  "AC-01": ["Root and scoped group help expose intent first."],
  "AC-02": ["Command help includes argument format and accepted values guidance."],
  "AC-03": ["Input precedence for --input-json is explicit and stable."],
  "AC-04": ["Invalid-value errors include domain, value, accepted list, and rerun hint."],
  "AC-05": ["Confirmation-required messaging uses canonical token ordering."],
  "AC-06": ["Dry-run preview exposes shared top-level key contract."],
  "AC-07": ["JSON output retains ao.cli.v1 envelope semantics."],
  "AC-08": ["Exit-code mapping remains visible and deterministic in error mode."],
  "AC-09": ["Destructive flow requires explicit confirmation after dry-run preview."],
  "AC-10": ["Wireframe strings are deterministic and ready for help/error regression assertions."],
  "AC-11": ["Canonical token order is centralized in formatter helpers."],
  "AC-12": ["Static message templates remain free of environment-dependent text."],
};

export function CliHelpErrorWireframeApp(): ReactNode {
  const [activeSurface, setActiveSurface] = useState<SurfaceId>("root-help");

  const invalidStatusError = useMemo(
    () => formatInvalidValueError("status", "paused", "ao task update"),
    [],
  );
  const invalidRequirementStatusError = useMemo(
    () =>
      formatInvalidValueError(
        "requirement-status",
        "waiting",
        "ao requirements update",
      ),
    [],
  );
  const workflowConfirmationMessage = useMemo(
    () =>
      formatConfirmationRequired(
        "ao workflow cancel --id WF-42",
        "--confirm",
        "WF-42",
      ),
    [],
  );
  const gitConfirmationMessage = useMemo(
    () =>
      formatConfirmationRequired(
        "ao git worktree remove --repo ao-cli --worktree-name task-task-002",
        "--confirmation-id",
        "CONF-7F3A",
      ),
    [],
  );

  const errorEnvelope: CliErrorEnvelope = useMemo(
    () => ({
      schema: "ao.cli.v1",
      ok: false,
      error: {
        code: "invalid_input",
        message: invalidStatusError,
        exit_code: 2,
      },
    }),
    [invalidStatusError],
  );

  const successEnvelope: CliSuccessEnvelope<DryRunPreview> = useMemo(
    () => ({
      schema: "ao.cli.v1",
      ok: true,
      data: DESTRUCTIVE_PREVIEW,
    }),
    [],
  );

  return (
    <section aria-label="CLI help and error wireframe">
      <header>
        <h1>TASK-002 CLI Help and Error Wireframe</h1>
        <p>
          Implementation scaffold for deterministic help messaging, invalid-value recovery, and
          destructive confirmation UX.
        </p>
      </header>

      <nav aria-label="Wireframe surfaces">
        {SURFACES.map((surface) => (
          <button
            type="button"
            key={surface.id}
            onClick={() => setActiveSurface(surface.id)}
            aria-current={activeSurface === surface.id ? "page" : undefined}
          >
            {surface.label}
          </button>
        ))}
      </nav>

      <SurfaceBoundary title={surfaceLabel(activeSurface)} acceptance={surfaceAcceptance(activeSurface)}>
        {renderSurface(activeSurface, {
          invalidStatusError,
          invalidRequirementStatusError,
          workflowConfirmationMessage,
          gitConfirmationMessage,
          errorEnvelope,
          successEnvelope,
        })}
      </SurfaceBoundary>
    </section>
  );
}

function surfaceLabel(surface: SurfaceId): string {
  return SURFACES.find((item) => item.id === surface)?.label ?? surface;
}

function surfaceAcceptance(surface: SurfaceId): TraceabilityId[] {
  return SURFACES.find((item) => item.id === surface)?.acceptance ?? [];
}

function renderSurface(
  activeSurface: SurfaceId,
  input: {
    invalidStatusError: string;
    invalidRequirementStatusError: string;
    workflowConfirmationMessage: string;
    gitConfirmationMessage: string;
    errorEnvelope: CliErrorEnvelope;
    successEnvelope: CliSuccessEnvelope<DryRunPreview>;
  },
): ReactNode {
  switch (activeSurface) {
    case "root-help":
      return <TerminalBlock command="ao --help" output={ROOT_HELP_LINES} />;
    case "group-help":
      return <TerminalBlock command="ao task --help" output={GROUP_HELP_LINES} />;
    case "command-help":
      return <TerminalBlock command="ao task update --help" output={COMMAND_HELP_LINES} />;
    case "validation":
      return (
        <>
          <TerminalBlock
            command="ao task update --id TASK-002 --status paused"
            output={input.invalidStatusError}
          />
          <TerminalBlock
            command="ao requirements update --id REQ-014 --status waiting"
            output={input.invalidRequirementStatusError}
          />
          <p>Suggested rerun: ao task update --id TASK-002 --status in-progress</p>
        </>
      );
    case "destructive":
      return (
        <>
          <TerminalBlock
            command="ao workflow cancel --id WF-42"
            output={input.workflowConfirmationMessage}
          />
          <TerminalBlock
            command="ao git worktree remove --repo ao-cli --worktree-name task-task-002"
            output={input.gitConfirmationMessage}
          />
          <JsonBlock value={input.successEnvelope.data} />
        </>
      );
    case "json-parity":
      return (
        <>
          <JsonBlock value={input.errorEnvelope} />
          <JsonBlock value={input.successEnvelope} />
        </>
      );
    default:
      return null;
  }
}

function SurfaceBoundary(props: {
  title: string;
  acceptance: TraceabilityId[];
  children: ReactNode;
}): ReactNode {
  return (
    <article aria-label={props.title}>
      <h2>{props.title}</h2>
      <p>Acceptance trace: {props.acceptance.join(", ")}</p>
      {props.children}
    </article>
  );
}

function TerminalBlock(props: { command: string; output: string }): ReactNode {
  return (
    <section aria-label={`Terminal output for ${props.command}`}>
      <h3>$ {props.command}</h3>
      <pre>{props.output}</pre>
    </section>
  );
}

function JsonBlock(props: { value: unknown }): ReactNode {
  const formatted = useMemo(() => JSON.stringify(props.value, null, 2), [props.value]);
  return (
    <section aria-label="JSON output preview">
      <h3>JSON output</h3>
      <pre>{formatted}</pre>
    </section>
  );
}
