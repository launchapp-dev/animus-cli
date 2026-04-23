# Installation

## Fast Path: Upstream Installer

Use the installer published from `launchapp-dev/ao`:

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash
```

Options:

```bash
# Install a specific release
AO_VERSION=v0.0.11 curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash

# Install into a custom directory
AO_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash
```

The upstream installer currently targets macOS. On Linux and Windows, use a release archive or build from source.

## Release Archives

Prebuilt releases are published at:

- <https://github.com/launchapp-dev/ao/releases>

Download the archive for your platform, extract it, and place these binaries on your `PATH`:

- `animus`
- `ao` (compatibility alias)
- `agent-runner`
- `llm-cli-wrapper`
- `ao-oai-runner`
- `ao-workflow-runner`

Supported release targets:

| Target | Platform |
|--------|----------|
| `aarch64-apple-darwin` | macOS (Apple Silicon) |
| `x86_64-apple-darwin` | macOS (Intel) |
| `x86_64-unknown-linux-gnu` | Linux (x86_64) |
| `x86_64-pc-windows-msvc` | Windows (x86_64) |

## Build From Source

```bash
git clone https://github.com/launchapp-dev/ao.git
cd ao

# Verify the runtime binaries
cargo ao-bin-check

# Debug build
cargo ao-bin-build

# Release build
cargo ao-bin-build-release
```

To run the CLI directly during development:

```bash
cargo run -p orchestrator-cli -- --help
```

## Verify Installation

```bash
animus --version
animus doctor
```

Run `animus doctor` inside a git repository to verify the local environment and Animus prerequisites.

## Prerequisites

Animus itself is a Rust application, but autonomous workflows need at least one supported AI coding CLI on your `PATH`:

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- [OpenAI Codex CLI](https://github.com/openai/codex)
- [Gemini CLI](https://github.com/google-gemini/gemini-cli)

Example installs:

```bash
npm install -g @anthropic-ai/claude-code
npm install -g @openai/codex
npm install -g @google/gemini-cli
```

## Next Steps

Proceed to the [Quick Start](quick-start.md) to initialize a repository and run the first workflow.
