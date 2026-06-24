# orchestrator-cli

The main `animus` binary and the primary user-facing CLI surface for the workspace.

## Overview

Every `animus` invocation flows through this crate. It parses the command line,
resolves the project root, constructs a `FileServiceHub` when needed, and then
dispatches into either runtime control flows or operation handlers. It also owns
the CLI-facing `animus.cli.v1` JSON envelope used by `--json`.

## Target

- Binary: `animus`

## Startup flow

```mermaid
flowchart TD
    MAIN["main.rs"] --> PARSE["Cli::parse()"]
    PARSE --> ROOT["resolve_project_root()"]
    ROOT --> CMD{"top-level command"}

    CMD -->|version| VER["print version"]
    CMD -->|doctor| EARLY["run early diagnostics"]
    CMD -->|everything else| HUB["FileServiceHub::new()"]

    HUB --> ROUTE{"handler family"}
    ROUTE -->|daemon / agent / chat| RT["services::runtime"]
    ROUTE -->|workflow / queue / git / plugin / mcp / web / ...| OPS["services::operations"]
```

## Current layout

### CLI types

`src/cli_types/` contains the Clap-derived command tree. The live top-level
surface is defined by:

- `root_types.rs`
- `agent_types.rs`
- `approval_types.rs`
- `auth_types.rs`
- `chat_types.rs`
- `cost_types.rs`
- `daemon_types.rs`
- `doctor_types.rs`
- `events_types.rs`
- `flavor_types.rs`
- `git_types.rs`
- `history_types.rs`
- `init_types.rs`
- `logs_types.rs`
- `mcp_types.rs`
- `output_types.rs`
- `pack_types.rs`
- `plugin_types.rs`
- `queue_types.rs`
- `secret_types.rs`
- `shared_types.rs`
- `skill_types.rs`
- `state_types.rs`
- `subject_types.rs`
- `trigger_types.rs`
- `update_types.rs`
- `web_types.rs`
- `workflow_types.rs`

### Services

```mermaid
graph TD
    subgraph "services/runtime"
        RDAEMON["runtime_daemon/"]
        RAGENT["runtime_agent/"]
        RCHAT["runtime_chat/"]
        RFACTS["execution_fact_projection/"]
        RMUT["workflow_mutation_surface/"]
    end

    subgraph "services/operations"
        OAGENT["ops_agent/"]
        ODOCTOR["ops_doctor/"]
        OGIT["ops_git/"]
        OMCP["ops_mcp/"]
        ONOW["ops_now/"]
        OPLUGIN["ops_plugin/"]
        OQUEUE["ops_queue/"]
        OSKILL["ops_skill/"]
        OSTATUS["ops_status/"]
        OWORKFLOW["ops_workflow/"]
    end
```

## Top-level commands

The current command families are:

- `version`
- `daemon`
- `agent`
- `chat`
- `queue`
- `workflow`
- `history`
- `git`
- `approval`
- `skill`
- `pack`
- `plugin`
- `status`
- `output`
- `mcp`
- `web`
- `init`
- `doctor`
- `trigger`
- `logs`
- `subject`
- `flavor`
- `update`
- `cost`
- `auth`
- `events`
- `state`
- `secret`

See [`docs/reference/cli/index.md`](../../docs/reference/cli/index.md) for the
full tree and selected flags.

## Key files

- `src/shared/output.rs`: success/error printing and JSON envelope formatting
- `src/shared/cli_error.rs`: exit-code mapping and error classification
- `src/shared/parsing.rs`: argument normalization and validation helpers
- `src/services/runtime/`: daemon, agent, and chat runtime control paths
- `src/services/operations/`: command handlers for workflow, git, plugin, MCP,
  output, queue, status, and related operations

## Notes

- `doctor` can run without constructing a `FileServiceHub`.
- `animus web` launches external `transport_backend` and `web_ui` plugins; the
  web stack is not in-tree.
