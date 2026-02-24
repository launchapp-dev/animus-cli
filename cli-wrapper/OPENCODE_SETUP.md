# OpenCode Configuration and Setup

## Overview

OpenCode is configured with **197 models** across **5 providers**, making it the most flexible CLI in your arsenal.

## Your Configuration

### Provider Summary

| Provider | Models | Best For |
|----------|--------|----------|
| **zai-coding-plan** | 16 | 🎯 **Autonomous coding tasks** (RECOMMENDED) |
| **z.ai** | 16 | General coding assistance |
| **minimax** | 2 | General purpose, MiniMax-M2.1 |
| **opencode** | 11 | Free tier models |
| **openrouter** | 152 | Access to Claude, GPT, DeepSeek, Gemini |

---

## Recommended Models for Agent Orchestrator

### 🥇 Primary: zai-coding-plan/glm-4.7

**Why this is perfect for your autonomous agents:**

✅ **Takes Real Actions** - Doesn't just suggest code, actually writes files
✅ **Specialized for Coding** - Trained specifically for development workflows
✅ **Plan Execution** - Can break down and implement multi-step tasks
✅ **Cost Effective** - Very affordable compared to Claude/GPT
✅ **Privacy Focused** - Runs through OpenCode's privacy layer

**Command Format**:
```bash
opencode run -m zai-coding-plan/glm-4.7 "your coding task"
```

**Example Output**:
```bash
$ opencode run -m zai-coding-plan/glm-4.7 "Create a hello world function in Python"

I'll create a simple hello world function in Python.
Resolving dependencies
Resolved, downloaded and extracted [6]
Saved lockfile
| Write    cli-wrapper/hello.py
Created `hello.py` with a simple hello world function.
```

**What it created**:
```python
def hello_world():
    return "Hello, World!"

if __name__ == "__main__":
    print(hello_world())
```

### 🥈 Secondary: minimax/MiniMax-M2.1

**Good for:**
- Quick queries
- Math/logic problems
- General coding questions
- Lower latency needs

**Command Format**:
```bash
opencode run -m minimax/MiniMax-M2.1 "your question"
```

### 🥉 Fallback: openrouter/* models

When you need:
- **Vision**: `openrouter/anthropic/claude-sonnet-4.5`
- **Max Intelligence**: `openrouter/anthropic/claude-opus-4.5`
- **Max Context**: `openrouter/google/gemini-2.5-pro`
- **Reasoning**: `openrouter/deepseek/deepseek-r1`
- **Free tier**: Many `*:free` models available

---

## CLI Wrapper Integration

### Updated Support

The CLI wrapper now supports model selection via environment variable:

```rust
// When creating a CliCommand
let command = CliCommand::new("Create a function")
    .with_env("OPENCODE_MODEL".to_string(), "zai-coding-plan/glm-4.7".to_string());
```

### Usage in Agent Orchestrator

**For PM Loop (Planning)**:
```rust
let plan_model = "zai-coding-plan/glm-4.7";
let command = CliCommand::new("Analyze this codebase and create an implementation plan")
    .with_env("OPENCODE_MODEL".to_string(), plan_model.to_string());
```

**For EM Loop (Implementation)**:
```rust
let impl_model = "zai-coding-plan/glm-4.7";
let command = CliCommand::new("Implement the authentication middleware")
    .with_env("OPENCODE_MODEL".to_string(), impl_model.to_string());
```

**For Quick Queries**:
```rust
let query_model = "minimax/MiniMax-M2.1";
let command = CliCommand::new("What's the time complexity of binary search?")
    .with_env("OPENCODE_MODEL".to_string(), query_model.to_string());
```

---

## Available Models by Category

### z.ai-coding-plan (Specialized Coding)
```
zai-coding-plan/glm-4.5         - Base model
zai-coding-plan/glm-4.5-air     - Lightweight variant
zai-coding-plan/glm-4.5-flash   - Fast responses
zai-coding-plan/glm-4.5v        - Vision capable
zai-coding-plan/glm-4.6         - Improved base
zai-coding-plan/glm-4.6v        - Vision + improvements
zai-coding-plan/glm-4.7         - Latest, most capable ⭐
zai-coding-plan/glm-4.7-flash   - Latest + fast
```

### z.ai (General Purpose)
Same models as above, but without coding specialization

### minimax
```
minimax/MiniMax-M2              - Base model
minimax/MiniMax-M2.1            - Latest version ⭐
```

### opencode (Free Tier)
```
opencode/big-pickle             - Large general model
opencode/glm-4.7-free           - Free z.ai
opencode/gpt-5-nano             - Efficient GPT
opencode/grok-code              - Code specialist
opencode/kimi-k2.5-free         - Free long-context
opencode/minimax-m2.1-free      - Free MiniMax ⭐
opencode/trinity-large-preview-free - Large free model
```

### openrouter (152 models)

**Anthropic Claude**:
- `openrouter/anthropic/claude-sonnet-4.5` - Best balance
- `openrouter/anthropic/claude-opus-4.5` - Most capable
- `openrouter/anthropic/claude-haiku-4.5` - Fast/cheap

**DeepSeek** (many free!):
- `openrouter/deepseek/deepseek-r1:free` - Reasoning
- `openrouter/deepseek/deepseek-v3.2` - Latest general
- `openrouter/deepseek/deepseek-chat-v3.1` - Chat optimized

**Google Gemini**:
- `openrouter/google/gemini-2.5-pro` - Maximum intelligence
- `openrouter/google/gemini-2.5-flash` - Fast responses
- `openrouter/google/gemini-2.0-flash-exp:free` - Free tier

*See full list with: `opencode models`*

---

## Performance Testing

### Response Times

| Model | Simple Query | Code Generation | File Operations |
|-------|-------------|-----------------|-----------------|
| zai-coding-plan/glm-4.7 | ~2-3s | ~3-5s | ~4-6s + writes files |
| minimax/MiniMax-M2.1 | ~1-2s | ~3-4s | N/A |
| Default (auto) | ~3-4s | ~4-6s | Variable |

### File Creation Capability

Only **zai-coding-plan** models actually write files:

```bash
✅ zai-coding-plan/glm-4.7 → Creates files
❌ zai/glm-4.7              → Suggests code only
❌ minimax/MiniMax-M2.1     → Suggests code only
✅ openrouter/anthropic/*   → Can use tools (via API)
```

---

## Integration Strategy for Agent Orchestrator

### Recommended Usage Pattern

```
┌─────────────────────────────────────────┐
│         Task Classification             │
└─────────────────────────────────────────┘
                  │
        ┌─────────┴──────────┐
        │                    │
        ▼                    ▼
    Planning            Implementation
        │                    │
        ├─ Complex           ├─ File Creation
        │  zai-coding-plan   │  zai-coding-plan/glm-4.7
        │                    │
        ├─ Simple            ├─ Code Review
        │  minimax/M2.1      │  claude-sonnet-4.5
        │                    │
        └─ Vision            └─ Refactoring
           claude-sonnet-4.5    zai-coding-plan/glm-4.7
```

### Cost Optimization

1. **Development Tasks** (80%): zai-coding-plan/glm-4.7
   - File creation
   - Implementation
   - Refactoring
   - Testing

2. **Quick Queries** (15%): minimax/MiniMax-M2.1 or free models
   - Documentation lookup
   - Quick questions
   - Syntax help

3. **Premium Features** (5%): OpenRouter Claude/Gemini
   - Vision analysis
   - Very complex reasoning
   - Maximum quality requirements

---

## Testing Your Setup

### Basic Test
```bash
opencode run -m zai-coding-plan/glm-4.7 "Create a factorial function in Python"
```

### Check Available Models
```bash
opencode models | grep -E "zai-coding|minimax"
```

### Test File Creation
```bash
opencode run -m zai-coding-plan/glm-4.7 "Create a REST API endpoint for user login"
```

### Performance Test
```bash
time opencode run -m minimax/MiniMax-M2.1 "What is 123 * 456?"
```

---

## Configuration Files

OpenCode stores configuration in:
- `~/.opencode/` - Main installation
- Models configured via API keys and provider settings

To see current configuration:
```bash
opencode models        # List all models
opencode auth          # Check authentication
```

---

## Recommendations Summary

### ✅ DO

1. **Use zai-coding-plan/glm-4.7 for autonomous tasks**
   - File creation
   - Implementation work
   - Multi-step plans

2. **Use minimax/MiniMax-M2.1 for quick queries**
   - Fast responses needed
   - Simple questions
   - Cost optimization

3. **Use OpenRouter for special features**
   - Vision analysis
   - Maximum intelligence
   - Specific model requirements

### ❌ DON'T

1. Don't use premium models (Claude Opus 4.5) for simple tasks
2. Don't use non-coding-plan z.ai for file operations
3. Don't use vision models for text-only tasks

---

## Next Steps

1. **Test Integration**
   ```bash
   cd cli-wrapper
   cargo build --release
   ./target/release/cli-wrapper test opencode --suite basic
   ```

2. **Configure Default Model**
   Set `OPENCODE_MODEL=zai-coding-plan/glm-4.7` in your agent orchestrator

3. **Monitor Performance**
   Track which model performs best for your specific workloads

4. **Cost Analysis**
   Compare costs between zai-coding-plan vs OpenRouter Claude

---

## Conclusion

Your OpenCode setup is **perfectly configured** for autonomous agent work:

- ✅ 197 models available
- ✅ zai-coding-plan for file creation
- ✅ minimax for quick queries
- ✅ OpenRouter for premium features
- ✅ Free tier options available

**Primary Recommendation**: Use `zai-coding-plan/glm-4.7` as your default model for the Agent Orchestrator's autonomous loops. It's the only one that actually writes files without additional API configuration.
