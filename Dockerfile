# Animus daemon image — built for v0.6.1 (2026-06-21).
# Bundles the `animus` CLI binary and installs the curated default flavor's
# REQUIRED-role plugins at build time so the daemon passes preflight and boots:
# config_source (animus-config-yaml — required as of v0.6), workflow_runner,
# queue, a provider (claude), and both subject backends. Extra providers/
# subjects/transports can be added at runtime via `animus plugin install`.

# ── Stage 1: Build all daemon binaries ─────────────────────────────────────────
# Match rust-toolchain.toml (1.96.0). The kernel uses APIs (e.g.
# Duration::from_mins) that are not available on older toolchains.
FROM rust:1.96-bookworm AS builder

ARG TARGETARCH=amd64
ARG BUILDARCH=amd64

WORKDIR /src

# System build deps. `libdbus-sys` (pulled transitively by the keyring /
# secret-service layer behind `animus secret`) needs the dbus dev headers +
# pkg-config to compile on Linux.
RUN apt-get update && apt-get install -y --no-install-recommends \
    libdbus-1-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace and crates
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
# Root-level files embedded into the binary via include_str!: the curated
# flavor manifest (plugin_registry / scope) and the RBAC policy doc
# (orchestrator-core principal + daemon-runtime policy).
COPY flavors flavors
COPY docs/architecture/multi-tenant-rbac-v0.5.5.md docs/architecture/multi-tenant-rbac-v0.5.5.md

# Build daemon binaries with optimized release profile
# Uses workspace settings: strip=true, lto=thin, codegen-units=1, opt-level=z.
# As of the v0.5.1 round-4 fold-in the in-tree workflow runner binary was
# deleted; the daemon scheduler now spawns `animus-workflow-runner-default`
# from the installed plugin. The image installs that plugin in stage 2.
# v0.5.2 surface-shrink: the in-tree `animus-oai-runner` binary was
# deleted; the runtime now spawns
# `launchapp-dev/animus-provider-oai-agent` v0.1.3 from the installed
# plugin. The image installs that plugin in stage 2 as well.
# v0.5.3 surface-shrink: the in-tree `agent-runner` sidecar was deleted;
# `animus agent {run, status, cancel, control}` now talks to provider
# plugins directly via `SessionBackendResolver`, so the image no longer
# ships an `agent-runner` binary.
RUN cargo build --release --locked \
    -p orchestrator-cli

# Verify binaries exist
RUN ls -lh \
    target/release/animus

# ── Stage 2: Minimal runtime image ──────────────────────────────────────────────
# Trixie (glibc 2.41), not bookworm (glibc 2.36): several launchapp-dev plugin
# release binaries are built against glibc 2.38/2.39 and won't load on bookworm.
# The `animus` binary built on the bookworm builder (glibc 2.36) runs fine here
# (glibc is backward compatible).
FROM debian:trixie-slim

# Install runtime dependencies + Node.js
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    libdbus-1-3 \
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

# Install the curated default flavor's REQUIRED-role plugins. As of v0.6 the
# daemon refuses to start unless every required role is satisfied — including
# `config_source` (the kernel no longer parses YAML in-process; it sources
# WorkflowConfig from the `animus-config-yaml` plugin). `install-defaults`
# reads `flavors/default.toml` and installs the required set: provider (claude),
# both subject backends, transport-http, workflow_runner, queue, and
# config_source. `--include-oai-agent` adds the OpenAI-compatible agent harness.
# Fail the build hard if config_source or workflow_runner are absent so a
# transient release-download error never produces an unbootable image.
RUN animus plugin install-defaults --yes --include-oai-agent \
    && animus plugin list | grep -q animus-config-yaml \
    && animus plugin list | grep -q animus-workflow-runner-default \
    && animus plugin list | grep -q animus-queue-default \
    && animus plugin list | grep -q animus-subject-default

# Create working directory
WORKDIR /workspace

# Expose daemon port (for web server if enabled)
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD animus status 2>/dev/null || exit 1

# Default entrypoint. Use the FOREGROUND `daemon run` (not `daemon start`,
# which detaches as a background process and would make the container exit
# immediately). `daemon run` performs the same plugin preflight; the required
# plugins were installed above so it boots.
ENTRYPOINT ["animus"]
CMD ["daemon", "run"]
