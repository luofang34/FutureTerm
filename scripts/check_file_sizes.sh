#!/usr/bin/env bash
# Fail if any .rs file under apps/ or crates/ exceeds the 500-line limit
# declared in CLAUDE.md. Called from ./dev.sh check and CI.

set -eu

MAX_LINES=500
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OFFENDERS=$(find apps crates -type f -name '*.rs' -not -path '*/target/*' \
    -exec wc -l {} + 2>/dev/null \
    | awk -v max="$MAX_LINES" '$2 != "total" && $1 > max' \
    | sort -rn)

if [ -n "$OFFENDERS" ]; then
    echo "The following .rs files exceed the ${MAX_LINES}-line limit declared in CLAUDE.md."
    echo "Split them into sub-modules (foo.rs + foo/, no mod.rs)."
    echo ""
    echo "$OFFENDERS"
    exit 1
fi

exit 0
