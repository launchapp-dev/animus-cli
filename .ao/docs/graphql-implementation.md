# GraphQL API Implementation Requirements

## Overview
Add a GraphQL API endpoint at `/graphql` on the web server using `async-graphql`. This provides an alternative to the existing REST API with support for flexible queries, reduced over-fetching, and relationships between entities.

## Scope

### Types to Expose

1. **Task**
   - Fields: id, title, description, status, priority, type, risk, complexity, scope
   - Relationships: linked_requirements, checklist, dependencies, assignee

2. **Requirement**
   - Fields: id, title, description, body, priority, status, type, category
   - Relationships: linked_tasks, linked_workflows

3. **Workflow**
   - Fields: id, status, current_phase, started_at, completed_at
   - Relationships: phases, decisions, checkpoints

4. **DaemonStatus**
   - Fields: healthy, status, runner_connected, active_agents, max_agents, project_root

5. **AgentRun**
   - Fields: id, task_id, workflow_id, status, started_at, completed_at, model

### Relationships
- Task -> Requirement (many-to-many via linked_requirements)
- Workflow -> Phase -> Decision (one-to-many)
- Task -> Dependencies (self-referential many-to-many)

### Endpoints
- `POST /graphql` - Execute GraphQL queries/mutations
- `GET /graphql` - Interactive GraphQL playground (if UI enabled)

### Operations
- Queries: list and fetch individual entities with filtering
- Mutations: create/update operations mirroring REST endpoints
- Subscriptions: future follow-up (not in initial scope)

## Constraints
- Must use existing `ServiceHub` interface from `orchestrator-core`
- Must integrate with existing `WebApiContext` for project_root access
- Must follow existing error handling patterns (WebApiError)
- Must work alongside existing REST API (non-breaking)
- Endpoint should be at `/graphql` (or `/api/graphql` for API-only mode)

## Acceptance Criteria
1. GraphQL endpoint responds at /graphql
2. Can query tasks, requirements, workflows, daemon status via GraphQL
3. Supports at least basic filtering (by id, status, priority)
4. Mutations work for creating/updating tasks
5. Error responses follow existing envelope pattern
6. Works in both API-only and full UI modes
7. All existing REST tests continue to pass
