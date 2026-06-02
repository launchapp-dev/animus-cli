# Contributing to Animus

Thank you for your interest in contributing to Animus! This document provides guidelines and instructions for contributing.

## Getting Started

### Prerequisites

- **Rust**: Animus is a Rust-only project. Install Rust via [rustup](https://rustup.rs/).
- **Node.js** (optional, for web UI work): v18+
- **At least one AI coding CLI** for testing agent integration:
  - `@anthropic-ai/claude-code` (recommended)
  - `@openai/codex`
  - `@google/gemini-cli`

### Building Locally

```bash
# Clone the repository
git clone https://github.com/launchapp-dev/animus-cli.git
cd animus-cli

# Build the project
cargo build

# Run tests
cargo test

# Format and lint
cargo animus-fmt
cargo animus-lint
```

## Development Workflow

### Before You Start

1. **Check existing issues and PRs** to avoid duplicate work
2. **Fork the repository** on GitHub
3. **Create a feature branch** from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```

### Making Changes

- Keep changes focused and minimal — one feature per PR
- Follow the existing code style (Rust conventions)
- Add tests for new functionality
- Update documentation if your changes affect user-facing behavior
- Reference the [CLAUDE.md](./CLAUDE.md) file for architecture landmarks and verification checks

### Code Organization

The workspace is organized into functional crates:

- **Core orchestration**: `orchestrator-cli`, `orchestrator-core`, `orchestrator-config`, `orchestrator-store`
- **Runtime & agents**: `agent-runner`, `workflow-runner-v2`, `orchestrator-daemon-runtime`, `orchestrator-session-host`
- **Plugin host**: `orchestrator-plugin-host`, `animus-plugin-protocol`, `animus-plugin-runtime`
- **Utilities**: `orchestrator-providers`, `orchestrator-git-ops`, `orchestrator-notifications`, `protocol`
- **Web stack**: shipped as external plugins (`launchapp-dev/animus-transport-http`, `animus-transport-graphql`, `animus-web-ui`). Install with `animus plugin install-defaults --include-transports`.

### Running Tests

```bash
# Test a specific crate
cargo test -p crate-name

# Run all tests
cargo test --workspace
```

### Verifying Your Changes

Before submitting a PR, verify your changes don't break anything:

```bash
# Format check
cargo animus-fmt-check

# Lint check
cargo animus-lint

# Test all crates
cargo test --workspace
```

## Submitting Changes

### Pull Request Process

1. **Push your branch** to your fork:
   ```bash
   git push origin feature/your-feature-name
   ```

2. **Create a Pull Request** with a clear title and description:
   - Link any related issues using `Closes #123`
   - Explain *why* the change is needed, not just *what* changed
   - Include testing notes if applicable

3. **Respond to feedback** from reviewers and update the PR as needed

4. **Ensure CI passes** — all automated checks must pass before merging

### Commit Guidelines

- Write clear, descriptive commit messages
- Use conventional commit format when possible: `type(scope): description`
  - `feat`: new feature
  - `fix`: bug fix
  - `refactor`: code restructuring
  - `test`: test additions or updates
  - `docs`: documentation updates
  - `chore`: maintenance tasks

Example:
```
feat(cli): add --dry-run flag to task create command

Allows users to preview task creation without persisting state.
```

## Documentation

- **CLI changes**: Update `docs/reference/cli/index.md`
- **MCP tools**: Update `docs/reference/mcp-tools.md` and `docs/guides/agents.md`
- **Configuration**: Update relevant docs in `docs/reference/`
- **README**: Keep it current with major feature additions

## Naming Conventions

Animus has a stable naming split between the user-facing brand (`animus`) and the wire/protocol/storage namespace (`ao`). When adding MCP tools, environment variables, CLI commands, pack ids, plugin repos, or state files, follow the rules documented in [Naming Contract](docs/architecture/naming-contract.md).

## Code Standards

### Rust Style

- Use `cargo fmt` for formatting — this is enforced in CI
- Follow Clippy suggestions — address warnings before submitting
- Write idiomatic Rust code

### Web UI Standards (TypeScript/React)

- If you change the external web UI plugins, follow the React and frontend patterns in those plugin repos
- Follow component patterns established in the codebase
- Test changes with responsive and accessibility checks

### State Management

- Treat Animus-managed state (project `.animus/` config and `~/.animus/<repo-scope>/` runtime state) as immutable except through CLI commands
- Use service APIs rather than direct file manipulation
- Preserve backward compatibility where possible

## Reporting Bugs

When reporting bugs, please include:

1. **Environment**: OS, Rust version, Animus version
2. **Steps to reproduce**: Clear, minimal example
3. **Expected behavior**: What should happen
4. **Actual behavior**: What actually happens
5. **Logs**: Output of `animus doctor` and relevant error messages

## Questions or Feedback?

- **Discussions**: Use GitHub Discussions for questions
- **Issues**: File issues for bugs or feature requests
- **Security**: For security issues, email security@example.com (do not open public issues)

## License

By contributing to Animus, you agree that your contributions will be licensed under the same [Elastic License 2.0 (ELv2)](LICENSE) as the project.

---

Thank you for contributing to Animus!
