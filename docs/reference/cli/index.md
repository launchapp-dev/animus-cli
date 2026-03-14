# CLI Command Surface

Complete reference of every `ao` command, subcommand, and key flag. This tree is the authoritative map of the CLI surface area. For global flags that apply to all commands, see [Global Flags](global-flags.md). For exit code semantics, see [Exit Codes](exit-codes.md).

## Global Flags

| Flag | Description |
|---|---|
| `--json` | Machine-readable JSON output (`ao.cli.v1` envelope) |
| `--project-root <PATH>` | Override project root (also reads `PROJECT_ROOT` env) |

---

## Top-Level Command Tree

```
ao
├── version                  Show installed ao version
├── status                   Unified project status dashboard
├── setup                    Guided onboarding wizard
├── doctor                   Environment diagnostics (--fix)
├── tui                      Interactive terminal UI
│
├── daemon                   Daemon lifecycle & automation
│   ├── start                Start daemon (detached/background)
│   ├── run                  Run daemon in foreground
│   ├── stop                 Stop daemon
│   ├── status               Show daemon status
│   ├── health               Show daemon health
│   ├── pause                Pause scheduler
│   ├── resume               Resume scheduler
│   ├── events               Stream event history
│   ├── logs                 Read daemon logs
│   ├── clear-logs           Clear daemon logs
│   ├── agents               List daemon-managed agents
│   └── config               Update automation config
│
├── agent                    Agent execution
│   ├── run                  Start an agent run
│   ├── control              Control agent (pause/resume/terminate)
│   ├── status               Get run status
│   ├── model-status         Check model availability
│   └── runner-status        Inspect runner availability
│
├── project                  Project management
│   ├── list                 List registered projects
│   ├── active               Show active project
│   ├── get                  Get project by id
│   ├── create               Create project
│   ├── load                 Set active project
│   ├── rename               Rename project
│   ├── archive              Archive project
│   └── remove               Remove project
│
├── task                     Task management
│   ├── list                 List tasks (filterable)
│   ├── prioritized          Tasks sorted by priority
│   ├── next                 Get next ready task
│   ├── stats                Task statistics
│   ├── get                  Get task by id
│   ├── create               Create task
│   ├── update               Update task
│   ├── delete               Delete task (confirmation)
│   ├── assign               Assign task to a user or agent
│   ├── checklist-add        Add checklist item
│   ├── checklist-update     Toggle checklist item
│   ├── dependency-add       Add dependency edge
│   ├── dependency-remove    Remove dependency edge
│   ├── status               Set task status
│   ├── history              Show workflow dispatch history
│   ├── pause                Pause task
│   ├── resume               Resume paused task
│   ├── cancel               Cancel task (confirmation)
│   ├── set-priority         Set task priority
│   ├── set-deadline         Set/clear task deadline
│   └── rebalance-priority   Rebalance priorities by budget
│
├── workflow                 Workflow execution & config
│   ├── list                 List workflows
│   ├── get                  Get workflow details
│   ├── decisions            Show workflow decisions
│   ├── run                  Start workflow (async, daemon)
│   ├── execute              Execute workflow (sync, no daemon)
│   ├── resume               Resume paused workflow
│   ├── resume-status        Check resumability
│   ├── pause                Pause workflow (confirmation)
│   ├── cancel               Cancel workflow (confirmation)
│   ├── update-definition    Update workflow definition by id
│   ├── checkpoints
│   │   ├── list             List checkpoints
│   │   ├── get              Get checkpoint
│   │   └── prune            Prune checkpoints
│   ├── phase
│   │   └── approve          Approve pending phase gate
│   ├── phases
│   │   ├── list             List phase definitions
│   │   ├── get              Get phase by id
│   │   ├── upsert           Create/replace phase
│   │   └── remove           Remove phase
│   ├── definitions
│   │   ├── list             List workflow definitions
│   │   └── upsert           Create/replace workflow definition
│   ├── config
│   │   ├── get              Read workflow config
│   │   ├── validate         Validate config
│   │   └── compile          Compile YAML workflows
│   ├── state-machine
│   │   ├── get              Read state-machine config
│   │   ├── validate         Validate state-machine
│   │   └── set              Replace state-machine config
│   └── agent-runtime
│       ├── get              Read agent-runtime config
│       ├── validate         Validate agent-runtime config
│       └── set              Replace agent-runtime config
│
├── vision                   Project vision
│   ├── draft                Draft vision
│   ├── refine               Refine vision
│   └── get                  Read vision
│
├── requirements             Requirements management
│   ├── draft                Draft from project context
│   ├── list                 List requirements
│   ├── get                  Get requirement by id
│   ├── refine               Refine requirements
│   ├── create               Create requirement
│   ├── update               Update requirement
│   ├── delete               Delete requirement
│   ├── graph
│   │   ├── get              Read requirement graph
│   │   └── save             Replace requirement graph
│   ├── mockups
│   │   ├── list             List mockups
│   │   ├── create           Create mockup record
│   │   ├── link             Link mockup to requirements
│   │   └── get-file         Get mockup file
│   └── recommendations
│       ├── scan             Run recommendation scan
│       ├── list             List recommendation reports
│       ├── apply            Apply recommendation report
│       ├── config-get       Read recommendation config
│       └── config-update    Update recommendation config
│
├── architecture             Architecture graph
│   ├── get                  Read architecture graph
│   ├── set                  Replace architecture graph
│   ├── suggest              Suggest links for a task
│   ├── entity
│   │   ├── list             List entities
│   │   ├── get              Get entity by id
│   │   ├── create           Create entity
│   │   ├── update           Update entity
│   │   └── delete           Delete entity
│   └── edge
│       ├── list             List edges
│       ├── create           Create edge
│       └── delete           Delete edge
│
├── review                   Review decisions (hidden)
│   ├── entity               Review status for entity
│   ├── record               Record review decision
│   ├── task-status          Review status for task
│   ├── requirement-status   Review status for requirement
│   ├── handoff              Record role handoff
│   └── dual-approve         Record dual-approval
│
├── qa                       QA evaluation
│   ├── evaluate             Evaluate QA gates
│   ├── get                  Get evaluation result
│   ├── list                 List evaluations
│   └── approval
│       ├── add              Add gate approval
│       └── list             List gate approvals
│
├── history                  Execution history
│   ├── task                 History for a task
│   ├── get                  Get history record
│   ├── recent               Recent history
│   ├── search               Search history
│   └── cleanup              Remove old records
│
├── errors                   Error tracking
│   ├── list                 List errors
│   ├── get                  Get error by id
│   ├── stats                Error statistics
│   ├── retry                Retry error
│   └── cleanup              Remove old errors
│
├── git                      Git operations
│   ├── repo
│   │   ├── list             List repositories
│   │   ├── get              Get repository
│   │   ├── init             Init + register repo
│   │   └── clone            Clone + register repo
│   ├── branches             List branches
│   ├── status               Repo status
│   ├── commit               Commit changes
│   ├── push                 Push branch
│   ├── pull                 Pull branch
│   ├── worktree
│   │   ├── create           Create worktree
│   │   ├── list             List worktrees
│   │   ├── get              Get worktree
│   │   ├── remove           Remove worktree (confirmation)
│   │   ├── prune            Prune task worktrees
│   │   ├── pull             Pull in worktree
│   │   ├── push             Push from worktree
│   │   ├── sync             Pull + push worktree
│   │   └── sync-status      Sync status
│   └── confirm
│       ├── request          Request confirmation
│       ├── respond          Approve/reject confirmation
│       └── outcome          Record operation outcome
│
├── skill                    Skill management
│   ├── search               Search skill catalog
│   ├── install              Install skill
│   ├── list                 List installed skills
│   ├── update               Update skills
│   └── publish              Publish skill version
│
├── model                    Model management
│   ├── availability         Check model availability
│   ├── status               Model + API key status
│   ├── validate             Validate model selection
│   ├── roster
│   │   ├── refresh          Refresh model roster
│   │   └── get              Get roster snapshot
│   └── eval
│       ├── run              Run model evaluation
│       └── report           Show evaluation report
│
├── runner                   Runner management
│   ├── health               Runner health
│   ├── orphans
│   │   ├── detect           Detect orphans
│   │   └── cleanup          Clean orphans
│   └── restart-stats        Restart statistics
│
├── output                   Run output inspection
│   ├── run                  Read run events
│   ├── artifacts            List artifacts
│   ├── download             Download artifact
│   ├── files                List artifact files
│   ├── jsonl                Read JSONL logs
│   ├── monitor              Monitor run output
│   └── cli                  Infer CLI provider
│
├── mcp                      MCP server
│   └── serve                Start MCP server
│
└── web                      Web UI
    ├── serve                Start web server
    └── open                 Open web UI in browser
```

## Summary

| Metric | Count |
|---|---|
| Top-level commands | 24 |
| Total subcommands (all levels) | ~130+ |
| Commands with `--confirmation` pattern | 8 |
| Commands with `--input-json` | 15+ |
| Commands with `--dry-run` | 6 |
