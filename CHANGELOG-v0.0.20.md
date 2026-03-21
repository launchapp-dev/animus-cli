# Changelog - v0.0.20

## Release Date
2026-03-21

## Overview
This patch release includes critical reliability improvements to the agent-runner and daemon, preventing process leaks and data corruption during concurrent crash scenarios.

---

## Fixes

### Daemon Reliability
- **Atomic orphan tracker writes** - Replace non-atomic `fs::write` with `tempfile+rename` in cleanup.rs under the existing exclusive lock. This prevents corrupt or lost tracker entries on daemon crash or concurrent scheduler ticks.
- **Exclusive lock guard for Cleanup handler** - Added `fs2` exclusive lock guard to the ops_runner.rs Cleanup handler's read-modify-write cycle. Both code paths now use the same `.lock` file on the shared CLI tracker.

---

## Dependencies

### Updated
- `agent-runner/Cargo.toml`: Added `tempfile = "3"` dependency
- `orchestrator-cli/Cargo.toml`: Promoted `fs2` from dev-dep to dependency

---

## Testing
- Added unit tests for atomic write, round-trip, and crash safety in cleanup.rs

---

## Contributors
- Sami Shukri

---

**Full Changelog**: [v0.0.19...v0.0.20](https://github.com/launchapp-dev/ao-cli/compare/v0.0.19...v0.0.20)
