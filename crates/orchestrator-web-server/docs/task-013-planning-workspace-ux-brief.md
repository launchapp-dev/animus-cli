# TASK-013 UX Brief: Vision/Requirements Planning Workspace UI

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `65737bd7-e019-4524-b446-a9de9e082165`
- Task: `TASK-013`

## UX Objective
Design a planning workspace that lets operators move quickly between vision and
requirement authoring without losing context, while preserving deterministic
deep-link behavior and safe mutation patterns.

The experience must make three things obvious at all times:
1. What planning artifact the user is editing (vision vs requirement).
2. Whether changes are saved, pending, or failed.
3. How to navigate between list/detail and project-scoped deep links.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Product owner | Author and refine project vision | Can draft/update/refine vision without leaving `/planning/vision` |
| Delivery lead | Maintain requirement backlog quality | Can create/edit/delete requirements and run refinement from list or detail views |
| Engineer/reviewer | Open requirement details from other surfaces | Can deep-link to exact requirement and recover from missing/deleted IDs |

## UX Principles for This Phase
1. Keep planning context explicit: route title, artifact ID, and action scope are always visible.
2. Optimize for write-edit loops: users should complete common edits in a single screen without modal churn.
3. Prefer deterministic feedback: loading, validation, success, and envelope errors are rendered inline and consistently.
4. Protect destructive actions: delete and broad-scope refine actions require clear confirmation.
5. Build keyboard-first forms: every planning action is reachable and operable without a mouse.

## Information Architecture

### Primary Navigation Order
1. `Dashboard`
2. `Daemon`
3. `Projects`
4. `Planning`
5. `Tasks`
6. `Workflows`
7. `Events`
8. `Review Handoff`

### Planning Route Group
- `/planning` (redirect to `/planning/vision`)
- `/planning/vision`
- `/planning/requirements`
- `/planning/requirements/new`
- `/planning/requirements/:requirementId`

### Cross-Surface Deep Link
- `/projects/:projectId/requirements/:requirementId` must expose a clear, visible
  path to `/planning/requirements/:requirementId`.
- `requirementId` is treated as case-preserving and must not be normalized by the
  client router.

## Key Screens and Interaction Contracts

| Route | Screen goal | Primary interactions | Required UI states |
| --- | --- | --- | --- |
| `/planning` | Canonical planning entrypoint | Immediate redirect to vision workspace | redirect-in-progress, fallback error |
| `/planning/vision` | Create/update/refine vision | Edit form fields, save vision, trigger refine | loading, empty-first-run, validation error, save pending, save success, refine pending, refine failure |
| `/planning/requirements` | Browse and prioritize requirement set | Stable list browse, multi-select, refine selected/all, open detail, open create form | loading, empty list, selection active, refine pending, refine result summary, API error |
| `/planning/requirements/new` | Author a new requirement | Fill required fields, submit create, cancel back to list | initial, validation error, submit pending, submit success redirect, submit failure |
| `/planning/requirements/:requirementId` | Inspect and mutate one requirement | Edit mutable fields, save, delete with confirmation, trigger refine for current requirement | loading, ready, optimistic pending, not-found recovery, delete confirm, delete success redirect, envelope error |
| `/projects/:projectId/requirements/:requirementId` | Project-scoped requirement read surface | Follow prominent link to planning detail editor | loading, not-found, navigation handoff available |

## Critical User Flows

### Flow A: Planning Entry and Route Stability
1. User opens `/planning`.
2. Router redirects to `/planning/vision`.
3. Refresh on any planning route preserves the same screen and route params.

### Flow B: First Vision Creation
1. User lands on `/planning/vision` with no existing vision.
2. Empty-state explains required fields and primary action.
3. User enters project name, problem statement, target users, goals, constraints, and value proposition.
4. Save action shows pending state and returns inline success or error without clearing inputs.

### Flow C: Vision Refinement Loop
1. User opens existing vision in `/planning/vision`.
2. User triggers refine action with explicit scope/intent context.
3. UI shows refine progress and then a deterministic result panel.
4. User can accept updated content and save without leaving the page.

### Flow D: Requirements List and Batch Refinement
1. User opens `/planning/requirements`.
2. List is sorted by requirement ID and supports keyboard selection.
3. User triggers refine for selected requirements or all requirements.
4. Result summary reports affected IDs and errors, with retry path for failures.

### Flow E: Requirement Authoring
1. User chooses `New Requirement` from list view.
2. `/planning/requirements/new` form loads with required labels and helper text.
3. Submit creates requirement and redirects to `/planning/requirements/:requirementId`.

### Flow F: Requirement Detail Edit and Delete
1. User opens `/planning/requirements/:requirementId`.
2. Edits are applied via save action with inline pending and result feedback.
3. Delete opens a confirmation dialog naming the requirement ID.
4. Confirmed delete redirects to list with success notice; revisiting deleted deep link renders recoverable `not_found`.

### Flow G: Project Detail to Planning Detail Handoff
1. User opens `/projects/:projectId/requirements/:requirementId`.
2. User selects `Edit in Planning Workspace`.
3. App navigates to `/planning/requirements/:requirementId` and preserves ID casing.

## Layout, Hierarchy, and Spacing Guidance

### Desktop (`>= 960px`)
- Keep route title + summary + primary actions in a single page header band.
- Requirements list page uses two-region layout: list/actions region and details/preview region.
- Form-heavy pages cap line length for readability and group fields into clearly labeled sections.

### Mobile (`< 960px`)
- Stack header metadata, actions, and forms vertically.
- Batch actions collapse into a single action bar above list content.
- Detail forms place destructive actions after save actions with clear separation.

### Spacing and Touch Targets
- Use a consistent spacing scale (`4/8/12/16/24/32px`).
- Keep minimum touch target size at `44x44px`.
- Preserve visible separation between: page heading, metadata, primary actions, and mutable form fields.

## Accessibility Constraints (Non-Negotiable)
1. Landmarks: one primary `header`, one primary `nav`, and one primary `main`.
2. Headings: each planning route exposes exactly one `h1` and ordered subordinate headings.
3. Labels: all form controls have programmatic labels and associated helper/error text.
4. Keyboard: every create/update/delete/refine action is reachable and actionable via keyboard only.
5. Focus: visible focus indicator on links, buttons, form fields, and dialog controls.
6. Errors: validation and envelope errors are announced via `role="alert"` and tied to fields where applicable.
7. Dialogs: delete confirmations use accessible dialog semantics with focus trap and escape/close behavior.
8. Contrast: text and controls satisfy WCAG AA contrast ratios.
9. Motion: honor reduced-motion preference for route transitions and async-state animation.
10. Reflow: planning surfaces remain usable at `320px` with no horizontal scroll.

## State and Feedback Model
- Route-level states: `loading`, `ready`, `empty`, `not_found`, `error`.
- Mutation-level states: `idle`, `pending`, `success`, `failure`.
- Shared error shape: always display `code`, `message`, and `exitCode` from envelope failures.
- Mutation feedback is local to the initiating form/action and does not silently reset user inputs.

## Design Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Vision and requirement contexts become conflated | User edits wrong artifact | Persistent route-level artifact labeling and explicit action copy |
| Broad refine action feels unsafe | Accidental large-scale content changes | Separate selected vs all actions and require confirmation for all-scope refine |
| Deep-link mismatch after deletes | Broken navigation and user confusion | Deterministic `not_found` state with immediate link back to requirements list |
| Dense planning forms degrade on mobile | Slow authoring and input errors | Stacked layout, grouped fields, and stable action placement |

## UX Acceptance Checklist for Implementation Phase
- Planning navigation includes a first-class `Planning` entry.
- Each planning route has explicit loading, empty/not-found, and error behavior.
- Vision authoring supports create, update, and refine loops in one workspace.
- Requirements workspace supports list/detail/new plus selected/all refine actions.
- Requirement delete flow uses explicit confirmation and recoverable deep-link behavior.
- Project requirement detail screen offers clear handoff link to planning detail editor.
- At `320px`, create/edit/refine flows remain keyboard-usable without horizontal scroll.
