#!/usr/bin/env bash
# Check/apply only the settings owned by .github/settings and labels.json.
set -euo pipefail

usage() {
  echo "Usage: $0 check|apply [owner/repo] [--yes] [--remove-main-protection]" >&2
  exit 2
}
fail() { echo "repo-settings: $*" >&2; exit 2; }
[ $# -ge 1 ] || usage
mode=$1
shift
case "$mode" in check|apply) ;; *) usage ;; esac
repo=Bojan-Ivanovski/folt
repo_given=false
yes=false
remove_main=false
for arg in "$@"; do
  case "$arg" in
    --yes) yes=true ;;
    --remove-main-protection) remove_main=true ;;
    -*) usage ;;
    *) [ "$repo_given" = false ] || usage; repo=$arg; repo_given=true ;;
  esac
done
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail 'Expected owner/repo.'
if [ "$mode" = check ] && { [ "$yes" = true ] || [ "$remove_main" = true ]; }; then usage; fi
for tool in gh jq diff; do command -v "$tool" >/dev/null || fail "Missing dependency: $tool"; done
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
settings=$root/.github/settings
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
repository=$(jq -ce . "$settings/repository.json")
security=$(jq -ce . "$settings/security.json")
ruleset=$(jq -ce . "$settings/rulesets/protect-integration-branches.json")
labels=$(jq -ce . "$root/.github/labels.json")
base="repos/$repo"
count=0
classic=false

# gh exits nonzero for HTTP errors. Accept 404 only at explicitly optional
# endpoints; all other failures abort before the first write. Do not mistake
# authorization/network failures for disabled settings.
get() {
  local endpoint=$1 optional=${2:-false}
  if gh api --hostname github.com -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' --include "$endpoint" \
    >"$work/response" 2>"$work/error"; then
    http_status=ok
  else
    http_status=$(awk 'NR == 1 {print $2}' "$work/response")
    if [ "$http_status" != 404 ] || [ "$optional" != true ]; then
      cat "$work/error" >&2
      fail "Cannot read $endpoint (HTTP ${http_status:-unknown}); no further changes made."
    fi
  fi
  if [ "$http_status" = 404 ]; then
    live=null
  else
    live=$(sed '1,/^[[:space:]]*$/d' "$work/response")
    live=${live:-null}
    jq -e . >/dev/null <<<"$live" || [ "$live" = null ] || fail "Invalid JSON from $endpoint"
  fi
}

# Ignore response-only fields, but compare every declared key. Array order is
# immaterial for these settings; missing keys remain null (including booleans).
project='def project($want):
  if ($want | type) == "object" then
    . as $live | reduce ($want | keys[]) as $k
      ({}; .[$k] = ($live[$k] | project($want[$k])))
  else . end;
  def canonical: walk(if type == "array" then sort_by(tojson) else . end);'
plan() {
  local name=$1 endpoint=$2 method=$3 desired=$4 actual=$5
  jq -S "$project project(\$want) | canonical" --argjson want "$desired" \
    <<<"$actual" >"$work/actual"
  jq -S "$project canonical" <<<"$desired" >"$work/desired"
  if ! cmp -s "$work/actual" "$work/desired"; then
    echo "Drift: $name"
    diff -u --label "live: $name" --label "file: $name" "$work/actual" "$work/desired" || :
    count=$((count + 1))
    printf '%s\n' "$desired" >"$work/$count.json"
    printf '%s\n' "$method" "$endpoint" >"$work/$count.request"
  fi
}
write() {
  local method=$1 endpoint=$2 payload=${3:-}
  echo "$method $endpoint"
  if [ -n "$payload" ]; then
    gh api --hostname github.com -H 'Accept: application/vnd.github+json' \
      -H 'X-GitHub-Api-Version: 2022-11-28' --method "$method" "$endpoint" --input "$payload" >/dev/null
  else
    gh api --hostname github.com -H 'Accept: application/vnd.github+json' \
      -H 'X-GitHub-Api-Version: 2022-11-28' --method "$method" "$endpoint" >/dev/null
  fi
}

get "$base"
plan repository "$base" PATCH "$(jq -c .settings <<<"$repository")" "$live"
plan secret-scanning "$base" PATCH "$(jq -c '{security_and_analysis}' <<<"$security")" "$live"
get "$base/topics"
plan topics "$base/topics" PUT "$(jq -c '{names: .topics}' <<<"$repository")" "$live"

# Include inherited rules when checking; never modify an organization ruleset.
rules_name=$(jq -r .name <<<"$ruleset")
rules=$(gh api --hostname github.com --paginate --slurp "$base/rulesets?per_page=100")
ids=$(jq -c --arg name "$rules_name" '[.[][] | select(.name == $name)]' <<<"$rules")
[ "$(jq length <<<"$ids")" -le 1 ] || fail "Multiple rulesets named $rules_name; resolve manually."
id=$(jq -r '.[0].id // empty' <<<"$ids")
if [ -n "$id" ]; then
  [ "$(jq -r '.[0].source_type' <<<"$ids")" = Repository ] || fail 'Target ruleset is inherited; cannot manage it.'
  get "$base/rulesets/$id"
  plan ruleset "$base/rulesets/$id" PUT "$ruleset" "$live"
else
  plan ruleset "$base/rulesets" POST "$ruleset" '{}'
fi

get "$base/private-vulnerability-reporting"
reporting_method=PUT
[ "$(jq -r .private_vulnerability_reporting.enabled <<<"$security")" = true ] || reporting_method=DELETE
plan private-reporting "$base/private-vulnerability-reporting" "$reporting_method" \
  "$(jq -c .private_vulnerability_reporting <<<"$security")" "$live"
get "$base/vulnerability-alerts" true
alerts=false
[ "$live" = null ] && [ "$http_status" = 404 ] || alerts=true
# This endpoint returns 204 when enabled, 404 when disabled; PUT has no body.
alerts_method=PUT
[ "$(jq -r .dependabot_alerts <<<"$security")" = true ] || alerts_method=DELETE
plan dependabot-alerts "$base/vulnerability-alerts" "$alerts_method" \
  "$(jq -c '{enabled: .dependabot_alerts}' <<<"$security")" "{\"enabled\":$alerts}"
get "$base/automated-security-fixes"
fixes_method=PUT
[ "$(jq -r .automated_security_fixes.enabled <<<"$security")" = true ] || fixes_method=DELETE
plan security-updates "$base/automated-security-fixes" "$fixes_method" \
  "$(jq -c .automated_security_fixes <<<"$security")" "$live"
get "$base/code-scanning/default-setup" true
[ "$live" != null ] || live='{}'
plan codeql "$base/code-scanning/default-setup" PATCH "$(jq -c .codeql <<<"$security")" "$live"
get "$base/actions/permissions"
plan actions "$base/actions/permissions" PUT "$(jq -c .actions <<<"$security")" "$live"
get "$base/actions/permissions/workflow"
plan workflow-token "$base/actions/permissions/workflow" PUT "$(jq -c .workflow <<<"$security")" "$live"

all_labels=$(gh api --hostname github.com --paginate --slurp "$base/labels?per_page=100")
while IFS= read -r label; do
  name=$(jq -r .name <<<"$label")
  encoded=$(jq -rn --arg name "$name" '$name | @uri')
  actual=$(jq -c --arg name "$name" '[.[][] | select(.name == $name)][0] // {}' <<<"$all_labels")
  if [ "$actual" = '{}' ]; then
    plan "label $name" "$base/labels" POST "$label" "$actual"
  else
    plan "label $name" "$base/labels/$encoded" PATCH "$label" "$actual"
  fi
done < <(jq -c '.[]' <<<"$labels")
get "$base/branches/main/protection" true
if [ "$http_status" != 404 ]; then
  classic=true
  echo 'Drift: classic main protection exists; use the separate removal step after verifying the ruleset.'
fi

if [ "$mode" = check ]; then
  if [ "$count" -gt 0 ] || [ "$classic" = true ]; then exit 1; fi
  echo "No drift: $repo"
  exit 0
fi
[ "$yes" = true ] || fail 'Diff shown; rerun apply with --yes after merge and review to write settings.'
for ((i=1; i<=count; i++)); do
  { IFS= read -r method; IFS= read -r endpoint; } <"$work/$i.request"
  case "$endpoint" in
    */private-vulnerability-reporting|*/vulnerability-alerts|*/automated-security-fixes)
      write "$method" "$endpoint" ;;
    *) write "$method" "$endpoint" "$work/$i.json" ;;
  esac
done
if [ "$remove_main" = true ] && [ "$classic" = true ]; then
  # Re-read the exact repository ruleset, including active enforcement, every
  # required rule and bypass actors. Never remove protection after a failed read.
  if [ -z "$id" ]; then
    rules=$(gh api --hostname github.com --paginate --slurp "$base/rulesets?per_page=100")
    id=$(jq -er --arg name "$rules_name" '[.[][] | select(.name == $name and .source_type == "Repository")] | if length == 1 then .[0].id else error("Ruleset not unique") end' <<<"$rules")
  fi
  get "$base/rulesets/$id"
  actual=$(jq -S "$project project(\$want) | canonical" --argjson want "$ruleset" <<<"$live")
  desired=$(jq -S "$project canonical" <<<"$ruleset")
  [ "$actual" = "$desired" ] || fail 'Ruleset not verified; keeping classic main protection.'
  write DELETE "$base/branches/main/protection"
fi
echo 'Apply finished. Run check again; CodeQL setup may finish asynchronously.'
if [ "$classic" = true ] && [ "$remove_main" = false ]; then
  echo 'Classic main protection retained. Review the active ruleset, then run the explicit removal step.'
fi
