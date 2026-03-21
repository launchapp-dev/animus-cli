# Release v0.0.19

## Fixes

- **Critical: Atomic orphan tracker writes prevent daemon crash/corruption** — Replaced non-atomic `fs::write` with `tempfile`+`rename` pattern in cleanup.rs, and added exclusive lock guard for the full read-modify-write cycle in ops_runner.rs. Both code paths now use the same `.lock` file on the shared CLI tracker, preventing corrupt or lost entries when the daemon crashes or during concurrent scheduler ticks.

## Improvements

- **Daemon reliability hardening** — Improved crash safety for the agent-runner orphan tracker with atomic file operations
- **Concurrency safety** — Added `fs2` exclusive lock guard to protect the Cleanup handler's read-modify-write cycle

## Testing

- Added unit tests for atomic write, round-trip, and crash safety in cleanup.rs

---

**Full Changelog**: https://github.com/AudioGenius-ai/ao-cli/compare/v0.0.18...v0.0.19
