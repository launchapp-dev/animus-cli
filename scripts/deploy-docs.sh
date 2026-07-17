#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

PATH="$repo_root/node_modules/.bin:$PATH"

bash scripts/check-doc-sync.sh

source "$repo_root/scripts/docs-env.sh"

vitepress_build_log="$(mktemp "${TMPDIR:-/tmp}/animus-vitepress.XXXXXX")"
if ! bash scripts/build-docs.sh >"$vitepress_build_log" 2>&1; then
  if rg -q '@rollup/rollup-darwin-arm64|ERR_DLOPEN_FAILED|Team IDs' "$vitepress_build_log"; then
    echo "Local VitePress build is blocked by the known Rollup macOS native-module signing issue." >&2
    echo "Continuing to Vercel so the remote build can validate and publish the docs." >&2
  else
    cat "$vitepress_build_log" >&2
    exit 1
  fi
fi
rm -f "$vitepress_build_log"

echo "Deploying docs with Vercel..."
echo "Prerequisites: network access for Vercel and a valid Vercel login."

vercel_bin=""
vercel_runner=()

if command -v vercel >/dev/null 2>&1; then
  vercel_bin="$(command -v vercel)"
  vercel_runner=("$vercel_bin" --yes --prod)
else
  npx_bin="$(resolve_node_package_manager_bin npx)" || {
    echo "Unable to find npx. Install Node tooling or expose an existing Node install." >&2
    exit 1
  }
  vercel_bin="$npx_bin"
  vercel_runner=("$npx_bin" vercel --yes --prod)
fi

npx_cache_dir="$(mktemp -d "${TMPDIR:-/tmp}/animus-vercel.XXXXXX")"
trap 'rm -rf "$npx_cache_dir"' EXIT
vercel_deploy_log="$(mktemp "${TMPDIR:-/tmp}/animus-vercel-deploy.XXXXXX")"
trap 'rm -rf "$npx_cache_dir"; rm -f "$vercel_deploy_log"' EXIT
timeout_bin="$(resolve_timeout_bin)" || {
  echo "Unable to find timeout/gtimeout. Install coreutils or expose timeout in PATH." >&2
  exit 1
}
vercel_timeout_seconds="${ANIMUS_VERCEL_TIMEOUT_SECONDS:-300}"
echo "Using Vercel command via $vercel_bin."
echo "Using temporary npm cache at $npx_cache_dir."
echo "Bounding Vercel deploy to ${vercel_timeout_seconds}s via $timeout_bin."

set +e
CI=1 \
npm_config_cache="$npx_cache_dir" \
npm_config_fetch_retries=0 \
npm_config_fetch_timeout=10000 \
npm_config_fetch_retry_maxtimeout=10000 \
  "$timeout_bin" --foreground "${vercel_timeout_seconds}s" \
  "${vercel_runner[@]}" >"$vercel_deploy_log" 2>&1
vercel_exit_code=$?
set -e

cat "$vercel_deploy_log"

if [[ $vercel_exit_code -eq 124 ]]; then
  echo "Vercel deploy timed out after ${vercel_timeout_seconds}s." >&2
  echo "Increase ANIMUS_VERCEL_TIMEOUT_SECONDS if the site legitimately needs more time." >&2
  exit 124
fi

if [[ $vercel_exit_code -ne 0 ]]; then
  echo "Vercel deploy failed with exit code $vercel_exit_code." >&2
  exit "$vercel_exit_code"
fi
