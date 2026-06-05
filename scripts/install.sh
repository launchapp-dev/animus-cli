#!/usr/bin/env bash
set -euo pipefail

# Animus CLI Installer for macOS
# Usage: curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
#
# Pin a specific version: ANIMUS_VERSION=v0.4.12 curl ... | bash
# Latest tested release:  v0.4.12 (2026-05-24)
#
# v0.4.12 note: after installing the binary you must install at least one
# provider plugin before the daemon will start. Set ANIMUS_SKIP_PLUGIN_INSTALL=1
# to skip the post-install plugin step (e.g. on CI). Set ANIMUS_INSTALL_PLUGINS=1
# to run `animus plugin install-defaults --include-subjects --include-transports`
# automatically. When neither is set, the installer prints the command for the
# operator to run manually.

REPO="launchapp-dev/animus-cli"
INSTALL_DIR="${ANIMUS_INSTALL_DIR:-${HOME}/.local/bin}"
# v0.5.2 surface-shrink: `animus-oai-runner` moved out-of-tree to
# `launchapp-dev/animus-provider-oai-agent`. Legacy v0.4.x / v0.5.0
# archives still ship the binary; the loop below maps it back onto disk
# for upgrading users without breaking new installs.
# v0.5.3 surface-shrink: the `agent-runner` sidecar binary was deleted.
# Newer archives only ship `animus`; legacy archives still ship
# `agent-runner` but it is no longer required, so make it optional.
BINARIES=(animus)
OPTIONAL_BINARIES=(agent-runner)

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarn:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

detect_arch() {
  local arch
  arch="$(uname -m)"
  case "${arch}" in
    arm64|aarch64) echo "aarch64-apple-darwin" ;;
    x86_64)        echo "x86_64-apple-darwin" ;;
    *)             error "Unsupported architecture: ${arch}" ;;
  esac
}

detect_version() {
  if [[ -n "${ANIMUS_VERSION:-}" ]]; then
    echo "${ANIMUS_VERSION}"
    return
  fi

  local latest
  latest="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | awk -F'"' '/"tag_name"/{print $4; exit}')" || true

  if [[ -z "${latest}" ]]; then
    error "Could not determine latest release. Set ANIMUS_VERSION=vX.Y.Z to install a specific version."
  fi
  echo "${latest}"
}

verify_checksum() {
  local archive="$1" checksums="$2" expected actual
  expected="$(grep "$(basename "${archive}")" "${checksums}" | awk '{print $1}')"
  if [[ -z "${expected}" ]]; then
    warn "No checksum found for $(basename "${archive}") — skipping verification"
    return 0
  fi
  actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
  if [[ "${expected}" != "${actual}" ]]; then
    error "Checksum mismatch for $(basename "${archive}")\n  expected: ${expected}\n  actual:   ${actual}"
  fi
  info "Checksum verified"
}

main() {
  [[ "$(uname -s)" == "Darwin" ]] || error "This installer is for macOS only"

  local target version archive_name archive_url checksums_url

  target="$(detect_arch)"
  version="$(detect_version)"

  info "Installing Animus CLI ${version} for ${target}"

  # TODO(naming): Release Binaries workflow currently uploads assets as
  # `ao-${VERSION}-${TARGET}.tar.gz` (see .github/workflows/release.yml). Rename
  # both sides to `animus-*` in a coordinated release once existing tags can be
  # left untouched.
  archive_name="ao-${version}-${target}.tar.gz"
  archive_url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"
  checksums_url="https://github.com/${REPO}/releases/download/${version}/SHA256SUMS.txt"

  TMPDIR_INSTALL="$(mktemp -d)"
  trap 'rm -rf "${TMPDIR_INSTALL}"' EXIT

  info "Downloading ${archive_name}..."
  if ! curl -fSL --progress-bar -o "${TMPDIR_INSTALL}/${archive_name}" "${archive_url}"; then
    error "Download failed. Check that release ${version} exists at:\n  https://github.com/${REPO}/releases/tag/${version}"
  fi

  if curl -fsSL -o "${TMPDIR_INSTALL}/SHA256SUMS.txt" "${checksums_url}" 2>/dev/null; then
    verify_checksum "${TMPDIR_INSTALL}/${archive_name}" "${TMPDIR_INSTALL}/SHA256SUMS.txt"
  else
    warn "Could not download checksums — skipping verification"
  fi

  info "Extracting..."
  tar -xzf "${TMPDIR_INSTALL}/${archive_name}" -C "${TMPDIR_INSTALL}"

  local stage_dir="${TMPDIR_INSTALL}/ao-${version}-${target}"
  if [[ ! -d "${stage_dir}" ]]; then
    stage_dir="$(find "${TMPDIR_INSTALL}" -mindepth 1 -maxdepth 1 -type d | head -1)"
  fi

  mkdir -p "${INSTALL_DIR}"

  # As of v0.5.1 round-4 fold-in the workflow runner binary moved out of
  # this release archive to the `animus-workflow-runner-default` plugin.
  # If a v0.4.x archive is pinned via ANIMUS_VERSION it may still ship the
  # legacy `animus-workflow-runner` / `ao-workflow-runner` binary; we map
  # it back onto disk under the v0.5 plugin install dir so the daemon's
  # resolver finds it without an extra `animus plugin install` step.
  local legacy_runner=""
  if [[ -f "${stage_dir}/animus-workflow-runner" ]]; then
    legacy_runner="${stage_dir}/animus-workflow-runner"
  elif [[ -f "${stage_dir}/ao-workflow-runner" ]]; then
    legacy_runner="${stage_dir}/ao-workflow-runner"
  fi

  # v0.5.2 surface-shrink: same pattern for `animus-oai-runner` — the
  # binary moved out-of-tree to `launchapp-dev/animus-provider-oai-agent`.
  # If a v0.4.x / v0.5.0 archive is pinned via ANIMUS_VERSION it still
  # ships the binary; copy it onto disk so the daemon's runtime contract
  # resolver finds it without an extra `animus plugin install` step.
  local legacy_oai_runner=""
  if [[ -f "${stage_dir}/animus-oai-runner" ]]; then
    legacy_oai_runner="${stage_dir}/animus-oai-runner"
  fi

  for bin in "${BINARIES[@]}"; do
    if [[ ! -f "${stage_dir}/${bin}" ]]; then
      error "Binary '${bin}' not found in archive"
    fi
    rm -f "${INSTALL_DIR}/${bin}"
    cp "${stage_dir}/${bin}" "${INSTALL_DIR}/${bin}"
    chmod +x "${INSTALL_DIR}/${bin}"
    if [[ "$(uname -s)" == "Darwin" ]] && command -v codesign &>/dev/null; then
      codesign --force --sign - "${INSTALL_DIR}/${bin}" 2>/dev/null || true
    fi
  done

  # Create 'ao' as a symlink to 'animus' for backwards compatibility
  rm -f "${INSTALL_DIR}/ao"
  ln -s "${INSTALL_DIR}/animus" "${INSTALL_DIR}/ao"

  # Workflow runner: prefer the v0.5 plugin install. If the archive
  # carries the legacy runner binary, drop it next to other CLI binaries
  # so an upgrading daemon keeps dispatching while the operator runs
  # `animus plugin install launchapp-dev/animus-workflow-runner-default`.
  if [[ -n "${legacy_runner}" ]]; then
    rm -f "${INSTALL_DIR}/animus-workflow-runner"
    cp "${legacy_runner}" "${INSTALL_DIR}/animus-workflow-runner"
    chmod +x "${INSTALL_DIR}/animus-workflow-runner"
    rm -f "${INSTALL_DIR}/ao-workflow-runner"
    ln -s "${INSTALL_DIR}/animus-workflow-runner" "${INSTALL_DIR}/ao-workflow-runner"
    warn "installed legacy workflow runner; v0.5.1+ uses the plugin — run 'animus plugin install launchapp-dev/animus-workflow-runner-default'"
  fi

  # OAI runner: same shape as the workflow runner above. The binary
  # ships from `launchapp-dev/animus-provider-oai-agent` as of v0.5.2.
  if [[ -n "${legacy_oai_runner}" ]]; then
    rm -f "${INSTALL_DIR}/animus-oai-runner"
    cp "${legacy_oai_runner}" "${INSTALL_DIR}/animus-oai-runner"
    chmod +x "${INSTALL_DIR}/animus-oai-runner"
    warn "installed legacy oai runner; v0.5.2+ uses the plugin — run 'animus plugin install launchapp-dev/animus-provider-oai-agent'"
  fi

  # v0.5.3 surface-shrink: the `agent-runner` sidecar was deleted.
  # Pre-v0.5.3 archives still ship the binary; copy it onto disk for
  # operators rolling back / staying on the old layout, but emit a
  # warning so they know it's no longer required.
  for legacy_bin in "${OPTIONAL_BINARIES[@]}"; do
    if [[ -f "${stage_dir}/${legacy_bin}" ]]; then
      rm -f "${INSTALL_DIR}/${legacy_bin}"
      cp "${stage_dir}/${legacy_bin}" "${INSTALL_DIR}/${legacy_bin}"
      chmod +x "${INSTALL_DIR}/${legacy_bin}"
      warn "installed legacy ${legacy_bin}; v0.5.3+ no longer requires it (provider plugins handle agent execution end-to-end)"
    fi
  done

  info "Installed to ${INSTALL_DIR}:"
  for bin in "${BINARIES[@]}"; do
    printf '  %s\n' "${INSTALL_DIR}/${bin}"
  done
  printf '  %s → %s (symlink)\n' "${INSTALL_DIR}/ao" "animus"
  printf '\nNext step: install the v0.5 workflow runner plugin:\n'
  printf '  animus plugin install launchapp-dev/animus-workflow-runner-default\n'
  printf '\nIf you use OAI-compatible providers (MiniMax/Z.AI/OpenRouter/etc), also install:\n'
  printf '  animus plugin install launchapp-dev/animus-provider-oai-agent\n'

  if ! echo "${PATH}" | tr ':' '\n' | grep -qxF "${INSTALL_DIR}"; then
    warn "${INSTALL_DIR} is not in your PATH"
    echo ""
    echo "Add it to your shell profile:"
    echo ""
    echo "  # bash"
    echo "  echo 'export PATH=\"\${HOME}/.local/bin:\${PATH}\"' >> ~/.bashrc"
    echo ""
    echo "  # zsh"
    echo "  echo 'export PATH=\"\${HOME}/.local/bin:\${PATH}\"' >> ~/.zshrc"
    echo ""
  fi

  if command -v animus &>/dev/null; then
    info "Verifying installation..."
    animus --version
    echo ""
    info "Run 'animus doctor' in a git repo to check prerequisites"
    info "Run 'animus init' to initialize a project"
    info "'ao' is also available as an alias"
  elif command -v ao &>/dev/null; then
    info "Verifying installation..."
    ao --version
    echo ""
    info "Run 'ao doctor' in a git repo to check prerequisites"
    info "Run 'ao init' to initialize a project"
  else
    echo ""
    info "Restart your shell, then run 'animus --version' to verify"
  fi

  echo ""
  info "Prerequisites (install at least one):"
  echo "  claude  — npm install -g @anthropic-ai/claude-code"
  echo "  codex   — npm install -g @openai/codex"
  echo "  gemini  — npm install -g @google/gemini-cli"

  echo ""
  info "v0.4.12 plugin install (required before 'animus daemon start'):"
  if [[ "${ANIMUS_SKIP_PLUGIN_INSTALL:-0}" == "1" ]]; then
    info "  Skipped (ANIMUS_SKIP_PLUGIN_INSTALL=1). Run later with:"
    echo "    animus plugin install-defaults --include-subjects --include-transports"
  elif [[ "${ANIMUS_INSTALL_PLUGINS:-0}" == "1" ]] && command -v animus &>/dev/null; then
    info "Installing default plugin set..."
    animus plugin install-defaults --include-subjects --include-transports --yes || \
      warn "Plugin install failed — re-run manually with 'animus plugin install-defaults --include-subjects --include-transports'"
  else
    info "Run this once before starting the daemon:"
    echo ""
    echo "    animus plugin install-defaults --include-subjects --include-transports"
    echo ""
    info "Then verify with: animus daemon preflight"
    info "(Set ANIMUS_INSTALL_PLUGINS=1 on next install to run this automatically.)"
  fi
}

main "$@"
