#!/usr/bin/env bash
set -euo pipefail

# AO CLI Installer for macOS
# Usage: curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao-cli/main/scripts/install.sh | bash

REPO="launchapp-dev/ao"
INSTALL_DIR="${AO_INSTALL_DIR:-${HOME}/.local/bin}"
BINARIES=(ao agent-runner llm-cli-wrapper ao-oai-runner ao-workflow-runner)

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
  if [[ -n "${AO_VERSION:-}" ]]; then
    echo "${AO_VERSION}"
    return
  fi

  local latest
  latest="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | awk -F'"' '/"tag_name"/{print $4; exit}')" || true

  if [[ -z "${latest}" ]]; then
    error "Could not determine latest release. Set AO_VERSION=vX.Y.Z to install a specific version."
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

  info "Installing AO CLI ${version} for ${target}"

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

  info "Installed to ${INSTALL_DIR}:"
  for bin in "${BINARIES[@]}"; do
    printf '  %s\n' "${INSTALL_DIR}/${bin}"
  done

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

  if command -v ao &>/dev/null; then
    info "Verifying installation..."
    ao --version
    echo ""
    info "Run 'ao doctor' in a git repo to check prerequisites"
    info "Run 'ao setup' to initialize a project"
  else
    echo ""
    info "Restart your shell, then run 'ao --version' to verify"
  fi

  echo ""
  info "Prerequisites (install at least one):"
  echo "  claude  — npm install -g @anthropic-ai/claude-code"
  echo "  codex   — npm install -g @openai/codex"
  echo "  gemini  — npm install -g @google/gemini-cli"
}

main "$@"
