# Quick Start Guide

## Build

```bash
cd cli-wrapper
cargo build --release
```

Binary location: `target/release/cli-wrapper`

## Quick Test

1. **Discover CLIs**:
   ```bash
   ./target/release/cli-wrapper discover
   ```

2. **List installed CLIs**:
   ```bash
   ./target/release/cli-wrapper list
   ```

3. **Run health check**:
   ```bash
   ./target/release/cli-wrapper health
   ```

4. **Test a CLI** (e.g., Claude):
   ```bash
   ./target/release/cli-wrapper test claude
   ```

5. **Show CLI info**:
   ```bash
   ./target/release/cli-wrapper info claude
   ```

## Common Commands

### Test all CLIs with basic suite
```bash
./target/release/cli-wrapper test
```

### Test specific CLI with verbose logging
```bash
./target/release/cli-wrapper --verbose test codex
```

### Run file operations test suite
```bash
./target/release/cli-wrapper test --suite file-ops
```

### Health check for all CLIs
```bash
./target/release/cli-wrapper health
```

## Setup

### For Claude Code
```bash
export ANTHROPIC_API_KEY="your-api-key"
```

### For Codex
```bash
codex login
```

### For Gemini
```bash
export GEMINI_API_KEY="your-api-key"
# OR
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/credentials.json"
```

## Example Output

```bash
$ ./target/release/cli-wrapper list

Installed CLIs:
────────────────────────────────────────────────────────────
Claude Code     ✓ Available
OpenAI Codex    ⚠ Not Authenticated
Google Gemini   ✗ Not Installed
Aider           ✓ Available
```

```bash
$ ./target/release/cli-wrapper test claude

Running test suite: Basic CLI Verification
────────────────────────────────────────────────────────────
✓ PASS version_check - Claude Code (45ms)
✓ PASS auth_check - Claude Code (12ms)
✓ PASS simple_echo - Claude Code (234ms)

────────────────────────────────────────────────────────────
Summary: 3/3 tests passed
```

## Integration with Tauri App

You can integrate this as a library in your Tauri app:

```toml
# In your Cargo.toml
[dependencies]
cli-wrapper = { path = "../cli-wrapper" }
```

```rust
use cli_wrapper::{CliRegistry, CliTester};

async fn test_clis() -> anyhow::Result<()> {
    let mut registry = CliRegistry::new();
    registry.discover_clis().await?;

    let tester = CliTester::new();
    let results = tester.health_check_all(&registry).await?;

    Ok(())
}
```

## Troubleshooting

**Problem**: "CLI not found"
```bash
# Make sure the CLI is in PATH
which claude
which codex
```

**Problem**: "Not authenticated"
```bash
# Set environment variables or run login
export ANTHROPIC_API_KEY="..."
codex login
```

**Problem**: Compilation errors
```bash
# Update Rust
rustup update

# Clean and rebuild
cargo clean
cargo build --release
```

## Next Steps

- See [README.md](README.md) for full documentation
- Check [config.example.toml](config.example.toml) for configuration options
- Run `./target/release/cli-wrapper --help` for all commands
