#!/usr/bin/env bash

resolve_node_package_manager_bin() {
  local bin_name="$1"
  local node_bin
  local sibling
  local candidate

  if command -v "$bin_name" >/dev/null 2>&1; then
    command -v "$bin_name"
    return 0
  fi

  if command -v node >/dev/null 2>&1; then
    node_bin="$(command -v node)"
    sibling="$(dirname "$node_bin")/$bin_name"
    if [[ -x "$sibling" ]]; then
      echo "$sibling"
      return 0
    fi
  fi

  for candidate in \
    "$HOME/.nvm/versions/node"/*/bin/"$bin_name" \
    "$HOME/.volta/bin/$bin_name" \
    "$HOME/.fnm"/*/bin/"$bin_name" \
    "$HOME/.asdf/shims/$bin_name" \
    /opt/homebrew/bin/"$bin_name" \
    /usr/local/bin/"$bin_name"
  do
    if [[ -x "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done

  return 1
}

resolve_timeout_bin() {
  if command -v timeout >/dev/null 2>&1; then
    command -v timeout
    return 0
  fi

  if command -v gtimeout >/dev/null 2>&1; then
    command -v gtimeout
    return 0
  fi

  return 1
}
