# TASK-094 Core Requirements: Profile MCP Schema and Runtime Contract Mapping

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `02df5ca9-bd93-49da-9950-19e50a22e44d`
- Task: `TASK-094`
- Baseline audited: `2026-02-27`

## Why this core note exists
`TASK-094` is primarily daemon wiring, but it depends on core-owned schema and
runtime-contract surfaces. This note defines the core crate contract needed by
`orchestrator-cli` and `agent-runner` integration work.

## Baseline Audit

| Core surface | Location | Current state | Required change |
| --- | --- | --- | --- |
| Agent profile schema | `crates/orchestrator-core/src/agent_runtime_config.rs` (`AgentProfile`) | model/tool/runtime fields only | add typed `mcp_servers` and `tool_policy` accessors for phase agent profile |
| Built-in config schema | `crates/orchestrator-core/config/agent-runtime-config.v2.json` | no MCP profile fields in default agent entries | extend schema examples/default shape to include optional MCP config |
| Runtime contract builder MCP payload | `crates/orchestrator-core/src/runtime_contract.rs` (`build_runtime_contract`) | endpoint + agent_id + enforce_only + default prefixes | support explicit caller-provided MCP transport/prefix fields including stdio |

## Core Functional Requirements
- `CFR-01`: `AgentProfile` can represent MCP server config with enough detail to
  select AO built-in server for daemon use.
- `CFR-02`: `AgentProfile` can represent profile and/or server tool-policy
  inputs required for prefix derivation.
- `CFR-03`: Runtime-contract builder accepts explicit MCP transport input for
  stdio launch (`command` + `args`) in addition to endpoint.
- `CFR-04`: Runtime-contract builder accepts explicit allowed prefixes and
  carries them to `mcp.allowed_tool_prefixes` unchanged.
- `CFR-05`: Existing callers without explicit MCP inputs continue to work.

## Core Constraints
- Keep schema changes additive and backward compatible for existing config files.
- Keep runtime contract JSON shape stable where already consumed by runner.
- Do not couple core logic to daemon-only environment variables.

## Core Acceptance Criteria
- `CAC-01`: Parsing/loading config with profile `mcp_servers` + `tool_policy`
  succeeds and values are retrievable from accessors.
- `CAC-02`: Runtime contract can encode stdio MCP transport for AO built-in
  server.
- `CAC-03`: Runtime contract preserves explicit caller-provided
  `allowed_tool_prefixes` values without fallback expansion.
- `CAC-04`: Legacy call sites that only pass endpoint/agent id continue to
  produce valid contract output.

## Core Validation Targets (Implementation Phase)
- `cargo test -p orchestrator-core agent_runtime_config`
- `cargo test -p orchestrator-core runtime_contract`
