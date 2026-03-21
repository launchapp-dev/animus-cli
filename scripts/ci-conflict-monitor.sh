#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="${1:-.}"

QUEUE_CACHE=""
load_queue() {
  QUEUE_CACHE=$(ao queue list --project-root "$PROJECT_ROOT" --json 2>/dev/null || echo "[]")
}

is_queued() {
  local subject_id="$1"
  echo "$QUEUE_CACHE" | jq -e --arg sid "$subject_id" '[.[] | select(.subject_id == $sid)] | length > 0' >/dev/null 2>&1
}

check_ci_failures() {
  local failed_runs
  failed_runs=$(gh run list --limit 20 --json databaseId,headBranch,name,conclusion \
    --jq '[.[] | select(.conclusion == "failure")] | .[0:5]')

  local count
  count=$(echo "$failed_runs" | jq 'length')
  if [[ "$count" -eq 0 ]]; then
    echo "CI: No recent failures"
    return
  fi

  echo "CI: Found $count failed runs"

  echo "$failed_runs" | jq -c '.[]' | while read -r run; do
    local run_id branch workflow
    run_id=$(echo "$run" | jq -r '.databaseId')
    branch=$(echo "$run" | jq -r '.headBranch')
    workflow=$(echo "$run" | jq -r '.name')

    if is_queued "ci-$run_id"; then
      echo "  Skip run $run_id ($workflow on $branch) — already queued"
      continue
    fi

    local logs
    logs=$(gh run view "$run_id" --log-failed 2>&1 | grep -i "error\|fail\|FAIL\|ERR" | head -10 || true)

    ao queue enqueue \
      --project-root "$PROJECT_ROOT" \
      --workflow-ref investigate-ci-failure \
      --subject-id "ci-$run_id" \
      --input "{\"run_id\": \"$run_id\", \"branch\": \"$branch\", \"workflow\": \"$workflow\", \"errors\": $(echo "$logs" | jq -Rs .)}" 2>/dev/null \
      && echo "  Enqueued investigation for run $run_id ($workflow on $branch)" \
      || echo "  Failed to enqueue run $run_id"
  done
}

check_pr_conflicts() {
  local prs
  prs=$(gh pr list --state open --json number,headRefName,baseRefName,mergeable --jq '[.[] | select(.mergeable == "CONFLICTING")]')

  local count
  count=$(echo "$prs" | jq 'length')
  if [[ "$count" -eq 0 ]]; then
    echo "Conflicts: No conflicting PRs"
    return
  fi

  echo "Conflicts: Found $count PRs with merge conflicts"

  echo "$prs" | jq -c '.[]' | while read -r pr; do
    local pr_num branch base
    pr_num=$(echo "$pr" | jq -r '.number')
    branch=$(echo "$pr" | jq -r '.headRefName')
    base=$(echo "$pr" | jq -r '.baseRefName')

    if is_queued "rebase-pr-$pr_num"; then
      echo "  Skip PR #$pr_num ($branch → $base) — already queued"
      continue
    fi

    ao queue enqueue \
      --project-root "$PROJECT_ROOT" \
      --workflow-ref rebase-and-retry \
      --subject-id "rebase-pr-$pr_num" \
      --input "{\"branch\": \"$branch\", \"base\": \"$base\", \"pr\": $pr_num}" 2>/dev/null \
      && echo "  Enqueued rebase for PR #$pr_num ($branch onto $base)" \
      || echo "  Failed to enqueue rebase for PR #$pr_num"
  done
}

echo "=== CI & Conflict Monitor ==="
load_queue
check_ci_failures
echo ""
check_pr_conflicts
echo "=== Done ==="
