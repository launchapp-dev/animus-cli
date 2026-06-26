#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

PATH="$repo_root/node_modules/.bin:$PATH"

bash scripts/check-doc-sync.sh

if command -v vitepress >/dev/null 2>&1; then
  rm -rf docs/.vitepress/.temp docs/.vitepress/cache
  vitepress build docs
else
  echo "vitepress not found. Install docs dependencies first (for example: npm install)." >&2
  exit 1
fi

echo "Deploying docs with Vercel..."
echo "Prerequisites: npm registry access and a valid Vercel login."
npx_cache_dir="$(mktemp -d "${TMPDIR:-/tmp}/animus-vercel.XXXXXX")"
trap 'rm -rf "$npx_cache_dir"' EXIT
echo "Using npx vercel for the production deploy."
echo "Using temporary npm cache at $npx_cache_dir."
if ! command -v npx >/dev/null 2>&1; then
  echo "npx not found. Install npm/npx or run from an environment that provides it." >&2
  exit 1
fi
npm_config_cache="$npx_cache_dir" \
npm_config_fetch_retries=0 \
npm_config_fetch_timeout=10000 \
npm_config_fetch_retry_maxtimeout=10000 \
  npx vercel --yes --prod
