# Installation

## Build from Source

AO is a Rust workspace. You need a working Rust toolchain (1.75+ recommended).

```bash
# Clone the repository
git clone https://github.com/your-org/ao-cli.git
cd ao-cli

# Build all runtime binaries (release mode)
cargo ao-bin-build-release

# Or build in debug mode for development
cargo ao-bin-build
```

The build produces the `ao` binary along with supporting binaries (agent-runner, workflow-runner).

## Run Directly (Development)

If you want to run without installing:

```bash
cargo run -p orchestrator-cli -- --help
```

This compiles and runs the CLI in one step. Useful during development.

## Check Binaries

Verify that all required binaries are present and correctly linked:

```bash
cargo ao-bin-check
```

## Release Binaries

Pre-built binaries are available on the [GitHub Releases](https://github.com/your-org/ao-cli/releases) page for the following targets:

| Target | Platform |
|--------|----------|
| `x86_64-unknown-linux-gnu` | Linux (x86_64) |
| `x86_64-apple-darwin` | macOS (Intel) |
| `aarch64-apple-darwin` | macOS (Apple Silicon) |
| `x86_64-pc-windows-msvc` | Windows (x86_64) |

Download the appropriate archive, extract it, and place the `ao` binary on your `PATH`.

## Verify Installation

```bash
# Check the installed version
ao --version

# Run environment diagnostics
ao doctor
```

`ao doctor` checks for required dependencies, verifies configuration, and reports any issues. Use `ao doctor --fix` to attempt automatic remediation of common problems.

## Prerequisites

AO orchestrates AI CLI tools. Depending on which agents and models you use, you may need one or more of:

- [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli) (`claude`)
- [Codex CLI](https://github.com/openai/codex) (`codex`)
- [Gemini CLI](https://github.com/google-gemini/gemini-cli) (`gemini`)

These are not required to install AO itself, but workflows that invoke AI agents will need the appropriate CLI tool available on your `PATH`.

## Next Steps

Once installed, proceed to the [Quick Start](quick-start.md) to configure your first project.
