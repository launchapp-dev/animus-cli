import { useMemo, useState, type ReactNode } from "react";

type SurfaceId =
  | "root-help"
  | "group-help"
  | "group-audit"
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
  target: {
    repo: string;
    worktree_name: string;
  };
  action: string;
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
  { id: "group-audit", label: "Scoped group audit", acceptance: ["AC-01", "AC-02"] },
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
  "action",
  "destructive",
  "dry_run",
  "requires_confirmation",
  "planned_effects",
  "next_step",
] as const;

const ROOT_HELP_LINES = [
  "ao - Agent Orchestrator CLI",
  "",
  "Purpose:",
  "  Coordinate daemon, project, task, workflow, review, and QA operations.",
  "",
  "Usage:",
  "  ao [OPTIONS] <COMMAND>",
  "",
  "Options:",
  "  --project-root <PATH>    Project root directory; overrides PROJECT_ROOT and default root resolution",
  "  --json                   Emit machine-readable output using ao.cli.v1 envelope",
  "  -h, --help               Print help",
  "",
  "Core command groups:",
  "  daemon          Manage daemon lifecycle and automation settings",
  "  agent           Run and inspect agent executions",
  "  project         Manage project registration and metadata",
  "  task            Manage tasks, dependencies, and status",
  "  task-control    Apply task pause/resume/cancel operational controls",
  "  workflow        Run and control workflow execution",
  "  requirements    Draft and manage project requirements",
  "  git             Manage git repos, worktrees, and confirmations",
].join("\n");

const GROUP_HELP_LINES = [
  "task - Manage tasks, dependencies, and status",
  "",
  "Usage:",
  "  ao task <COMMAND>",
  "",
  "Commands:",
  "  list                 List tasks with optional filters",
  "  prioritized          List tasks sorted by priority and urgency",
  "  next                 Get the next ready task",
  "  get                  Get a task by id",
  "  create               Create a task",
  "  update               Update title, description, status, priority, and links",
  "  delete               Remove a task (destructive; confirmation required)",
  "  assign-agent         Assign an agent role to a task",
  "  dependency-add       Add a dependency edge",
  "  dependency-remove    Remove a dependency edge",
  "  status               Set task status",
  "",
  "Next step:",
  "  Run 'ao task update --help' for accepted values and --input-json precedence.",
].join("\n");

const TASK_CONTROL_HELP_LINES = [
  "task-control - Apply operational controls to tasks",
  "",
  "Usage:",
  "  ao task-control <COMMAND>",
  "",
  "Commands:",
  "  pause               Pause task execution",
  "  resume              Resume paused task execution",
  "  cancel              Cancel a task (destructive; --dry-run and --confirm available)",
  "  set-priority        Set task priority: critical, high, medium, low",
  "  set-deadline        Set task deadline using RFC3339 timestamp format",
].join("\n");

const WORKFLOW_HELP_LINES = [
  "workflow - Run and control workflow execution",
  "",
  "Usage:",
  "  ao workflow <COMMAND>",
  "",
  "Commands:",
  "  run                 Start a workflow for a task",
  "  pause               Pause an active workflow (destructive; --dry-run and --confirm available)",
  "  cancel              Cancel a workflow (destructive; --dry-run and --confirm available)",
  "  phase               Manual actions for one workflow phase",
  "  phases              Manage workflow phase definitions (remove is destructive)",
  "  checkpoints         List and inspect workflow checkpoints",
].join("\n");

const REQUIREMENTS_HELP_LINES = [
  "requirements - Draft and manage project requirements",
  "",
  "Usage:",
  "  ao requirements <COMMAND>",
  "",
  "Commands:",
  "  draft               Draft requirements from project context",
  "  list                List requirements",
  "  get                 Get a requirement by id",
  "  refine              Refine existing requirements",
  "  create              Create a requirement",
  "  update              Update requirement fields",
  "  mockups             Manage requirement mockups and linked assets",
  "",
  "Accepted values (requirements update):",
  "  --status: draft|refined|planned|in-progress|in_progress|done",
  "  --priority: must|should|could|wont|won't",
].join("\n");

const GIT_HELP_LINES = [
  "git - Manage Git repositories, worktrees, and confirmations",
  "",
  "Usage:",
  "  ao git <COMMAND>",
  "",
  "Commands:",
  "  repo               Manage repo registry entries",
  "  push               Push branch updates (--force requires --confirmation-id)",
  "  worktree           Manage repository worktrees (remove supports --dry-run)",
  "  confirm            Request/respond/outcome for confirmation workflows",
].join("\n");

const COMMAND_HELP_LINES = [
  "update - Update a task using explicit flags or --input-json",
  "",
  "Usage:",
  "  ao task update --id <TASK_ID> [OPTIONS]",
  "",
  "Required:",
  "  --id <TASK_ID>                        Task identifier (for example: TASK-002)",
  "",
  "Options:",
  "  --title <TITLE>                       Updated task title",
  "  --description <TEXT>                  Updated task description",
  "  --status <STATUS>                     Task status: backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled",
  "  --priority <PRIORITY>                 Task priority: critical|high|medium|low",
  "  --assignee <ASSIGNEE>                 Updated assignee value",
  "  --linked-architecture-entity <ENTITY_ID>  Architecture entity id to link (repeatable)",
  "  --replace-linked-architecture-entities     Replace existing linked architecture entities",
  "  --input-json <JSON>                   JSON payload; when set, JSON values override individual field flags",
  "  --json                                Emit ao.cli.v1 envelope",
  "  -h, --help                            Print help",
  "",
  "Examples:",
  "  ao task update --id TASK-002 --status in-progress --priority high",
  "  ao task update --id TASK-002 --input-json '{\"status\":\"in-progress\",\"priority\":\"high\"}' --json",
].join("\n");

const DESTRUCTIVE_PREVIEW: DryRunPreview = {
  operation: "git.worktree.remove",
  target: {
    repo: "ao-cli",
    worktree_name: "task-task-002",
  },
  action: "git.worktree.remove",
  destructive: true,
  dry_run: true,
  requires_confirmation: true,
  planned_effects: [
    "remove git worktree from repository",
  ],
  next_step: "request/approve a git confirmation, then rerun with --confirmation-id <id>",
};

export function formatInvalidValueError(
  domain: ValidationDomain,
  invalidValue: string,
): string {
  const accepted = ACCEPTED_VALUES[domain].join(", ");
  return `invalid ${domain} '${invalidValue}'; expected one of: ${accepted}; run the same command with --help`;
}

export function formatConfirmationRequired(
  command: string,
  token: string,
): string {
  return `CONFIRMATION_REQUIRED: rerun '${command}' with --confirm ${token}; use --dry-run to preview changes`;
}

export function formatGitConfirmationRequired(operationType: string, repoName: string): string {
  return `CONFIRMATION_REQUIRED: request and approve a git confirmation for '${operationType}' on '${repoName}', then rerun with --confirmation-id <id>; use --dry-run to preview changes`;
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
    () => formatInvalidValueError("status", "paused"),
    [],
  );
  const invalidRequirementStatusError = useMemo(
    () => formatInvalidValueError("requirement-status", "waiting"),
    [],
  );
  const workflowConfirmationMessage = useMemo(
    () => formatConfirmationRequired("ao workflow cancel --id WF-42", "WF-42"),
    [],
  );
  const gitConfirmationMessage = useMemo(
    () => formatGitConfirmationRequired("remove_worktree", "ao-cli"),
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
    case "group-audit":
      return (
        <>
          <TerminalBlock command="ao task-control --help" output={TASK_CONTROL_HELP_LINES} />
          <TerminalBlock command="ao workflow --help" output={WORKFLOW_HELP_LINES} />
          <TerminalBlock command="ao requirements --help" output={REQUIREMENTS_HELP_LINES} />
          <TerminalBlock command="ao git --help" output={GIT_HELP_LINES} />
        </>
      );
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
