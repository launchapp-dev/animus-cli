import { useMemo, useState } from "react";

type ResolutionMode = "config" | "fallback-missing-config" | "invalid-config";

type ResolutionScenario = {
  mode: ResolutionMode;
  requestedPipelineId: string;
  resolvedPipelineId: string;
  configPath: string;
  sourceLabel: string;
  phases: string[];
  status: "started" | "started-with-fallback" | "failed";
  error?: {
    code: string;
    message: string;
    exitCode: number;
  };
};

type PhaseRow = {
  index: number;
  id: string;
};

const CONFIG_PATH = "/repo/.ao/state/workflow-config.v2.json";

const SCENARIOS: Record<ResolutionMode, ResolutionScenario> = {
  config: {
    mode: "config",
    requestedPipelineId: "ui-ux-standard",
    resolvedPipelineId: "ui-ux-standard",
    configPath: CONFIG_PATH,
    sourceLabel: "config:/repo/.ao/state/workflow-config.v2.json",
    status: "started",
    phases: [
      "requirements",
      "ux-research",
      "wireframe",
      "mockup-review",
      "accessibility-audit",
      "implementation",
      "code-review",
      "testing",
    ],
  },
  "fallback-missing-config": {
    mode: "fallback-missing-config",
    requestedPipelineId: "standard",
    resolvedPipelineId: "standard",
    configPath: CONFIG_PATH,
    sourceLabel: "fallback-hardcoded:phase_plan.rs",
    status: "started-with-fallback",
    phases: ["requirements", "implementation", "code-review", "testing"],
  },
  "invalid-config": {
    mode: "invalid-config",
    requestedPipelineId: "ui-ux-standard",
    resolvedPipelineId: "ui-ux-standard",
    configPath: CONFIG_PATH,
    sourceLabel: "invalid-config",
    status: "failed",
    phases: [],
    error: {
      code: "invalid_input",
      message:
        "invalid workflow config at /repo/.ao/state/workflow-config.v2.json: pipeline 'ui-ux-standard' phase 'wireframex' is missing from phase_catalog",
      exitCode: 2,
    },
  },
};

function toOrderedPhaseRows(phases: string[]): PhaseRow[] {
  return phases.map((id, index) => ({ index: index + 1, id }));
}

function toEnvelope(scenario: ResolutionScenario): string {
  if (scenario.mode !== "invalid-config" || !scenario.error) {
    return JSON.stringify(
      {
        schema: "ao.cli.v1",
        ok: true,
        data: {
          status: scenario.status,
          pipeline_id: scenario.resolvedPipelineId,
          phase_source: scenario.sourceLabel,
          phases: scenario.phases,
        },
      },
      null,
      2,
    );
  }

  return JSON.stringify(
    {
      schema: "ao.cli.v1",
      ok: false,
      error: {
        code: scenario.error.code,
        message: scenario.error.message,
        exit_code: scenario.error.exitCode,
      },
    },
    null,
    2,
  );
}

function toRunTranscript(scenario: ResolutionScenario): string {
  if (scenario.mode === "invalid-config" && scenario.error) {
    return [
      "status: failed",
      `pipeline_id: ${scenario.requestedPipelineId}`,
      `phase_source: ${scenario.sourceLabel}`,
      "",
      `error_code: ${scenario.error.code}`,
      `error: ${scenario.error.message}`,
      "next_step: fix workflow config and rerun",
    ].join("\n");
  }

  const rows = toOrderedPhaseRows(scenario.phases)
    .map((row) => `${row.index}. ${row.id}`)
    .join("\n");

  const fallbackReason =
    scenario.mode === "fallback-missing-config"
      ? `reason: missing workflow config at ${scenario.configPath}\n`
      : "";

  return [
    `status: ${scenario.status}`,
    `pipeline_id: ${scenario.resolvedPipelineId}`,
    `phase_source: ${scenario.sourceLabel}`,
    fallbackReason.trimEnd(),
    "",
    "phases:",
    rows,
    "",
    "next_step: ao workflow get --id <workflow_id> --json",
  ]
    .filter(Boolean)
    .join("\n");
}

function parityStatus(scenario: ResolutionScenario): "match" | "not-applicable" {
  if (scenario.mode === "invalid-config") {
    return "not-applicable";
  }
  return "match";
}

function currentPhase(scenario: ResolutionScenario): string {
  return scenario.phases[0] ?? "none";
}

function nextPhase(scenario: ResolutionScenario): string {
  return scenario.phases[1] ?? "none";
}

function statusTone(mode: ResolutionMode): "ok" | "warn" | "error" {
  if (mode === "config") {
    return "ok";
  }
  if (mode === "fallback-missing-config") {
    return "warn";
  }
  return "error";
}

export function PhasePlanConfigWireframe() {
  const [mode, setMode] = useState<ResolutionMode>("config");
  const scenario = useMemo(() => SCENARIOS[mode], [mode]);
  const parity = parityStatus(scenario);

  return (
    <section aria-label="TASK-053 config-first phase plan wireframe">
      <h1>Config-First Phase Plan Resolution</h1>
      <p>
        Wireframe-only simulation for task handoff. It models deterministic
        source visibility for config, fallback, and invalid-config paths.
      </p>

      <nav aria-label="Resolution scenario">
        <button type="button" onClick={() => setMode("config")}>
          Config source
        </button>
        <button type="button" onClick={() => setMode("fallback-missing-config")}>
          Missing config fallback
        </button>
        <button type="button" onClick={() => setMode("invalid-config")}>
          Invalid config
        </button>
      </nav>

      <WorkflowRunSurfaceWireframe scenario={scenario} />
      <PlanningParityWireframe scenario={scenario} parity={parity} />
      <InspectionSurfaceWireframe scenario={scenario} />
      <ErrorSurfaceWireframe scenario={scenario} />

      <section aria-label="JSON envelope preview">
        <h2>Envelope Preview</h2>
        <p>
          Stable machine-readable envelope shape for automation and deterministic
          recovery handling.
        </p>
        <pre data-tone={statusTone(scenario.mode)}>{toEnvelope(scenario)}</pre>
      </section>

      <section aria-label="Acceptance traceability">
        <h2>Acceptance Traceability</h2>
        <ul>
          <li>AC-01: config scenario renders configured phase order.</li>
          <li>AC-02: planning parity card checks run and execute alignment.</li>
          <li>AC-03: fallback scenario remains explicit and deterministic.</li>
          <li>AC-04: invalid-config scenario returns actionable error, no fallback.</li>
          <li>AC-05: config-only phase addition appears in rendered phase list.</li>
          <li>AC-06: output formatting is deterministic and test-ready.</li>
        </ul>
      </section>
    </section>
  );
}

function WorkflowRunSurfaceWireframe(props: { scenario: ResolutionScenario }) {
  return (
    <section aria-label="Workflow run wireframe">
      <h2>Workflow Run Surface</h2>
      <p>
        Request: <code>ao workflow run --task-id TASK-053 --pipeline-id {props.scenario.requestedPipelineId} --json</code>
      </p>
      <dl>
        <div>
          <dt>status</dt>
          <dd>{props.scenario.status}</dd>
        </div>
        <div>
          <dt>source</dt>
          <dd>{props.scenario.sourceLabel}</dd>
        </div>
        <div>
          <dt>pipeline</dt>
          <dd>{props.scenario.resolvedPipelineId}</dd>
        </div>
      </dl>
      <pre data-tone={statusTone(props.scenario.mode)}>{toRunTranscript(props.scenario)}</pre>
    </section>
  );
}

function PlanningParityWireframe(props: {
  scenario: ResolutionScenario;
  parity: "match" | "not-applicable";
}) {
  const planningRows = props.scenario.phases;
  const directRows = props.scenario.phases;

  return (
    <section aria-label="Planning execute parity wireframe">
      <h2>Planning Execute Parity</h2>
      <p>
        Ensures direct workflow starts and planning-triggered starts resolve
        phase plans through the same contract.
      </p>

      <dl>
        <div>
          <dt>parity</dt>
          <dd>{props.parity}</dd>
        </div>
        <div>
          <dt>source</dt>
          <dd>{props.scenario.sourceLabel}</dd>
        </div>
      </dl>

      <div>
        <h3>Direct workflow run phases</h3>
        <ol>
          {directRows.map((phase) => (
            <li key={`direct-${phase}`}>{phase}</li>
          ))}
        </ol>
      </div>

      <div>
        <h3>Planning execute phases</h3>
        <ol>
          {planningRows.map((phase) => (
            <li key={`planning-${phase}`}>{phase}</li>
          ))}
        </ol>
      </div>
    </section>
  );
}

function InspectionSurfaceWireframe(props: { scenario: ResolutionScenario }) {
  return (
    <section aria-label="Workflow inspection wireframe">
      <h2>Workflow Inspection Surface</h2>
      <dl>
        <div>
          <dt>config path</dt>
          <dd>{props.scenario.configPath}</dd>
        </div>
        <div>
          <dt>current phase</dt>
          <dd>{currentPhase(props.scenario)}</dd>
        </div>
        <div>
          <dt>next phase</dt>
          <dd>{nextPhase(props.scenario)}</dd>
        </div>
      </dl>

      <ol>
        {toOrderedPhaseRows(props.scenario.phases).map((row) => (
          <li key={`inspection-${row.id}`}>
            {row.index}. {row.id}
          </li>
        ))}
      </ol>
    </section>
  );
}

function ErrorSurfaceWireframe(props: { scenario: ResolutionScenario }) {
  if (props.scenario.mode !== "invalid-config" || !props.scenario.error) {
    return (
      <section aria-label="Misconfiguration error wireframe">
        <h2>Misconfiguration Error Surface</h2>
        <p>No validation error in this scenario.</p>
      </section>
    );
  }

  return (
    <section aria-label="Misconfiguration error wireframe">
      <h2>Misconfiguration Error Surface</h2>
      <p role="alert">
        {props.scenario.error.code}: {props.scenario.error.message}
      </p>
      <ol>
        <li>Fix phase id in {props.scenario.configPath}.</li>
        <li>Run ao workflow config validate --json.</li>
        <li>Rerun ao workflow run --task-id TASK-053 --pipeline-id ui-ux-standard --json.</li>
      </ol>
    </section>
  );
}
