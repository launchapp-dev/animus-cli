#!/usr/bin/env bash
set -euo pipefail

current_tag="${1:-${GITHUB_REF_NAME:-}}"
current_commit="${2:-${GITHUB_SHA:-}}"

if [[ -z "${current_tag}" || -z "${current_commit}" ]]; then
  echo "usage: $0 <release-tag> <commit>" >&2
  echo "example: $0 v0.7.0-rc.33 HEAD" >&2
  exit 2
fi

if [[ ! "${current_tag}" =~ ^(v[0-9]+\.[0-9]+\.[0-9]+)-rc\.([0-9]+)$ ]]; then
  echo "${current_tag} is not a release-candidate tag; no RC lineage check is required."
  exit 0
fi

release_prefix="${BASH_REMATCH[1]}"
current_rc=$((10#${BASH_REMATCH[2]}))
previous_tag=""
previous_rc=-1

if ! git rev-parse --verify --quiet "${current_commit}^{commit}" >/dev/null; then
  echo "${current_commit} does not resolve to a commit." >&2
  exit 2
fi

# Keep direct and CI invocations safe when the checkout is shallow or its local
# tag set is stale; ancestry checks need both RC tags and the commits between them.
if git remote get-url origin >/dev/null 2>&1; then
  if [[ "$(git rev-parse --is-shallow-repository)" == "true" ]]; then
    git fetch --quiet --unshallow --tags origin
  else
    git fetch --quiet --force origin \
      "+refs/tags/${release_prefix}-rc.*:refs/tags/${release_prefix}-rc.*"
  fi
fi

while IFS= read -r tag; do
  if [[ "${tag}" =~ ^(v[0-9]+\.[0-9]+\.[0-9]+)-rc\.([0-9]+)$ ]] \
    && [[ "${BASH_REMATCH[1]}" == "${release_prefix}" ]]; then
    candidate_rc=$((10#${BASH_REMATCH[2]}))
    if (( candidate_rc < current_rc && candidate_rc > previous_rc )); then
      previous_tag="${tag}"
      previous_rc=${candidate_rc}
    fi
  fi
done < <(git tag --list "${release_prefix}-rc.*")

if [[ -z "${previous_tag}" ]]; then
  echo "No earlier ${release_prefix} release candidate exists; lineage starts at ${current_tag}."
  exit 0
fi

if ! git merge-base --is-ancestor "${previous_tag}^{commit}" "${current_commit}^{commit}"; then
  echo "::error::${current_tag} (${current_commit}) is not descended from ${previous_tag}."
  echo "Create the release candidate from the commit carrying ${previous_tag}, then tag it again."
  exit 1
fi

echo "Release lineage verified: ${previous_tag} is an ancestor of ${current_tag}."
