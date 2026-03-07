#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/cleanup-build-targets.sh [options]

Without flags, remove target/debug/incremental for the current repo.

Options:
  --report       Show current target size breakdown and exit unless paired with an action.
  --incremental  Remove only target/debug/incremental.
  --debug        Remove target/debug but keep target/release.
  --all          Run cargo clean for the current repo.
  --worktrees    Also prune stale ~/.ao/*/worktrees/*/target directories.
  --days N       Age threshold for --worktrees pruning. Defaults to 7.
  -h, --help     Show this help.
EOF
}

report=0
mode=""
prune_worktrees=0
days=7

while (($# > 0)); do
  case "$1" in
    --report)
      report=1
      ;;
    --incremental)
      mode="incremental"
      ;;
    --debug)
      mode="debug"
      ;;
    --all)
      mode="all"
      ;;
    --worktrees)
      prune_worktrees=1
      ;;
    --days)
      shift
      if (($# == 0)); then
        echo "missing value for --days" >&2
        exit 1
      fi
      days="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

if ! [[ "$days" =~ ^[0-9]+$ ]]; then
  echo "--days must be an integer" >&2
  exit 1
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"
cd "$repo_root"
target_dir="$repo_root/target"

if [[ -z "$mode" && "$report" -eq 0 ]]; then
  mode="incremental"
fi

size_of() {
  local path="$1"
  if [[ -e "$path" ]]; then
    du -sh "$path" 2>/dev/null | awk '{print $1}'
  else
    echo "0B"
  fi
}

print_report() {
  echo "Repo: $repo_root"
  echo "target: $(size_of "$target_dir")"
  echo "target/debug: $(size_of "$target_dir/debug")"
  echo "target/debug/deps: $(size_of "$target_dir/debug/deps")"
  echo "target/debug/incremental: $(size_of "$target_dir/debug/incremental")"
  echo "target/release: $(size_of "$target_dir/release")"
  if [[ "$prune_worktrees" -eq 1 ]]; then
    echo
    echo "Stale worktree targets older than ${days} day(s):"
    local found=0
    while IFS= read -r -d '' path; do
      found=1
      echo "$(size_of "$path")  $path"
    done < <(find "${HOME}/.ao" -type d -path '*/worktrees/*/target' -mtime +"$days" -print0 2>/dev/null)
    if [[ "$found" -eq 0 ]]; then
      echo "none"
    fi
  fi
}

remove_path() {
  local path="$1"
  if [[ ! -e "$path" ]]; then
    echo "skip: $path"
    return
  fi

  local size
  size="$(size_of "$path")"
  rm -rf "$path"
  echo "removed: $path ($size)"
}

prune_stale_worktrees() {
  local count=0
  while IFS= read -r -d '' path; do
    remove_path "$path"
    count=$((count + 1))
  done < <(find "${HOME}/.ao" -type d -path '*/worktrees/*/target' -mtime +"$days" -print0 2>/dev/null)
  echo "pruned stale worktree targets: $count"
}

if [[ "$report" -eq 1 ]]; then
  print_report
  if [[ -z "$mode" ]]; then
    exit 0
  fi
  echo
fi

case "$mode" in
  incremental)
    remove_path "$target_dir/debug/incremental"
    ;;
  debug)
    remove_path "$target_dir/debug"
    ;;
  all)
    cargo clean
    ;;
  "")
    ;;
  *)
    echo "unsupported mode: $mode" >&2
    exit 1
    ;;
esac

if [[ "$prune_worktrees" -eq 1 ]]; then
  prune_stale_worktrees
fi
