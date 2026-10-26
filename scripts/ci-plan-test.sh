#!/usr/bin/env bash
# Offline integration tests; each case uses disposable Git history.
# Usage: bash scripts/ci-plan-test.sh
set -euo pipefail

script=$(cd "$(dirname "$0")" && pwd)/ci-plan.sh
temp=$(mktemp -d)
trap 'rm -rf "$temp"' EXIT
git -c init.defaultBranch=base -c init.templateDir= init -q "$temp/repo"
cd "$temp/repo"
git config user.name 'CI plan test'
git config user.email 'ci-plan@example.invalid'
git config commit.gpgsign false
git config core.hooksPath /dev/null
git config core.autocrlf false
git commit -q --allow-empty -m 'Initial base'
base=$(git rev-parse HEAD)
export GITHUB_STEP_SUMMARY="$temp/summary"
passed=0

reset_case() {
  git checkout -q --detach "$base"
  git clean -fdq
  : > "$GITHUB_STEP_SUMMARY"
}

commit() {
  local path=$1 message=$2
  mkdir -p "$(dirname "$path")"
  printf 'change\n' >> "$path"
  git add -- "$path"
  printf '%s\n' "$message" | git -c commit.cleanup=verbatim commit -q -F -
}

expect() {
  local expected=$1 label=$2 event=${3:-pull_request} output reason
  output=$(bash "$script" "$event" "$base" HEAD)
  if [[ "$output" != "skip=$expected"$'\nreason='* ]]; then
    printf 'FAIL: %s\n%s\n' "$label" "$output" >&2
    exit 1
  fi
  reason=${output#*$'\nreason='}
  grep -Fq "Skip: \`$expected\`" "$GITHUB_STEP_SUMMARY"
  grep -Fq "Reason: $reason" "$GITHUB_STEP_SUMMARY"
  passed=$((passed + 1))
  printf 'PASS: %s\n' "$label"
}

reset_case
commit docs/guide.md 'Documentation[#110]: Update the guide'
expect true 'documentation keyword'
expect false 'push always runs' push
expect false 'manual dispatch always runs' workflow_dispatch
expect false 'other events always run' schedule

reset_case
commit .github/settings/rules.json $'Maintenance[#110]: Update settings\n\n**Summary**\n\nSettings only.\n\n[SKIP BUILD]'
expect true 'settings marker'

reset_case
commit scripts/helper.sh $'Maintenance[#110]: Update helper\n\n[SKIP BUILD]\r\n\n \t\r\n'
expect true 'CR ignored and trailing blank lines ignored'
commit docs/guide.md 'Documentation[#110]: Update guide'
expect true 'every commit qualifies through either rule'
commit docs/other.md 'Maintenance[#110]: Unmarked follow-up'
expect false 'unmarked follow-up forces full run'

reset_case
commit docs/guide.md 'Maintenance[#110]: Unmarked first commit'
commit docs/other.md 'Documentation[#110]: Update guide'
expect false 'keyword at head cannot hide an earlier unmarked commit'

for message in \
  'documentation[#110]: Wrong case' \
  'Documentation[#x]: Invalid issue number' \
  'Prefix Documentation[#110]: Not at start' \
  $'Maintenance[#110]: Wrong marker case\n\n[skip build]' \
  $'Maintenance[#110]: Marker followed by text\n\n[SKIP BUILD]\nMore text' \
  $'Maintenance[#110]: Marker padded\n\n [SKIP BUILD]' \
  $'Maintenance[#110]: Marker padded\n\n[SKIP BUILD] ' \
  'Maintenance[#110]: Subject includes [SKIP BUILD]'; do
  reset_case
  commit docs/guide.md "$message"
  expect false "exact keyword or final marker required: ${message%%$'\n'*}"
done

for path in crates/core/src/lib.rs tests/validation/main.rs Cargo.toml Cargo.lock \
  .cargo/config.toml rust-toolchain rust-toolchain.toml rust-toolchain-custom \
  .github/workflows/ci.yml $'crates/a\nstrange name.rs'; do
  reset_case
  commit "$path" $'Documentation[#110]: Marked guarded change\n\n[SKIP BUILD]'
  expect false "guard overrides both rules: $path"
done

reset_case
commit docs/tests/guide.md 'Documentation[#110]: Nested tests directory is documentation'
commit $'docs/a\nstrange name.md' 'Documentation[#110]: Unusual filename'
expect true 'guard applies only to root paths'

reset_case
git commit -q --allow-empty -m 'Documentation[#110]: Empty documentation commit'
expect true 'qualifying empty commit'
git commit -q --allow-empty -m 'Maintenance[#110]: Empty unmarked commit'
expect false 'unmarked empty commit'
reset_case
expect false 'empty commit range'

# Put a guarded file in the base to test removals, renames and restoration.
commit crates/example.rs 'Maintenance[#110]: Base code'
base=$(git rev-parse HEAD)
git rm -q crates/example.rs
git commit -q -m 'Documentation[#110]: Delete code'
expect false 'deleted guarded file'
reset_case
mkdir -p docs
git mv crates/example.rs docs/example.md
git commit -q -m 'Documentation[#110]: Move code out of guarded directory'
expect false 'renamed guarded file checks old path'
reset_case
commit crates/example.rs 'Documentation[#110]: Change code'
git revert --no-edit HEAD > /dev/null
expect false 'revert commit without keyword still runs when net diff is empty'

# A stacked/diverged base must not count base-only changes as PR changes.
reset_case
commit docs/stack.md 'Documentation[#110]: Upper branch documentation'
head=$(git rev-parse HEAD)
git checkout -q --detach "$base"
commit crates/base-only.rs 'Maintenance[#110]: Base branch progresses'
base=$(git rev-parse HEAD)
git checkout -q --detach "$head"
expect true 'three-dot diff excludes changes only on diverged base'

if bash "$script" pull_request missing-base HEAD > "$temp/output" 2> "$temp/error"; then
  echo 'FAIL: unavailable history must fail the plan' >&2
  exit 1
fi
if bash "$script" pull_request > "$temp/output" 2> "$temp/error"; then
  echo 'FAIL: missing SHAs must fail the plan' >&2
  exit 1
fi
passed=$((passed + 2))
printf 'PASS: unavailable history and missing SHAs fail the plan\n'

# Non-PR decisions must not need repository history.
cd "$temp"
output=$(bash "$script" push)
[[ "$output" == "skip=false"$'\nreason='* ]]
passed=$((passed + 1))
printf 'PASS: push outside a repository\n%s checks passed\n' "$passed"
