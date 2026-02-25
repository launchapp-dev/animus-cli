# TASK-013 Wireframes: Planning Workspace UI

These mockups define concrete, production-oriented planning workspace wireframes
for vision and requirements authoring flows in `TASK-013`.

## Files
- `wireframes.html`: desktop/mobile wireframe boards for planning routes,
  realistic loading/empty/error/mutation states, and deep-link handoff flows.
- `wireframes.css`: shared visual system for hierarchy, spacing, focus states,
  responsive behavior, and reduced-motion handling.
- `route-architecture.tsx`: React-oriented route/contracts scaffold for planning
  navigation, envelope parsing, mutation state modeling, and ID-safe deep links.

## Route Coverage

| Route | Covered in |
| --- | --- |
| `/planning` | `wireframes.html` (`Planning Entry Redirect`) + `route-architecture.tsx` |
| `/planning/vision` | `wireframes.html` (`Vision Workspace`) + `route-architecture.tsx` |
| `/planning/requirements` | `wireframes.html` (`Requirements Workspace`) + `route-architecture.tsx` |
| `/planning/requirements/new` | `wireframes.html` (`New Requirement`) + `route-architecture.tsx` |
| `/planning/requirements/:requirementId` | `wireframes.html` (`Requirement Detail`) + `route-architecture.tsx` |
| `/projects/:projectId/requirements/:requirementId` | `wireframes.html` (`Project Requirement Handoff`) + `route-architecture.tsx` |

## Interaction and State Coverage
- Route-level states: `loading`, `ready`, `empty`, `not_found`, `error`
- Mutation-level states: `idle`, `pending`, `success`, `failure`
- Refinement scope states: `single`, `selected`, `all`

The boards include realistic states for:
- first-run vision creation
- vision refine pending/result/failure rendering
- requirements multi-select and batch refine summary
- requirement create form validation and pending save
- requirement detail delete confirmation and post-delete not-found recovery
- project requirement deep-link handoff to planning editor

## Accessibility and Responsive Coverage
- Landmark hierarchy represented with `header`, `nav`, and `main`.
- All authoring controls are shown with explicit labels and helper/error text.
- Focus-visible treatment is present for links, buttons, selects, checkboxes,
  and text inputs.
- Delete confirmation is modeled with accessible dialog semantics.
- Mobile board demonstrates `320px` usability with stacked controls and no
  horizontal page scrolling.
- Reduced-motion behavior is defined in `wireframes.css`.

## Mockup Review Updates
- Added explicit requirement-detail envelope-error state in route cards.
- Added a dedicated `Refine All` confirmation dialog to model broad-scope safety.
- Added status live-region annotations for refine mutation progress and result.
- Increased requirement row interaction targets to `44x44` minimum for selection
  and open actions.
- Added route-scaffold helper logic to explicitly model refine-all confirmation
  requirements.
- Added explicit delete-dialog keyboard behavior notes (initial focus, focus
  trap, escape-to-close).
- Wrapped the state matrix in a responsive container to avoid page-level
  horizontal scroll pressure at narrow widths.

## Acceptance Criteria Traceability

| AC | Trace |
| --- | --- |
| `AC-01` | Planning route cards, redirect board, and route config scaffold |
| `AC-02` | Vision form states and save/refine loops in desktop/mobile boards |
| `AC-03` | Requirements list/new/detail boards with create/update/delete actions |
| `AC-04` | Refine action controls, all-scope confirmation, and deterministic refine result summaries |
| `AC-05` | Deep-link cards and project -> planning handoff board |
| `AC-06` | Shared envelope error panel and `parseAoEnvelope` scaffold |
| `AC-07` | Keyboard/focus examples, labeled controls, and 44px interaction targets across wireframes |
| `AC-08` | 320px mobile board, responsive CSS constraints, and state-matrix overflow containment |
