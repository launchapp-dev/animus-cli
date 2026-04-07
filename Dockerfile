# ── Stage 1: Build all daemon binaries ─────────────────────────────────────────
FROM rust:1.85-bookworm AS builder

ARG TARGETARCH=amd64
ARG BUILDARCH=amd64

WORKDIR /src

# Copy workspace and crates
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates

# Build daemon binaries with optimized release profile
# Uses workspace settings: strip=true, lto=thin, codegen-units=1, opt-level=z
RUN cargo build --release --locked \
    -p orchestrator-cli \
    -p agent-runner \
    -p llm-cli-wrapper

# Verify binaries exist
RUN ls -lh target/release/animus target/release/agent-runner target/release/llm-cli-wrapper

# ── Stage 2: Minimal runtime image ──────────────────────────────────────────────
FROM debian:bookworm-slim

# Install minimal runtime dependencies
# ca-certificates: for HTTPS/TLS
# openssl: for cryptographic operations
# git: for git operations (protocol requirements)
# openssh-client: for SSH-based git operations
# curl: for HTTP requests
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    openssh-client \
    openssl \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

# Create .ao directory for state
RUN mkdir -p /root/.ao

# Copy binaries from builder
COPY --from=builder /src/target/release/animus /usr/local/bin/animus
COPY --from=builder /src/target/release/agent-runner /usr/local/bin/agent-runner
COPY --from=builder /src/target/release/llm-cli-wrapper /usr/local/bin/llm-cli-wrapper

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
