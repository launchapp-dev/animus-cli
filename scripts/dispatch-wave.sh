#!/usr/bin/env bash
# dispatch-wave.sh
#
# Create per-agent git worktrees so parallel sub-agents stop stomping each
# other's working tree. Each worktree branches from current HEAD, lives at
# /tmp/animus-wt/<agent>, and has its own branch agent/<wave>/<agent>.
#
# Usage:
#   ./scripts/dispatch-wave.sh create <wave> <agent1> [<agent2> ...]
#       Refuses if working tree dirty. Creates one worktree per agent.
#       Prints a per-agent table the orchestrator pastes into each prompt.
#
#   ./scripts/dispatch-wave.sh list [<wave>]
#       Show all agent worktrees (filter by wave if given).
#
#   ./scripts/dispatch-wave.sh merge <wave>
#       Merge all branches from <wave> back to main (one --no-ff merge per
#       agent, in argv order). Aborts on conflict so you can resolve.
#
#   ./scripts/dispatch-wave.sh cleanup <wave>
#       After successful merge: remove worktrees + delete agent branches.
#       Refuses to remove a worktree with uncommitted changes.
#
#   ./scripts/dispatch-wave.sh prune
#       Best-effort cleanup of stale worktrees (git worktree prune + remove
#       any /tmp/animus-wt/* dirs whose branch is already merged into main).
#
# Flags (any subcommand):
#   --shared-target          Set CARGO_TARGET_DIR=<repo>/target/wt-shared
#                            in each worktree's environment. Saves ~3GB per
#                            agent but introduces cargo build-lock contention
#                            when agents compile concurrently. Default off.
#   --root <PATH>            Worktree root (default: /tmp/animus-wt).
#   --help                   Print this header.

set -euo pipefail

# -----------------------------------------------------------------------------
# Parse flags + subcommand
# -----------------------------------------------------------------------------
SHARED_TARGET=0
WT_ROOT="/tmp/animus-wt"
SUBCOMMAND=""
ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --shared-target) SHARED_TARGET=1; shift ;;
        --root)          WT_ROOT="$2"; shift 2 ;;
        --help|-h)       sed -n '2,35p' "$0"; exit 0 ;;
        create|list|merge|cleanup|prune)
            SUBCOMMAND="$1"; shift
            ARGS=("$@")
            break
            ;;
        *)
            echo "ERROR: unknown arg '$1'. Try --help."
            exit 2
            ;;
    esac
done

if [[ -z "$SUBCOMMAND" ]]; then
    echo "ERROR: missing subcommand. Try --help."
    exit 2
fi

REPO_ROOT="$(git rev-parse --show-toplevel)" || {
    echo "ERROR: not inside a git repo"
    exit 1
}

mkdir -p "$WT_ROOT"

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
agent_branch() {
    local wave="$1" agent="$2"
    echo "agent/${wave}/${agent}"
}

agent_wt_path() {
    local agent="$1"
    echo "${WT_ROOT}/${agent}"
}

# -----------------------------------------------------------------------------
# create <wave> <agent1> [<agent2> ...]
# -----------------------------------------------------------------------------
cmd_create() {
    if [[ ${#ARGS[@]} -lt 2 ]]; then
        echo "ERROR: create needs <wave> <agent1> [<agent2> ...]"
        exit 2
    fi
    local wave="${ARGS[0]}"
    local agents=("${ARGS[@]:1}")

    # Pre-flight: dirty working tree means agents would inherit half-done work.
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "ERROR: working tree dirty. Commit or stash before creating worktrees."
        echo "       Worktree branches inherit current HEAD, so dirty state would silently leak."
        git status --short | head
        exit 1
    fi

    local head_sha
    head_sha="$(git rev-parse HEAD)"
    echo "Creating ${#agents[@]} worktrees from HEAD ($head_sha):"
    echo ""

    # Table header
    printf '%-20s  %-50s  %s\n' "AGENT" "WORKTREE_PATH" "BRANCH"
    printf '%-20s  %-50s  %s\n' "-----" "-------------" "------"

    for agent in "${agents[@]}"; do
        local branch wt_path
        branch="$(agent_branch "$wave" "$agent")"
        wt_path="$(agent_wt_path "$agent")"

        if [[ -d "$wt_path" ]]; then
            echo "WARN: $wt_path exists; skipping. Remove with: ./scripts/dispatch-wave.sh cleanup $wave"
            continue
        fi
        if git show-ref --verify --quiet "refs/heads/$branch"; then
            echo "WARN: branch $branch exists; skipping. Force-delete with: git branch -D $branch"
            continue
        fi

        git worktree add -b "$branch" "$wt_path" HEAD >/dev/null
        printf '%-20s  %-50s  %s\n' "$agent" "$wt_path" "$branch"
    done

    echo ""
    echo "Hand each agent the table row matching its name. Each agent prompt MUST start with:"
    echo ""
    echo "  Work DIRECTLY in <WORKTREE_PATH> (cd at startup). Commit on the current"
    echo "  branch (already <BRANCH>). Do not touch ${REPO_ROOT}/ or any"
    echo "  sibling worktree under ${WT_ROOT}/. Push is handled by the orchestrator"
    echo "  via 'dispatch-wave.sh merge'."

    if [[ $SHARED_TARGET -eq 1 ]]; then
        echo ""
        echo "SHARED CARGO_TARGET_DIR: instruct each agent to prepend:"
        echo "  export CARGO_TARGET_DIR=${REPO_ROOT}/target/wt-shared"
        echo "Saves ~3GB per worktree but cargo lock contends when agents build concurrently."
    fi
}

# -----------------------------------------------------------------------------
# list [<wave>]
# -----------------------------------------------------------------------------
cmd_list() {
    local filter_wave="${ARGS[0]:-}"
    printf '%-20s  %-50s  %-40s  %s\n' "AGENT" "WORKTREE" "BRANCH" "STATUS"
    printf '%-20s  %-50s  %-40s  %s\n' "-----" "--------" "------" "------"
    git worktree list --porcelain | awk '
        /^worktree/ { wt = $2 }
        /^branch refs\/heads\// { gsub("refs/heads/", "", $2); print wt " " $2 }
    ' | while read -r wt branch; do
        if [[ "$branch" =~ ^agent/ ]]; then
            if [[ -n "$filter_wave" && ! "$branch" =~ ^agent/${filter_wave}/ ]]; then
                continue
            fi
            local agent
            agent="${branch##*/}"
            local status="clean"
            if [[ -n "$(cd "$wt" && git status --porcelain 2>/dev/null)" ]]; then
                status="DIRTY"
            elif ! git merge-base --is-ancestor "$branch" main 2>/dev/null; then
                status="unmerged"
            else
                status="merged"
            fi
            printf '%-20s  %-50s  %-40s  %s\n' "$agent" "$wt" "$branch" "$status"
        fi
    done
}

# -----------------------------------------------------------------------------
# merge <wave>
# -----------------------------------------------------------------------------
cmd_merge() {
    if [[ ${#ARGS[@]} -lt 1 ]]; then
        echo "ERROR: merge needs <wave>"
        exit 2
    fi
    local wave="${ARGS[0]}"

    cd "$REPO_ROOT"
    git checkout main >/dev/null 2>&1 || { echo "ERROR: cannot checkout main"; exit 1; }

    local merged=0 conflicts=()
    while read -r branch; do
        [[ -z "$branch" ]] && continue
        echo ""
        echo "==> merging $branch"
        if git merge --no-ff --no-edit "$branch"; then
            merged=$((merged + 1))
        else
            echo "CONFLICT on $branch — aborting this merge so you can resolve manually."
            git merge --abort 2>/dev/null || true
            conflicts+=("$branch")
        fi
    done < <(git branch --list "agent/${wave}/*" --format='%(refname:short)' | sort)

    echo ""
    echo "Merged: $merged"
    if [[ ${#conflicts[@]} -gt 0 ]]; then
        echo "Conflicts (resolve then 'git merge --no-ff' manually):"
        printf '  %s\n' "${conflicts[@]}"
        exit 1
    fi
    echo "All wave $wave branches merged into main."
    echo "Next: ./scripts/dispatch-wave.sh cleanup $wave"
}

# -----------------------------------------------------------------------------
# cleanup <wave>
# -----------------------------------------------------------------------------
cmd_cleanup() {
    if [[ ${#ARGS[@]} -lt 1 ]]; then
        echo "ERROR: cleanup needs <wave>"
        exit 2
    fi
    local wave="${ARGS[0]}"

    while read -r branch; do
        [[ -z "$branch" ]] && continue
        local agent wt_path
        agent="${branch##*/}"
        wt_path="$(agent_wt_path "$agent")"

        if [[ -d "$wt_path" ]]; then
            if [[ -n "$(cd "$wt_path" && git status --porcelain 2>/dev/null)" ]]; then
                echo "REFUSING to remove $wt_path — has uncommitted changes. Inspect manually."
                continue
            fi
            git worktree remove "$wt_path" 2>/dev/null && echo "  removed worktree $wt_path"
        fi

        if git merge-base --is-ancestor "$branch" main 2>/dev/null; then
            git branch -d "$branch" 2>/dev/null && echo "  deleted branch $branch"
        else
            echo "WARN: branch $branch is NOT merged into main; not deleting. Force with: git branch -D $branch"
        fi
    done < <(git branch --list "agent/${wave}/*" --format='%(refname:short)')

    git worktree prune 2>/dev/null || true
}

# -----------------------------------------------------------------------------
# prune
# -----------------------------------------------------------------------------
cmd_prune() {
    git worktree prune
    echo "Pruned stale worktree metadata. Existing worktrees:"
    git worktree list
}

# -----------------------------------------------------------------------------
# Dispatch
# -----------------------------------------------------------------------------
case "$SUBCOMMAND" in
    create)  cmd_create ;;
    list)    cmd_list ;;
    merge)   cmd_merge ;;
    cleanup) cmd_cleanup ;;
    prune)   cmd_prune ;;
    *)
        echo "ERROR: unknown subcommand '$SUBCOMMAND'"
        exit 2
        ;;
esac
