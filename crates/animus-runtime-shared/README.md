# animus-runtime-shared

Shared runtime helpers used by Animus's `workflow_runner` plugins (e.g.
`animus-workflow-runner-default`) and the kernel daemon (`animus-cli`).

This crate exists to dedupe the modules that **both** the plugin and the
kernel daemon need to understand byte-identically:

- **Phase session checkpoints** (`phase_session`) — durable Running/Completed
  state on disk, the recovery oracle for crash replay.
- **Phase output persistence** (`phase_output`) — JSON output files,
  completion markers, persisted decisions.
- **Phase prompt rendering** (`phase_prompt`) — prompt assembly from
  workflow YAML + agent runtime config.
- **Phase metadata types** (`phase_metadata`) — `PhaseExecutionMetadata`,
  `PhaseExecutionOutcome`, `PhaseExecutionSignal`.
- **Runtime contract construction** (`runtime_contract`) — assembles the
  `runtime_contract` JSON object passed to agent CLIs, including MCP injection.
- **Runner IPC bridge** (`ipc`) — the Unix-socket bridge to `agent-runner`
  used by phase executors AND the kernel CLI to dispatch agent processes.
- **Workflow event emitters** (`workflow_event_emitter`, `reattach`) — the
  subprocess back-channel (`SubprocessPipeEmitter`) and reattach-listener
  (`ReattachListenerEmitter`) wire protocols.
- **Agent memory** (`agent_state`) — agent_memory JSON documents and
  message append/list operations.
- **Helpers** (`config_context`, `runtime_support`, `payload_traversal`,
  `metrics_hook`, `notification_log`, `phase_git`, `workflow_helpers`,
  `workflow_merge_recovery`, `ensure_execution_cwd`).

Heavy execution machinery (`phase_executor`, `workflow_execute`,
`phase_targets`, `phase_failover`, `phase_command`, `skill_dispatch`,
`direct_exec`) is plugin-private and intentionally NOT here.

## Versioning

This crate follows semver. The first release is `v0.1.0`. It pins to the
v0.5 baseline rev of `launchapp-dev/animus-cli` for `protocol`,
`orchestrator-core`, `orchestrator-config`, `orchestrator-store`.

## License

Elastic-2.0.
