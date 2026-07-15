#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

PATH="$repo_root/node_modules/.bin:$PATH"

bash scripts/check-doc-sync.sh

resolve_node_package_manager_bin() {
  local bin_name="$1"
  local node_bin
  local sibling
  local candidate

  if command -v "$bin_name" >/dev/null 2>&1; then
    command -v "$bin_name"
    return 0
  fi

  if command -v node >/dev/null 2>&1; then
    node_bin="$(command -v node)"
    sibling="$(dirname "$node_bin")/$bin_name"
    if [[ -x "$sibling" ]]; then
      echo "$sibling"
      return 0
    fi
  fi

  for candidate in \
    "$HOME/.nvm/versions/node"/*/bin/"$bin_name" \
    "$HOME/.volta/bin/$bin_name" \
    "$HOME/.fnm"/*/bin/"$bin_name" \
    "$HOME/.asdf/shims/$bin_name" \
    /opt/homebrew/bin/"$bin_name" \
    /usr/local/bin/"$bin_name"
  do
    if [[ -x "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done

  return 1
}

if command -v vitepress >/dev/null 2>&1; then
  rm -rf docs/.vitepress/.temp docs/.vitepress/cache
  vitepress_build_log="$(mktemp "${TMPDIR:-/tmp}/animus-vitepress.XXXXXX")"
  if ! vitepress build docs >"$vitepress_build_log" 2>&1; then
    if rg -q '@rollup/rollup-darwin-arm64|ERR_DLOPEN_FAILED|Team IDs' "$vitepress_build_log"; then
      echo "Local VitePress build is blocked by the known Rollup macOS native-module signing issue." >&2
      echo "Continuing to Vercel so the remote build can validate and publish the docs." >&2
    else
      cat "$vitepress_build_log" >&2
      exit 1
    fi
  fi
  rm -f "$vitepress_build_log"
else
  echo "vitepress not found. Install docs dependencies first (for example: npm install)." >&2
  exit 1
fi

echo "Deploying docs with Vercel..."
echo "Prerequisites: network access for Vercel and a valid Vercel login."

npx_bin="$(resolve_node_package_manager_bin npx)" || {
  echo "Unable to find npx. Install Node tooling or expose an existing Node install." >&2
  exit 1
}

npx_cache_dir="$(mktemp -d "${TMPDIR:-/tmp}/animus-vercel.XXXXXX")"
trap 'rm -rf "$npx_cache_dir"' EXIT
echo "Using npx via $npx_bin."
echo "Using temporary npm cache at $npx_cache_dir."
npm_config_cache="$npx_cache_dir" \
npm_config_fetch_retries=0 \
npm_config_fetch_timeout=10000 \
npm_config_fetch_retry_maxtimeout=10000 \
  "$npx_bin" vercel --yes --prod
