#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cli_source="$repo_root/crates/orchestrator-cli/src/cli_types/root_types.rs"
cli_docs="$repo_root/docs/reference/cli/index.md"
agents_guide="$repo_root/AGENTS.md"
mcp_source_dir="$repo_root/crates/orchestrator-cli/src/services/operations/ops_mcp"
mcp_docs="$repo_root/docs/reference/mcp-tools.md"
cargo_manifest="$repo_root/Cargo.toml"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

extract_cli_source() {
  perl -ne '
    if (/pub\(crate\) enum Command \{/) {
      $in_enum = 1;
      next;
    }
    if ($in_enum && /^\}/) {
      $in_enum = 0;
    }
    if ($in_enum && /^\s*#\[command\(name = "([^"]+)"\)\]/) {
      $pending_name = $1;
      next;
    }
    if ($in_enum && /^\s+([A-Z][A-Za-z0-9_]*)\s*[({,]/) {
      if (defined $pending_name && length $pending_name) {
        print $pending_name, "\n";
      } else {
        print lc($1), "\n";
      }
      undef $pending_name;
    }
  ' "$cli_source" | sort -u
}

extract_cli_docs() {
  awk '
    /^```$/ {
      if (in_block) {
        exit
      }
      in_block=1
      next
    }
    in_block && $0 ~ /^[├└]── / {
      line=$0
      sub(/^[├└]── /, "", line)
      split(line, parts, /[[:space:]]+/)
      if (parts[1] != "" && parts[1] != "help") {
        print parts[1]
      }
    }
  ' "$cli_docs" | sort -u
}

extract_agents_cli_docs() {
  awk '
    /^Visible top-level commands:/ {
      in_section=1
      next
    }
    in_section && !started && NF == 0 {
      next
    }
    in_section && started && NF == 0 {
      exit
    }
    in_section && /^- / {
      started=1
      line=$0
      sub(/^- `/, "", line)
      sub(/`$/, "", line)
      if (line != "") {
        print line
      }
    }
  ' "$agents_guide" | sort -u
}

extract_mcp_source() {
  rg -o 'name = "animus\.[^"]+"' "$mcp_source_dir" -N \
    | sed 's/^.*name = "//' \
    | sed 's/"$//' \
    | sort -u
}

extract_mcp_docs() {
  awk -F'|' '
    /^\| `animus\./ {
      tool=$2
      gsub(/`/, "", tool)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", tool)
      print tool
    }
  ' "$mcp_docs" | sort -u
}

workspace_member_count() {
  awk '
    /^\[workspace\]/ { in_workspace=1; next }
    /^\[/ && $0 != "[workspace]" { in_workspace=0 }
    in_workspace && /^members = \[/ { in_members=1; next }
    in_members && /^\]/ { in_members=0; exit }
    in_members && /"/ { count++ }
    END { print count + 0 }
  ' "$cargo_manifest"
}

mcp_tool_count() {
  extract_mcp_source | wc -l | tr -d ' '
}

tool_family_count() {
  local family="$1"
  extract_mcp_source | awk -v family="$family" 'index($0, "animus." family ".") == 1 { count++ } END { print count + 0 }'
}

cli_version() {
  awk -F'"' '
    $1 ~ /^version = / { print $2; exit }
  ' "$repo_root/crates/orchestrator-cli/Cargo.toml"
}

check_surface() {
  local label="$1"
  local source_file="$2"
  local docs_file="$3"

  local missing_file="$tmp_dir/${label}-missing.txt"
  local extra_file="$tmp_dir/${label}-extra.txt"

  comm -23 "$source_file" "$docs_file" > "$missing_file"
  comm -13 "$source_file" "$docs_file" > "$extra_file"

  if [[ -s "$missing_file" || -s "$extra_file" ]]; then
    echo "Doc drift detected for $label."
    if [[ -s "$missing_file" ]]; then
      echo "Missing from docs:"
      sed 's/^/  - /' "$missing_file"
    fi
    if [[ -s "$extra_file" ]]; then
      echo "Present in docs but not in code:"
      sed 's/^/  - /' "$extra_file"
    fi
    return 1
  fi
}

extract_cli_source > "$tmp_dir/cli-source.txt"
extract_cli_docs > "$tmp_dir/cli-docs.txt"
extract_agents_cli_docs > "$tmp_dir/agents-cli-docs.txt"
extract_mcp_source > "$tmp_dir/mcp-source.txt"
extract_mcp_docs > "$tmp_dir/mcp-docs.txt"

check_surface "CLI command tree" "$tmp_dir/cli-source.txt" "$tmp_dir/cli-docs.txt"
check_surface "AGENTS top-level command list" "$tmp_dir/cli-source.txt" "$tmp_dir/agents-cli-docs.txt"
check_surface "MCP tool reference" "$tmp_dir/mcp-source.txt" "$tmp_dir/mcp-docs.txt"

assert_contains() {
  local file="$1"
  local pattern="$2"
  local label="$3"
  if ! rg -q --fixed-strings -- "$pattern" "$file"; then
    echo "Doc drift detected for $label."
    echo "Expected to find: $pattern"
    echo "File: $file"
    return 1
  fi
}

assert_not_contains() {
  local file="$1"
  local pattern="$2"
  local label="$3"
  if rg -q --fixed-strings -- "$pattern" "$file"; then
    echo "Doc drift detected for $label."
    echo "Unexpected stale text: $pattern"
    echo "File: $file"
    return 1
  fi
}

assert_contains \
  "$repo_root/docs/architecture/full-system-architecture.md" \
  '`animus-workflow-runner-default`' \
  "workflow runner architecture inventory"
assert_not_contains \
  "$repo_root/docs/architecture/full-system-architecture.md" \
  '| `animus-workflow-runner` | `workflow-runner-v2` |' \
  "workflow runner architecture inventory"
assert_contains \
  "$repo_root/docs/guides/ci-cd.md" \
  '`animus-workflow-runner-default`' \
  "workflow runner CI inventory"
assert_not_contains \
  "$repo_root/docs/guides/ci-cd.md" \
  '| `animus-workflow-runner` | `workflow-runner-v2` |' \
  "workflow runner CI inventory"

workspace_count="$(workspace_member_count)"
mcp_count="$(mcp_tool_count)"
skill_tool_count="$(tool_family_count "skill")"
current_cli_version="v$(cli_version)"
assert_contains \
  "$repo_root/docs/architecture/full-system-architecture.md" \
  '`Cargo.toml` currently declares '"${workspace_count}"' workspace members.' \
  "full system architecture workspace count"
assert_contains \
  "$repo_root/docs/architecture/crate-map.md" \
  "The Animus workspace is a Cargo workspace of ${workspace_count} crates organized by runtime" \
  "crate map workspace count"
assert_contains \
  "$repo_root/docs/contributing/development.md" \
  "The workspace is a Cargo workspace of ${workspace_count} crates." \
  "development guide workspace count"
assert_contains \
  "$repo_root/docs/design/acp-integration.md" \
  "- **Rust-only Cargo workspace** (${workspace_count} current workspace members)" \
  "ACP integration workspace count"
assert_contains \
  "$repo_root/docs/architecture/index.md" \
  "Animus is a Rust-only agent orchestrator built as a Cargo workspace of ${workspace_count} crates." \
  "architecture overview workspace count"
assert_contains \
  "$repo_root/docs/index.md" \
  "details: ${mcp_count} built-in MCP tools for subject management, workflow control, plugin operations, output inspection, and runtime state mutations." \
  "docs home MCP tool count"
assert_contains \
  "$repo_root/docs/guides/index.md" \
  "Complete guide to all ${mcp_count} built-in MCP tools" \
  "guides index MCP tool count"
assert_contains \
  "$repo_root/docs/guides/agents.md" \
  "Animus currently exposes **${mcp_count} built-in MCP tools** across these families:" \
  "agents guide MCP tool count"
assert_contains \
  "$repo_root/docs/reference/mcp-tools.md" \
  "## Skills (${skill_tool_count} tools)" \
  "MCP reference skill tool count"
assert_contains \
  "$repo_root/docs/guides/agents.md" \
  "| \`animus.skill.*\` | ${skill_tool_count} |" \
  "agents guide skill tool count"
assert_contains \
  "$repo_root/README.md" \
  "currently \`${current_cli_version}\` in this repo" \
  "README current CLI version"
assert_contains \
  "$repo_root/README.md" \
  "ANIMUS_VERSION=${current_cli_version}" \
  "README install snippet version"
assert_contains \
  "$repo_root/docs/getting-started/installation.md" \
  "Current workspace CLI version: **${current_cli_version}**." \
  "installation guide current CLI version"
assert_contains \
  "$repo_root/docs/getting-started/installation.md" \
  "ANIMUS_VERSION=${current_cli_version}" \
  "installation guide install snippet version"

echo "CLI command tree and MCP tool reference are in sync."
