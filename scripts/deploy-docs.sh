#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_command() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command '$cmd' is not installed or not on PATH" >&2
    exit 127
  fi
}

require_command npm
require_command npx

cd "$repo_root"

npm run docs:check-sync
npm run docs:build
echo "Deploying docs with Vercel..."
echo "Prerequisites: npm registry access and a valid Vercel login."
npx_cache_dir="$(mktemp -d "${TMPDIR:-/tmp}/animus-vercel.XXXXXX")"
trap 'rm -rf "$npx_cache_dir"' EXIT
echo "Using npx vercel for the production deploy."
echo "Using temporary npm cache at $npx_cache_dir."
npm_config_cache="$npx_cache_dir" \
npm_config_fetch_retries=0 \
npm_config_fetch_timeout=10000 \
npm_config_fetch_retry_maxtimeout=10000 \
  npx vercel --yes --prod
