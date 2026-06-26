#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

PATH="$repo_root/node_modules/.bin:$PATH"

bash scripts/check-doc-sync.sh

resolve_vercel_cli() {
  if command -v npx >/dev/null 2>&1; then
    echo "npx"
    return 0
  fi

  if [[ -x "$repo_root/node_modules/.bin/vercel" ]]; then
    echo "$repo_root/node_modules/.bin/vercel"
    return 0
  fi

  local cached_vercel
  cached_vercel="$(find "$HOME/.npm/_npx" -path '*/node_modules/.bin/vercel' 2>/dev/null | tail -n 1 || true)"
  if [[ -n "$cached_vercel" && -x "$cached_vercel" ]]; then
    echo "$cached_vercel"
    return 0
  fi

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

vercel_cli="$(resolve_vercel_cli)" || {
  echo "Unable to find a usable Vercel CLI. Install npm/npx or ensure a cached/local vercel binary exists." >&2
  exit 1
}

if [[ "$vercel_cli" == "npx" ]]; then
  npx_cache_dir="$(mktemp -d "${TMPDIR:-/tmp}/animus-vercel.XXXXXX")"
  trap 'rm -rf "$npx_cache_dir"' EXIT
  echo "Using npx vercel for the production deploy."
  echo "Using temporary npm cache at $npx_cache_dir."
  npm_config_cache="$npx_cache_dir" \
  npm_config_fetch_retries=0 \
  npm_config_fetch_timeout=10000 \
  npm_config_fetch_retry_maxtimeout=10000 \
    npx vercel --yes --prod
else
  echo "Using cached/local Vercel CLI at $vercel_cli."
  "$vercel_cli" --yes --prod
fi
