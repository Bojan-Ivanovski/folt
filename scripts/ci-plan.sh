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
[ -n "$base" ] && [ -n "$head" ] || { echo 'PR base and head SHAs are required' >&2; exit 1; }

# Disable rename detection so moving a guarded file also checks its old path.
# Keep NUL delimiters: paths may contain whitespace, newlines or quotes.
paths=$(mktemp)
trap 'rm -f "$paths"' EXIT
git diff --no-renames --name-only -z "$base...$head" > "$paths"
while IFS= read -r -d '' path; do
  case "$path" in
    crates/*|tests/*|Cargo.toml|Cargo.lock|.cargo/*|rust-toolchain*|.github/workflows/*)
      decision false 'code path changed' ;;
  esac
done < "$paths"

commits=$(git rev-list "$base..$head")
count=0
for commit in $commits; do
  count=$((count + 1))
  subject=$(git log -1 --format=%s "$commit")
  last=$(git log -1 --format=%B "$commit" | tr -d '\r' | awk 'NF { last = $0 } END { print last }')
  if [[ "$subject" =~ ^Documentation\[#[0-9]+\]: ]] || [ "$last" = '[SKIP BUILD]' ]; then
    continue
  fi
  decision false "commit $(git rev-parse --short "$commit") has neither the Documentation keyword nor the marker"
done

[ "$count" -gt 0 ] || decision false 'no commits in range'
decision true "all $count commit(s) qualify and no code path changed"
