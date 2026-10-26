#!/usr/bin/env bash
# Usage: scripts/ci-plan.sh <event> [<base-sha> <head-sha>]
set -euo pipefail

event=${1:?event is required}
base=${2:-} head=${3:-}

decision() {
  printf 'skip=%s\nreason=%s\n' "$1" "$2"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '### CI plan\n\nSkip: `%s`\n\nReason: %s\n' "$1" "$2" >> "$GITHUB_STEP_SUMMARY"
  fi
  exit 0
}

[ "$event" = pull_request ] || decision false "event $event always runs in full"
[ -n "$base" ] && [ -n "$head" ] || decision false 'PR base or head SHA is missing; full build required'

# Full checkout history can omit the event base after a force-push. Fetch that
# exact commit with its history, and never skip if it cannot be retrieved.
git fetch --no-tags origin "$base" >&2 || decision false 'event base SHA could not be fetched; full build required'

# Disable rename detection so moving a guarded file also checks its old path.
# Keep NUL delimiters: paths may contain whitespace, newlines or quotes.
paths=$(mktemp)
trap 'rm -f "$paths"' EXIT
git diff --no-renames --name-only -z "$base...$head" > "$paths" || decision false 'PR diff failed; full build required'
while IFS= read -r -d '' path; do
  case "$path" in
    crates/*|tests/*|Cargo.toml|Cargo.lock|.cargo/*|rust-toolchain*|.github/workflows/*)
      decision false 'code path changed' ;;
  esac
done < "$paths"

commits=$(git rev-list "$base..$head") || decision false 'PR commit listing failed; full build required'
count=0
for commit in $commits; do
  count=$((count + 1))
  subject=$(git log -1 --format=%s "$commit") || decision false 'PR commit log failed; full build required'
  last=$(git log -1 --format=%B "$commit" | tr -d '\r' | awk 'NF { last = $0 } END { print last }') || decision false 'PR commit log failed; full build required'
  if [[ "$subject" =~ ^Documentation\[#[0-9]+\]: ]] || [ "$last" = '[SKIP BUILD]' ]; then
    continue
  fi
  decision false "commit ${commit:0:12} has neither the Documentation keyword nor the marker"
done

[ "$count" -gt 0 ] || decision false 'no commits in range'
decision true "all $count commit(s) qualify and no code path changed"
