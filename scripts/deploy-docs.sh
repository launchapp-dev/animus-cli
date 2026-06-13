#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

npm run docs:check-sync
npm run docs:build
echo "Deploying docs with Vercel..."
echo "Prerequisites: npm registry access and a valid Vercel login."
if [ -x "./node_modules/.bin/vercel" ]; then
  ./node_modules/.bin/vercel --yes --prod
else
  echo "Local vercel CLI not found in node_modules/.bin; npx will download it from npm."
  npm_config_fetch_retries=0 \
  npm_config_fetch_timeout=10000 \
  npm_config_fetch_retry_maxtimeout=10000 \
    npx vercel --yes --prod
fi
