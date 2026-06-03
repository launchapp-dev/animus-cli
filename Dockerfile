# Animus daemon image — built for v0.4.4 (2026-05-21).
# Bundles the CLI + runtime binaries; install provider plugins at runtime via
# `animus plugin install launchapp-dev/animus-provider-<name>`.

# ── Stage 1: Build all daemon binaries ─────────────────────────────────────────
FROM rust:1.89-bookworm AS builder

ARG TARGETARCH=amd64
ARG BUILDARCH=amd64

WORKDIR /src

# Copy workspace and crates
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates

# Build daemon binaries with optimized release profile
# Uses workspace settings: strip=true, lto=thin, codegen-units=1, opt-level=z.
# As of the v0.5.1 round-4 fold-in the in-tree workflow runner binary was
# deleted; the daemon scheduler now spawns `animus-workflow-runner-default`
# from the installed plugin. The image installs that plugin in stage 2.
# v0.5.2 surface-shrink: the in-tree `animus-oai-runner` binary was
# deleted; the runtime now spawns
# `launchapp-dev/animus-provider-oai-agent` v0.1.3 from the installed
# plugin. The image installs that plugin in stage 2 as well.
RUN cargo build --release --locked \
    -p orchestrator-cli \
    -p agent-runner

# Verify binaries exist
RUN ls -lh \
    target/release/animus \
    target/release/agent-runner

# ── Stage 2: Minimal runtime image ──────────────────────────────────────────────
FROM debian:bookworm-slim

# Install runtime dependencies + Node.js
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    openssh-client \
    openssl \
    unzip \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

# Install AI coding tools
RUN npm install -g @anthropic-ai/claude-code @openai/codex \
    && npm cache clean --force

# Install OpenCode (tarball asset; pattern changed upstream to
# opencode-linux-<arch>.tar.gz where <arch> is x86_64 or arm64 —
# NOT the dpkg names amd64/arm64. Translate amd64 → x86_64; arm64 stays.)
RUN ARCH=$(dpkg --print-architecture | sed 's/^amd64$/x86_64/') \
    && curl -fsSL "https://github.com/opencode-ai/opencode/releases/latest/download/opencode-linux-${ARCH}.tar.gz" -o /tmp/opencode.tar.gz \
    && tar -xzf /tmp/opencode.tar.gz -C /usr/local/bin/ opencode \
    && chmod +x /usr/local/bin/opencode \
    && rm /tmp/opencode.tar.gz

# Create Animus state directory + plugin install root
RUN mkdir -p /root/.animus /root/.animus/plugins

# Copy binaries from builder
COPY --from=builder /src/target/release/animus /usr/local/bin/animus
COPY --from=builder /src/target/release/agent-runner /usr/local/bin/agent-runner

# Install the workflow runner plugin. As of v0.5.1 round-4 fold-in this
# binary is the out-of-tree `animus-workflow-runner-default` plugin and
# the daemon's `resolve_workflow_runner_binary` looks for it in
# `~/.animus/plugins/`. The image MUST ship this plugin — without it the
# daemon's preflight refuses to start. Fail the image build hard so a
# transient release-download error never produces a broken image.
# v0.5.2 surface-shrink: same pattern for `animus-provider-oai-agent`
# (formerly bundled as the in-tree `animus-oai-runner` binary). Without
# the plugin the runtime contract resolver falls back to a bare PATH
# lookup that fails inside the slim base image.
RUN animus plugin install launchapp-dev/animus-workflow-runner-default --yes \
    && test -x /root/.animus/plugins/animus-workflow-runner-default \
    && animus plugin install launchapp-dev/animus-provider-oai-agent --yes \
    && test -x /root/.animus/plugins/animus-provider-oai-agent/bin/animus-oai-runner

# Create working directory
WORKDIR /workspace

# Expose daemon port (for web server if enabled)
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD animus status 2>/dev/null || exit 1

# Default entrypoint
ENTRYPOINT ["animus"]
CMD ["daemon", "start"]
