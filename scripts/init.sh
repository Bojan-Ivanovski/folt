#!/usr/bin/env bash
# Replace template placeholders after copying this repository.
# Usage: scripts/init.sh "Foltivo CLI" Bojan-Ivanovski/foltivo-cli
set -euo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <project name> <owner/repo>" >&2; exit 1; }
name=$1 repo=$2
cd "$(dirname "$0")/.."
files=$(grep -rl --exclude-dir=.git --exclude=init.sh -e '{{PROJECT_NAME}}' -e '{{REPO}}' . || true)
for f in $files; do
  sed -i "s|{{PROJECT_NAME}}|$name|g; s|{{REPO}}|$repo|g" "$f"
done
echo "Initialised $name ($repo). Next: README title, docs/dev/stack.md."
echo "Then create the repository, labels, Project Foltivo items and epic #1 (see README)."
