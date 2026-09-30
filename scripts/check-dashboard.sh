#!/usr/bin/env bash
# Serve the dashboard from the pinned transport + web UI plugins, against a
# running daemon, in a throwaway HOME, with an animus binary built from this
# checkout. Checks that:
#
# - the web UI serves the real dashboard build, not the "build the UI first"
#   placeholder;
# - the dashboard's own query, sent to the web UI's /graphql, comes back with
#   data and no errors (the web UI proxies it to the GraphQL transport,
#   which asks the daemon);
# - both the web UI and the GraphQL transport refuse a cross-site request.
#
# So this fails when a pin in crates/orchestrator-core/src/plugin_registry.rs
# points at a transport or web UI release that can't serve the dashboard
# against this CLI's daemon.
#
# Uses ports 8080-8082 (the plugins' defaults).
#
# Usage: scripts/check-dashboard.sh <path-to-animus>
set -euo pipefail

animus_arg="${1:?usage: scripts/check-dashboard.sh <path-to-animus>}"
animus="$(cd "$(dirname "${animus_arg}")" && pwd)/$(basename "${animus_arg}")"

# Keep the path short: the daemon's control socket lives under
# $HOME/.animus/<scope>/, and Unix socket paths are capped at ~104 bytes
# (macOS's default $TMPDIR is already long enough to exceed it).
work="$(mktemp -d /tmp/animus-dashboard.XXXXXX)"
export HOME="${work}/home"
project="${work}/project"
serve_pid=""

cleanup() {
  if [[ -n "${serve_pid}" ]]; then
    kill "${serve_pid}" 2>/dev/null || true
    wait "${serve_pid}" 2>/dev/null || true
  fi
  "${animus}" daemon stop --project-root "${project}" >/dev/null 2>&1 || true
  rm -rf "${work}"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  for log in "${work}/serve.log" "${work}/serve.json"; do
    [[ -s "${log}" ]] && { echo "--- ${log##*/}" >&2; cat "${log}" >&2; }
  done
  exit 1
}

# Poll until the command succeeds, for up to $1 seconds.
wait_for() {
  local seconds="$1"
  shift
  for _ in $(seq "${seconds}"); do
    "$@" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}

mkdir -p "${HOME}" "${project}"
git -C "${project}" init -q
git -C "${project}" -c user.name=ci -c user.email=ci@example.invalid commit -q --allow-empty -m init

echo "== animus plugin install-defaults --include-transports"
"${animus}" plugin install-defaults --include-transports --yes --project-root "${project}" >"${work}/install.json"
cat "${work}/install.json"

echo "== animus daemon start"
"${animus}" daemon start --project-root "${project}"
wait_for 60 "${animus}" daemon health --project-root "${project}" || fail "daemon did not become healthy"

echo "== animus web serve"
"${animus}" web serve --project-root "${project}" --json >"${work}/serve.json" 2>"${work}/serve.log" &
serve_pid=$!
wait_for 60 curl -fsS http://127.0.0.1:8082/healthz || fail "web UI did not come up on 8082"
wait_for 30 curl -fsS http://127.0.0.1:8081/healthz || fail "GraphQL transport did not come up on 8081"

echo "== dashboard page"
page="$(curl -fsS http://127.0.0.1:8082/)"
if grep -q "build the UI first" <<<"${page}"; then
  fail "web UI serves the placeholder page: the release has no embedded dashboard build"
fi
grep -q '<script' <<<"${page}" || fail "web UI page has no script tag: ${page:0:200}"

echo "== dashboard query through the web UI"
# The dashboard page's own query (animus-web-ui src/lib/graphql/operations/dashboard.graphql).
query='query Dashboard {
  subject(kind: "task") { id status priority }
  daemon { running version projectRoot }
  daemonHealth { healthy status plugins { name kind status } }
  daemonAgents { sessionId provider model workflowId phaseId }
  queueStats { total ready held inFlight }
}'
body="$(python3 -c 'import json, sys; print(json.dumps({"query": sys.argv[1]}))' "${query}")"

dashboard_query() {
  local response
  response="$(curl -sS -X POST -H 'content-type: application/json' --data "${body}" http://127.0.0.1:8082/graphql)" || return 1
  python3 - "${response}" <<'PY'
import json, sys
response = json.loads(sys.argv[1])
errors = response.get("errors")
if errors:
    sys.exit(f"errors: {json.dumps(errors)[:500]}")
daemon = (response.get("data") or {}).get("daemon") or {}
if daemon.get("running") is not True:
    sys.exit(f"daemon not reported running: {daemon}")
print(f"daemon {daemon.get('version')} running; queue {response['data']['queueStats']}")
PY
}

# `daemon health` passes once the process is up, but the control socket the
# GraphQL transport talks to opens later, so retry until the daemon answers.
if ! wait_for 120 dashboard_query; then
  dashboard_query || fail "dashboard query failed"
fi
dashboard_query

echo "== cross-site requests are refused"
for port in 8082 8081; do
  status="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
    -H 'content-type: application/json' -H 'origin: https://evil.example' \
    --data '{"query":"{ __typename }"}' "http://127.0.0.1:${port}/graphql")"
  [[ "${status}" == "403" ]] || fail "port ${port} answered a cross-site request with ${status}, expected 403"
done

echo "dashboard OK"
