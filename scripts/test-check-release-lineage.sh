#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
checker="${repo_root}/scripts/check-release-lineage.sh"
test_repo="$(mktemp -d)"
trap 'rm -rf "${test_repo}"' EXIT

git -C "${test_repo}" init --quiet
git -C "${test_repo}" config user.name "Release Lineage Test"
git -C "${test_repo}" config user.email "release-lineage-test@example.invalid"

commit_file() {
  local contents="$1"
  printf '%s\n' "${contents}" >"${test_repo}/release.txt"
  git -C "${test_repo}" add release.txt
  git -C "${test_repo}" commit --quiet -m "${contents}"
  git -C "${test_repo}" rev-parse HEAD
}

base_commit="$(commit_file base)"
git -C "${test_repo}" tag v0.7.0-rc.9 "${base_commit}"

next_commit="$(commit_file next)"
git -C "${test_repo}" tag v0.7.0-rc.10 "${next_commit}"

# Numeric ordering must select rc.10 rather than lexically sorting rc.9 later.
latest_commit="$(commit_file latest)"
lineage_output="$(
  cd "${test_repo}"
  "${checker}" v0.7.0-rc.11 "${latest_commit}"
)"
grep -Fq "v0.7.0-rc.10 is an ancestor" <<<"${lineage_output}"

# Model the default-branch auto-tag preflight: a proposal based on the old
# rc.9 line must be rejected before the tag is created or pushed.
git -C "${test_repo}" switch --quiet --detach "${base_commit}"
diverged_commit="$(commit_file diverged)"
if lineage_output="$(
  cd "${test_repo}"
  "${checker}" v0.7.0-rc.11 "${diverged_commit}" 2>&1
)"; then
  echo "expected a release candidate from a stale branch to be rejected" >&2
  exit 1
fi
grep -Fq "is not descended from v0.7.0-rc.10" <<<"${lineage_output}"

# A proposal descended from the latest RC must pass the same preflight.
git -C "${test_repo}" switch --quiet --detach "${next_commit}"
descended_commit="$(commit_file descended)"
descended_output="$(
  cd "${test_repo}"
  "${checker}" v0.7.0-rc.11 "${descended_commit}"
)"
grep -Fq "v0.7.0-rc.10 is an ancestor" <<<"${descended_output}"

stable_output="$(
  cd "${test_repo}"
  "${checker}" v0.7.0 "${diverged_commit}"
)"
grep -Fq "no RC lineage check is required" <<<"${stable_output}"

if invalid_output="$(
  cd "${test_repo}"
  "${checker}" v0.7.0-rc.11 does-not-exist 2>&1
)"; then
  echo "expected an invalid release commit to be rejected" >&2
  exit 1
fi
grep -Fq "does-not-exist does not resolve to a commit" <<<"${invalid_output}"

echo "release lineage checks passed"
