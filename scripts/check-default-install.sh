#!/usr/bin/env bash
# Install the default plugin set into a throwaway HOME with an animus binary
# built from this checkout, then run `animus daemon preflight`.
#
# Preflight probes the installed queue for generation-fenced leases and the
# installed workflow runner for execution fences, the same checks
# `animus daemon run` makes before it starts. So this fails when a pin in
# crates/orchestrator-core/src/plugin_registry.rs points at a plugin the
# daemon can't run with (for example animus-queue-default v0.3.3).
#
# Usage: scripts/check-default-install.sh <path-to-animus>
set -euo pipefail

animus_arg="${1:?usage: scripts/check-default-install.sh <path-to-animus>}"
animus="$(cd "$(dirname "${animus_arg}")" && pwd)/$(basename "${animus_arg}")"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
export HOME="${work}/home"
project="${work}/project"
mkdir -p "${HOME}" "${project}"
git -C "${project}" init -q
git -C "${project}" -c user.name=ci -c user.email=ci@example.invalid commit -q --allow-empty -m init

echo "== animus plugin install-defaults"
"${animus}" plugin install-defaults --yes --project-root "${project}" >"${work}/install.json"
cat "${work}/install.json"

echo "== animus daemon preflight"
"${animus}" daemon preflight --project-root "${project}"
