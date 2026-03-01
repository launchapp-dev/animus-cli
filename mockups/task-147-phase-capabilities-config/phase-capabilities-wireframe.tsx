type ModelGroup =
  | "ui_ux"
  | "research"
  | "review"
  | "requirements"
  | "testing"
  | "default";

type PhaseCapability = {
  phase_id: string;
  can_write: boolean;
  needs_commit: boolean;
  model_group: ModelGroup;
  parse_research_signal: boolean;
  safety_rules_template: string | null;
};

type DriftRow = {
  phase_id: string;
  in_model_routing_ui_ux: boolean;
  in_phase_targets_ui_ux: boolean;
  is_write_phase: boolean;
  in_model_routing_review: boolean;
  in_model_routing_testing: boolean;
  in_model_routing_requirements: boolean;
  in_model_routing_research: boolean;
};

const DRIFT_ROWS: DriftRow[] = [
  {
    phase_id: "wireframe",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: true,
    is_write_phase: true,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "design",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: true,
    is_write_phase: true,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "ux-research",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: true,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "mockup-review",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: true,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "ui-design",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "ux-design",
    in_model_routing_ui_ux: true,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "design-review",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: true,
    is_write_phase: false,
    in_model_routing_review: true,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "code-review",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: true,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "review",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: true,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "architecture",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: true,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "testing",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: true,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "test",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: true,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "qa",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: true,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
  {
    phase_id: "requirements",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: true,
    in_model_routing_research: false,
  },
  {
    phase_id: "research",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: false,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: true,
  },
  {
    phase_id: "implementation",
    in_model_routing_ui_ux: false,
    in_phase_targets_ui_ux: false,
    is_write_phase: true,
    in_model_routing_review: false,
    in_model_routing_testing: false,
    in_model_routing_requirements: false,
    in_model_routing_research: false,
  },
];

const PROPOSED_CAPABILITIES: PhaseCapability[] = [
  {
    phase_id: "wireframe",
    can_write: true,
    needs_commit: false,
    model_group: "ui_ux",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "design",
    can_write: true,
    needs_commit: false,
    model_group: "ui_ux",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "ux-research",
    can_write: false,
    needs_commit: false,
    model_group: "ui_ux",
    parse_research_signal: true,
    safety_rules_template: null,
  },
  {
    phase_id: "mockup-review",
    can_write: false,
    needs_commit: false,
    model_group: "ui_ux",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "research",
    can_write: false,
    needs_commit: false,
    model_group: "research",
    parse_research_signal: true,
    safety_rules_template: "research",
  },
  {
    phase_id: "code-review",
    can_write: false,
    needs_commit: false,
    model_group: "review",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "testing",
    can_write: false,
    needs_commit: false,
    model_group: "testing",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "requirements",
    can_write: true,
    needs_commit: false,
    model_group: "requirements",
    parse_research_signal: false,
    safety_rules_template: null,
  },
  {
    phase_id: "implementation",
    can_write: true,
    needs_commit: true,
    model_group: "default",
    parse_research_signal: false,
    safety_rules_template: null,
  },
];

function hasDrift(row: DriftRow): boolean {
  const uiUxAgreement =
    row.in_model_routing_ui_ux === row.in_phase_targets_ui_ux;
  return !uiUxAgreement;
}

function Mark({
  value,
  isDrift,
}: {
  value: boolean;
  isDrift?: boolean;
}): JSX.Element {
  if (isDrift) {
    return (
      <span
        className="drift-mark drift"
        aria-label="drift detected"
        title="Inconsistent across files"
      >
        ✗
      </span>
    );
  }
  return (
    <span
      className={`drift-mark ${value ? "yes" : "no"}`}
      aria-label={value ? "yes" : "no"}
    >
      {value ? "✓" : "–"}
    </span>
  );
}

function ModelGroupBadge({ group }: { group: ModelGroup }): JSX.Element {
  return (
    <span className={`model-group mg-${group}`} aria-label={`model group: ${group}`}>
      {group}
    </span>
  );
}

function BoolCell({ value }: { value: boolean }): JSX.Element {
  return (
    <code style={{ color: value ? "#0f6a47" : "#7a8fa0" }}>
      {value ? "true" : "false"}
    </code>
  );
}

function CapabilitySchemaBlock(): JSX.Element {
  return (
    <pre className="json-block" aria-label="proposed phase capabilities schema">
      <span className="comment">// agent-runtime-config.v2.json — phase entry (proposed)</span>{"\n"}
      {`{`}{"\n"}
      {"  "}<span className="key">"wireframe"</span>{`: {`}{"\n"}
      {"    "}<span className="key">"mode"</span>{`: `}<span className="val-s">"agent"</span>{`,`}{"\n"}
      {"    "}<span className="key">"agent_id"</span>{`: `}<span className="val-s">"default"</span>{`,`}{"\n"}
      {"    "}<span className="key">"directive"</span>{`: `}<span className="val-s">"Create UI mockups under mockups/..."</span>{`,`}{"\n"}
      {"    "}<span className="comment">// ← NEW: phase_capabilities replaces hardcoded is_*_phase() functions</span>{"\n"}
      {"    "}<span className="key">"phase_capabilities"</span>{`: {`}{"\n"}
      {"      "}<span className="key">"can_write"</span>{`: `}<span className="val-t">true</span>{`,`}{"\n"}
      {"      "}<span className="key">"needs_commit"</span>{`: `}<span className="val-f">false</span>{`,`}{"\n"}
      {"      "}<span className="key">"model_group"</span>{`: `}<span className="val-s">"ui_ux"</span>{`,`}{"\n"}
      {"      "}<span className="key">"parse_research_signal"</span>{`: `}<span className="val-f">false</span>{`,`}{"\n"}
      {"      "}<span className="key">"safety_rules_template"</span>{`: `}<span className="val-n">null</span>{"\n"}
      {"    }{,\n"}
      {"    "}<span className="comment">// remaining fields unchanged ...</span>{"\n"}
      {"    "}<span className="key">"runtime"</span>{`: `}<span className="val-n">null</span>{`,`}{"\n"}
      {"    "}<span className="key">"output_contract"</span>{`: `}<span className="val-n">null</span>{"\n"}
      {`  }`}{"\n"}
      {`}`}
    </pre>
  );
}

function RoutingFlowWireframe(): JSX.Element {
  return (
    <section aria-label="config-driven routing flow">
      <h2>Config-Driven Routing Flow (After)</h2>
      <p>
        Instead of branching on <code>phase_id</code> strings in three files,
        the daemon reads <code>phase_capabilities</code> from config at startup
        and dispatches on the capability fields.
      </p>

      <div className="flow" role="list" aria-label="routing steps">
        <div className="flow-step" role="listitem">
          <p className="step-label">1. Phase starts</p>
          <p className="step-value">phase_id = "wireframe"</p>
        </div>
        <div className="flow-step" role="listitem">
          <p className="step-label">2. Load config</p>
          <p className="step-value">agent-runtime-config.v2.json phases["wireframe"]</p>
        </div>
        <div className="flow-step highlight" role="listitem">
          <p className="step-label">3. Read capabilities</p>
          <p className="step-value">model_group = "ui_ux"<br />can_write = true</p>
        </div>
        <div className="flow-step" role="listitem">
          <p className="step-label">4. Route model</p>
          <p className="step-value">primary = "gemini-3.1-pro-preview"<br />(via model_group)</p>
        </div>
        <div className="flow-step" role="listitem">
          <p className="step-label">5. Set prompt rule</p>
          <p className="step-value">can_write → write action rule<br />needs_commit → false</p>
        </div>
      </div>

      <p className="inline-note" style={{ marginTop: 16 }}>
        <strong>Before:</strong> three separate <code>is_ui_ux_phase()</code>,{" "}
        <code>is_write_phase()</code>, and <code>is_research_phase()</code>{" "}
        functions with diverging string sets. <strong>After:</strong> one config
        read, one field lookup per decision axis.
      </p>
    </section>
  );
}

function InconsistencyTableWireframe(): JSX.Element {
  return (
    <section aria-label="phase membership inconsistency table">
      <h2>Phase Membership Drift (Before)</h2>
      <p>
        Each column represents a hardcoded match arm in a different file. Rows
        with a <span className="drift-mark drift" style={{ display: "inline-flex" }}>✗</span>{" "}
        mark indicate the same phase is classified differently between files —
        the core motivation for this refactor.
      </p>

      <div style={{ overflowX: "auto" }}>
        <table className="drift-table" aria-label="drift between three hardcoded sets">
          <thead>
            <tr>
              <th scope="col">phase_id</th>
              <th scope="col" title="protocol/src/model_routing.rs: is_ui_ux_phase()">
                model_routing<br />is_ui_ux
              </th>
              <th scope="col" title="daemon_scheduler_phase_targets.rs: is_ui_ux_phase()">
                phase_targets<br />is_ui_ux
              </th>
              <th scope="col" title="phase_executor.rs line 202-203">
                executor<br />is_write
              </th>
              <th scope="col" title="model_routing.rs: is_review_phase()">
                routing<br />is_review
              </th>
              <th scope="col" title="model_routing.rs: is_testing_phase()">
                routing<br />is_testing
              </th>
              <th scope="col" title="model_routing.rs: is_requirements_phase()">
                routing<br />is_req.
              </th>
              <th scope="col" title="Both files: is_research_phase()">
                routing<br />is_research
              </th>
            </tr>
          </thead>
          <tbody>
            {DRIFT_ROWS.map((row) => {
              const uiUxDrift =
                row.in_model_routing_ui_ux !== row.in_phase_targets_ui_ux;
              return (
                <tr key={row.phase_id} aria-label={`phase ${row.phase_id}`}>
                  <td>{row.phase_id}</td>
                  <td>
                    <Mark value={row.in_model_routing_ui_ux} isDrift={uiUxDrift && row.in_model_routing_ui_ux} />
                  </td>
                  <td>
                    <Mark value={row.in_phase_targets_ui_ux} isDrift={uiUxDrift && !row.in_model_routing_ui_ux} />
                  </td>
                  <td><Mark value={row.is_write_phase} /></td>
                  <td><Mark value={row.in_model_routing_review} /></td>
                  <td><Mark value={row.in_model_routing_testing} /></td>
                  <td><Mark value={row.in_model_routing_requirements} /></td>
                  <td><Mark value={row.in_model_routing_research} /></td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      <div className="drift-legend" aria-label="legend">
        <span className="drift-mark yes">✓</span> present
        <span className="drift-mark no">–</span> absent
        <span className="drift-mark drift">✗</span> drift — inconsistent between files (ui-design/ux-design missing from phase_targets; design-review in phase_targets but not model_routing ui_ux)
      </div>
    </section>
  );
}

function ProposedCapabilitiesWireframe(): JSX.Element {
  return (
    <section aria-label="proposed phase capabilities table">
      <h2>Proposed Phase Capabilities (After)</h2>
      <p>
        Single source of truth per phase. The daemon reads these at startup;
        no branching on <code>phase_id</code> strings anywhere in routing,
        execution, or target planning.
      </p>

      <div style={{ overflowX: "auto" }}>
        <table className="drift-table" aria-label="proposed capabilities per phase">
          <thead>
            <tr>
              <th scope="col">phase_id</th>
              <th scope="col">can_write</th>
              <th scope="col">needs_commit</th>
              <th scope="col">model_group</th>
              <th scope="col">parse_research_signal</th>
              <th scope="col">safety_rules_template</th>
            </tr>
          </thead>
          <tbody>
            {PROPOSED_CAPABILITIES.map((cap) => (
              <tr key={cap.phase_id}>
                <td>{cap.phase_id}</td>
                <td><BoolCell value={cap.can_write} /></td>
                <td><BoolCell value={cap.needs_commit} /></td>
                <td><ModelGroupBadge group={cap.model_group} /></td>
                <td><BoolCell value={cap.parse_research_signal} /></td>
                <td>
                  {cap.safety_rules_template ? (
                    <code style={{ color: "#1a3a4e" }}>{cap.safety_rules_template}</code>
                  ) : (
                    <code style={{ color: "#7a8fa0" }}>null</code>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function CapabilityFieldsWireframe(): JSX.Element {
  const fields = [
    {
      name: "can_write",
      type: "bool",
      desc: "Controls write action rule in prompt. Replaces is_write_phase() in phase_executor.rs.",
    },
    {
      name: "needs_commit",
      type: "bool",
      desc: "Requests commit_message in output contract. Replaces phase_requires_commit_message().",
    },
    {
      name: "model_group",
      type: '"ui_ux" | "research" | "review" | "requirements" | "testing" | "default"',
      desc: "Primary model routing group. Replaces is_ui_ux_phase/is_research_phase/etc. in both model_routing.rs and phase_targets.rs.",
    },
    {
      name: "parse_research_signal",
      type: "bool",
      desc: 'Whether to scan stdout for {"kind":"research_required",...} signals during execution.',
    },
    {
      name: "safety_rules_template",
      type: "string | null",
      desc: 'Named safety rule block injected into prompt. "research" emits the greenfield/targeted-discovery rules. Null means no extra rules.',
    },
  ];

  return (
    <section aria-label="capability field definitions">
      <h2>Capability Field Definitions</h2>
      <p>
        Each field maps directly to a decision point that is currently a
        hardcoded match arm. Adding a new phase or reclassifying one requires
        only a config edit — no Rust recompile.
      </p>
      <div className="cap-grid">
        {fields.map((f) => (
          <div className="cap-card" key={f.name}>
            <p className="cap-name">{f.name}</p>
            <p className="cap-type">{f.type}</p>
            <p className="cap-desc">{f.desc}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

function AcceptanceCriteriaWireframe(): JSX.Element {
  const acs = [
    {
      id: "AC-01",
      text: "phase_capabilities block present in agent-runtime-config.v2.json for all standard phases.",
    },
    {
      id: "AC-02",
      text: "model_routing.rs is_*_phase() functions removed; replaced by lookup of model_group from config.",
    },
    {
      id: "AC-03",
      text: "daemon_scheduler_phase_targets.rs is_ui_ux_phase/is_research_phase removed; ui_ux/research group resolved from config.",
    },
    {
      id: "AC-04",
      text: "phase_executor.rs is_write_phase()/enforce_product_file_changes() replaced by can_write capability.",
    },
    {
      id: "AC-05",
      text: "phase_requires_commit_message() replaced by needs_commit capability field.",
    },
    {
      id: "AC-06",
      text: "safety_rules_template field drives phase_safety_rules(); research template matches current hardcoded string.",
    },
    {
      id: "AC-07",
      text: "Adding a new phase requires only a config entry; no Rust code change needed for standard classification.",
    },
    {
      id: "AC-08",
      text: "All existing tests pass; routing decisions for standard phases produce identical model/tool outputs before and after.",
    },
  ];

  return (
    <section aria-label="acceptance criteria traceability">
      <h2>Acceptance Criteria Traceability</h2>
      <ul className="ac-list">
        {acs.map((ac) => (
          <li key={ac.id}>
            <span className="ac-id">{ac.id}</span>
            <span>{ac.text}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function PhaseCapabilitiesWireframe(): JSX.Element {
  return (
    <section aria-label="TASK-147 phase capabilities config wireframe">
      <h1>Phase Capabilities Config</h1>
      <p>
        Wireframe for TASK-147: replacing three diverging hardcoded phase
        category match arms (<code>is_ui_ux_phase</code>,{" "}
        <code>is_research_phase</code>, <code>is_write_phase</code>, …) across{" "}
        <code>model_routing.rs</code>, <code>daemon_scheduler_phase_targets.rs</code>,
        and <code>phase_executor.rs</code> with a single{" "}
        <code>phase_capabilities</code> block in{" "}
        <code>agent-runtime-config.v2.json</code>.
      </p>

      <InconsistencyTableWireframe />
      <CapabilityFieldsWireframe />

      <section aria-label="config schema authoring">
        <h2>Config Schema (agent-runtime-config.v2.json)</h2>
        <p>
          The <code>phase_capabilities</code> key is added to each phase entry
          alongside the existing <code>mode</code>, <code>directive</code>, and{" "}
          <code>runtime</code> fields. Omitting it falls back to compiled
          defaults, preserving backward compatibility.
        </p>
        <CapabilitySchemaBlock />
      </section>

      <ProposedCapabilitiesWireframe />
      <RoutingFlowWireframe />
      <AcceptanceCriteriaWireframe />
    </section>
  );
}
