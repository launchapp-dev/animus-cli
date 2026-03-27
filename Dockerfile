# ── Stage 1: Build AO from source ─────────────────────────────────────────────
FROM rust:1.85-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates

RUN cargo build --release \
    -p orchestrator-cli \
    -p agent-runner \
    -p llm-cli-wrapper \
    -p oai-runner \
    -p workflow-runner-v2

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    openssh-client \
    openssl \
    && rm -rf /var/lib/apt/lists/*

# Node.js 22 — required for Claude Code, Codex, and Gemini CLIs
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Agent CLIs
RUN npm install -g \
    @anthropic-ai/claude-code \
    @openai/codex \
    @google/gemini-cli

# AO binaries
COPY --from=builder /src/target/release/ao                  /usr/local/bin/ao
COPY --from=builder /src/target/release/agent-runner         /usr/local/bin/agent-runner
COPY --from=builder /src/target/release/llm-cli-wrapper      /usr/local/bin/llm-cli-wrapper
COPY --from=builder /src/target/release/oai-runner           /usr/local/bin/oai-runner
COPY --from=builder /src/target/release/ao-workflow-runner   /usr/local/bin/ao-workflow-runner

RUN mkdir -p /root/.ao

VOLUME ["/root/.ao", "/workspace"]

WORKDIR /workspace

ENTRYPOINT ["ao"]
CMD ["--help"]
