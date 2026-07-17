#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

PATH="$repo_root/node_modules/.bin:$PATH"

build_docs_once() {
  local vitepress_build_log="$1"

  rm -rf docs/.vitepress/.temp docs/.vitepress/.cache
  vitepress build docs >"$vitepress_build_log" 2>&1
}

if ! command -v vitepress >/dev/null 2>&1; then
  echo "vitepress not found. Install docs dependencies first (for example: npm install)." >&2
  exit 1
fi

vitepress_build_log="$(mktemp "${TMPDIR:-/tmp}/animus-vitepress.XXXXXX")"
trap 'rm -f "$vitepress_build_log"' EXIT

if ! build_docs_once "$vitepress_build_log"; then
  if rg -q "ERR_MODULE_NOT_FOUND.*docs/.vitepress/.temp.*\\.md\\.js|Cannot find module '.*/docs/.vitepress/.temp/.*\\.md\\.js'" "$vitepress_build_log"; then
    echo "VitePress hit the transient missing-temp-module render failure; retrying once after a clean temp/cache reset." >&2
    if ! build_docs_once "$vitepress_build_log"; then
      cat "$vitepress_build_log" >&2
      exit 1
    fi
  elif rg -q '@rollup/rollup-darwin-arm64|ERR_DLOPEN_FAILED|Team IDs' "$vitepress_build_log"; then
    echo "Local VitePress build is blocked by the known Rollup macOS native-module signing issue." >&2
    echo "Continuing is only supported in the Vercel deploy wrapper so the remote build can validate and publish the docs." >&2
    cat "$vitepress_build_log" >&2
    exit 1
  else
    cat "$vitepress_build_log" >&2
    exit 1
  fi
fi

cat "$vitepress_build_log"
