# orchestrator-cli

The main `ao` command-line binary and the primary user-facing surface of the AO workspace.

## Overview

Every `ao` invocation flows through this crate. It parses the command line, resolves the project root, decides whether a `FileServiceHub` is needed, and then dispatches the request into one of three handler families:

- `services::runtime`
- `services::operations`
- `services::tui`

It also owns the CLI-facing JSON envelope behavior for `--json`.

## Targets

- Binary: `ao`

## Architecture

```mermaid
flowchart TD
    MAIN["main.rs"] --> PARSE["Cli::parse()"]
    PARSE --> ROOT["resolve_project_root()"]
    ROOT --> CMD{"top-level command"}

    CMD -->|version| VER["print version"]
    CMD -->|setup / doctor| EARLY["operations handlers without service hub"]
    CMD -->|everything else| HUB["FileServiceHub::new()"]

    HUB --> ROUTE{"handler family"}
    ROUTE -->|daemon / agent / project / task| RT["services::runtime"]
    ROUTE -->|workflow / schedule / queue / requirements / review / web / ...| OPS["services::operations"]
    ROUTE -->|tui / workflow-monitor| TUI["services::tui"]
```

## Current service tree

```mermaid
graph TD
    subgraph "services/runtime"
        RDAEMON["runtime_daemon/"]
        RAGENT["runtime_agent/"]
        RPT["runtime_project_task/"]
        RSTALE["stale_in_progress.rs"]
        RSYNC["workflow_result_sync.rs"]
    end

    subgraph "services/operations"
        OPLAN["ops_planning/"]
        OREQ["ops_requirements/"]
        OWF["ops_workflow/"]
        OGIT["ops_git/"]
        OMODEL["ops_model/"]
        OSKILL["ops_skill/"]
        OREST["ops_architecture / ops_history / ops_mcp / ops_output / ops_queue / ops_review / ops_runner / ops_schedule / ops_setup / ops_status / ops_web / ops_qa / ops_errors / ops_doctor"]
    end

    subgraph "services/tui"
        TAPP["app_state / app_event / render"]
        TMON["daemon_monitor/"]
        TWF["workflow_monitor/"]
        TMCP["mcp_bridge.rs"]
    end
```

## Top-level command groups

Current top-level commands include:

- `version`
- `daemon`
- `agent`
- `project`
- `queue`
- `task`
- `workflow`
- `schedule`
- `vision`
- `requirements`
- `architecture`
- `review`
- `qa`
- `history`
- `errors`
- `git`
- `skill`
- `model`
- `runner`
- `status`
- `output`
- `mcp`
- `web`
- `setup`
- `tui`
- `workflow-monitor`
- `doctor`

Deprecated compatibility aliases still exist for `task-control` and `execute`.

## Key pieces

### CLI types

`src/cli_types/` contains the Clap-derived command tree. The command surface is split by domain into files such as `task_types.rs`, `workflow_types.rs`, `daemon_types.rs`, `agent_types.rs`, `review_types.rs`, and `web_types.rs`.

### Shared CLI infrastructure

- `src/shared/output.rs`: JSON envelope formatting and success/error printing.
- `src/shared/cli_error.rs`: CLI error classification and exit-code mapping.
- `src/shared/parsing.rs`: argument normalization and validation helpers.
- `src/shared/runner.rs`: runner-related helper logic used by the command handlers.

### Runtime handlers

The runtime layer handles stateful or long-lived flows such as daemon lifecycle, agent execution, and task/project mutations.

### Operations handlers

The operations layer handles planning, CRUD, inspection, web serving, queue management, review, model inspection, git commands, schedules, and similar command groups.

### TUI handlers

The TUI layer provides the `tui` and `workflow-monitor` experiences built on `ratatui` and `crossterm`.

## Workspace dependencies

```mermaid
graph LR
    CLI["orchestrator-cli"]
    CORE["orchestrator-core"]
    WFR["workflow-runner"]
    DRT["orchestrator-daemon-runtime"]
    GIT["orchestrator-git-ops"]
    NOTIF["orchestrator-notifications"]
    WEBAPI["orchestrator-web-api"]
    WEBSRV["orchestrator-web-server"]
    WEBCON["orchestrator-web-contracts"]
    PROTO["protocol"]
    WRAP["llm-cli-wrapper"]

    CLI --> CORE
    CLI --> WFR
    CLI --> DRT
    CLI --> GIT
    CLI --> NOTIF
    CLI --> WEBAPI
    CLI --> WEBSRV
    CLI --> WEBCON
    CLI --> PROTO
    CLI --> WRAP
```

## Notes

- `setup` and `doctor` are handled before `FileServiceHub` initialization.
- All `--json` responses use the `ao.cli.v1` schema envelope.
