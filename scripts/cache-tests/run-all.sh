#!/usr/bin/env bash
#
# Run all cache tests sequentially.
# Stops on first failure unless --continue is passed.
#
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
CONTINUE="${1:-}"

passed=0
failed=0
failures=()

for test in "$DIR"/[0-9]*.sh; do
    name="$(basename "$test")"
    echo ""
    echo "================================================================"
    echo " Running: $name"
    echo "================================================================"
    echo ""

    if bash "$test"; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        failures+=("$name")
        if [[ "$CONTINUE" != "--continue" ]]; then
            echo ""
            echo "Stopping on first failure. Use --continue to run all."
            break
        fi
    fi
done

echo ""
echo "================================================================"
echo " Results: $passed passed, $failed failed"
if [[ ${#failures[@]} -gt 0 ]]; then
    echo " Failed: ${failures[*]}"
fi
echo "================================================================"

[[ $failed -eq 0 ]]
