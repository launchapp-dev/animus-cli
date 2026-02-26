# TASK-016 Mockup Review: High-Risk Action Safeguards in Web UI

## Phase
- Workflow phase: `mockup-review`
- Workflow ID: `9f3fbbad-f13f-4c31-ae02-09398e9e3b36`
- Task: `TASK-016`

## Scope of Review
Reviewed `TASK-016` wireframe artifacts against:
- `task-016-high-risk-action-safeguards-requirements.md`
- `task-016-high-risk-action-safeguards-ux-brief.md`

Reviewed artifacts:
- `mockups/task-016-high-risk-action-safeguards/wireframes.html`
- `mockups/task-016-high-risk-action-safeguards/wireframes.css`
- `mockups/task-016-high-risk-action-safeguards/daemon-action-safeguards-wireframe.tsx`
- `mockups/task-016-high-risk-action-safeguards/README.md`

## Mismatch Resolution Log

| Mismatch | Requirement/UX reference | Resolution |
| --- | --- | --- |
| Feedback board showed retention cap but did not explicitly show scope filtering and deterministic oldest-first eviction behavior | FR-05, FR-09, AC-11 | Added daemon-scope filter controls, retention/eviction copy, and record IDs in desktop and mobile feedback examples (`wireframes.html`, `wireframes.css`, `.tsx`, `README.md`) |
| Keyboard/focus-return contract existed in text docs but was underrepresented in the dialog board itself | FR-07, AC-08 | Added explicit keyboard/focus contract callout in the desktop dialog board and mirrored focus-return semantics in the React wireframe scaffold (`wireframes.html`, `daemon-action-safeguards-wireframe.tsx`) |
| Mobile board showed confirmation flow but lacked an auditable feedback row at 320px | FR-05, FR-08, AC-09 | Added a compact mobile feedback card with outcome, record ID, and correlation ID (`wireframes.html`, `wireframes.css`) |
| Mockup README acceptance traceability only covered AC-01 through AC-07 | Phase directive + AC traceability requirement | Expanded AC matrix to AC-01 through AC-12 with artifact-level evidence (`README.md`) |
| TSX scaffold lacked explicit acceptance traceability payload for handoff review | Phase directive + implementation handoff clarity | Added `ACCEPTANCE_TRACEABILITY` matrix in scaffold output for deterministic review evidence (`daemon-action-safeguards-wireframe.tsx`) |

## Acceptance Criteria Traceability (Mockup Phase)

| AC | Evidence |
| --- | --- |
| `AC-01` | High-risk buttons route to confirmation dialog and block direct mutation (`wireframes.html`, `onRequestAction`) |
| `AC-02` | Dialog confirm remains disabled until exact typed intent match (`wireframes.html`, `isTypedIntentValid`) |
| `AC-02a` | Exact phrases are rendered and encoded (`STOP DAEMON`, `CLEAR DAEMON LOGS`) (`wireframes.html`, guard registry) |
| `AC-03` | Pre-submit preview cards show request metadata, planned effects, snapshot checks, irreversible effects, rollback guidance (`wireframes.html`, `.tsx`) |
| `AC-04` | Pending lock copy and single in-flight action state are represented (`wireframes.html`, `pendingAction`) |
| `AC-05` | Feedback entries include record ID, actor, timestamp, action, method/path, outcome, code/message, correlation ID (`wireframes.html`, `.tsx`) |
| `AC-06` | Diagnostics linkage is represented via shared correlation ID continuity (`wireframes.html`) |
| `AC-07` | Existing `/api/v1` method/path contracts remain unchanged in previews and feedback metadata (`wireframes.html`, `.tsx`) |
| `AC-08` | Keyboard and focus behavior (`Escape`, cancel, focus return) are explicitly modeled (`wireframes.html`, `.tsx`) |
| `AC-09` | Mobile `320px` board includes stacked confirmation + feedback with no horizontal overflow intent (`wireframes.html`, `wireframes.css`) |
| `AC-10` | Low-risk route behavior remains unchanged; safeguards are scoped to high-risk mutation flow (`README.md`, `.tsx`) |
| `AC-11` | Bounded cap `50`, newest-first display, and oldest-first eviction behavior are represented (`wireframes.html`, `.tsx`, `README.md`) |
| `AC-12` | `daemon.pause`, `daemon.start`, `daemon.resume` remain direct-execution actions (`wireframes.html`, guard policy) |

## Outcome
`TASK-016` mockups now provide explicit safeguard UX evidence for filtering, eviction, keyboard/focus behavior, and full acceptance-criteria coverage, enabling deterministic build handoff for the implementation phase.
