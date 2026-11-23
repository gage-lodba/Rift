#!/usr/bin/env bash
#
# Keep the two local-gate front-ends in sync and syntax-clean.
#
# `tools/ci-local.sh` and `tools/ci-local.ps1` are two implementations of the
# same 17-row gate. A row added to (or removed from) one front-end but not the
# other is a silent acceptance hole, so this guard:
#
#   * syntax-checks the bash front-end (`bash -n`) and, when `pwsh` is available,
#     the PowerShell front-end;
#   * extracts every row name from both front-ends, normalizes the skip/command
#     suffixes, and fails when the sets differ or the count is not the expected
#     17;
#   * checks that the four clippy configurations the gate exists for are still
#     present (with `-D warnings`) in both front-ends and in ci.yml.
#
# It deliberately does not compare CI step-for-step with the local rows: ci.yml
# has finer steps (the shipped and preview release clippy configurations are
# separate CI steps so a Windows failure cannot be masked), while each local
# gate reports one release-clippy row. The clippy-configuration checks above are
# the part that must not drift.
#
# Usage: .github/scripts/check-local-gates.sh [bash-gate] [ps-gate] [ci-workflow]
# The optional paths are for testing the guard itself; defaults are the real
# files relative to the repository root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." >/dev/null 2>&1 && pwd)"
cd "$ROOT"

bash_gate="${1:-tools/ci-local.sh}"
ps_gate="${2:-tools/ci-local.ps1}"
ci_workflow="${3:-.github/workflows/ci.yml}"

fail=0

error() {
    echo "::error::$*"
    fail=1
}

for file in "$bash_gate" "$ps_gate" "$ci_workflow"; do
    if [ ! -f "$file" ]; then
        error "missing $file"
    fi
done
if [ "$fail" -ne 0 ]; then
    exit 1
fi

bash -n "$bash_gate" || error "$bash_gate failed 'bash -n'"

# Syntax-check the Windows front-end too. GitHub's ubuntu runners include pwsh;
# developers without it still get the row-set comparison, so a missing pwsh is a
# note rather than a failure.
if command -v pwsh >/dev/null 2>&1; then
    if ! ps_parse="$(pwsh -NoProfile -Command "\$t = \$null; \$e = \$null; [System.Management.Automation.Language.Parser]::ParseFile('$ps_gate', [ref]\$t, [ref]\$e) | Out-Null; if (\$e.Count -gt 0) { \$e | ForEach-Object { Write-Error \$_.Message }; exit 1 }" 2>&1)"; then
        printf '%s\n' "$ps_parse"
        error "$ps_gate failed the PowerShell parser"
    fi
else
    if [ "${CI:-}" = "true" ]; then
        error "pwsh is required in CI to parse $ps_gate"
    else
        echo "note: pwsh not found; skipping the PowerShell syntax check"
    fi
fi

# Extract the row name from each call site:
#   invoke_step "NAME" ...        (bash)
#   skip_step "NAME" ...          (bash)
#   na_step "NAME" ...            (bash)
#   Invoke-Step "NAME" {          (PowerShell)
#   Add-Result -Step "NAME" ...   (PowerShell)
extract_rows() {
    sed -nE 's/^[[:space:]]*(invoke_step|skip_step|na_step|Invoke-Step|Add-Result -Step)[[:space:]]+"([^"]+)".*/\2/p' "$1"
}

# Normalize names that differ between a full row and its skip variant, and
# between the two front-ends' wording, to the platform-independent row identity.
normalize_rows() {
    sed -E \
        -e 's/ \(skipped\)$//' \
        -e 's/, skipped\)$/)/' \
        -e 's/, -D warnings\)$/)/' \
        -e 's/ \(metrics only\)$//' \
        -e 's/ \(-p rift-desktop --features backend\)$//' \
        -e 's/ \(PowerShell fallback\)$//' \
        -e 's/ all$//'
}

bash_rows="$(extract_rows "$bash_gate" | normalize_rows | sort -u)"
ps_rows="$(extract_rows "$ps_gate" | normalize_rows | sort -u)"

if ! diff -u <(printf '%s\n' "$ps_rows") <(printf '%s\n' "$bash_rows"); then
    error "local gate row sets differ (left: $ps_gate, right: $bash_gate)"
fi

row_count="$(printf '%s\n' "$bash_rows" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
if [ "$row_count" -ne 17 ]; then
    error "expected 17 local-gate row names, found $row_count"
fi

# Each front-end and CI must still run the four configurations. Requiring
# `-D warnings` on the same line is what makes a clippy invocation a gate rather
# than a report.
require_clippy() {
    local file="$1" pattern="$2" label="$3"
    if ! grep -Eq -- "$pattern" "$file"; then
        error "$file is missing the $label clippy configuration ($pattern)"
    fi
}

for file in "$bash_gate" "$ps_gate" "$ci_workflow"; do
    require_clippy "$file" 'cargo clippy .*--all-targets.*-D warnings' 'debug/default'
    require_clippy "$file" 'cargo clippy .*--no-default-features.*-D warnings' 'UI-only'
    require_clippy "$file" 'cargo clippy --release .*--all-targets.*-D warnings' 'release shipped'
    require_clippy "$file" 'cargo clippy --release .*--features backend,preview.*-D warnings' 'release preview'
done

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "local gates: $bash_gate parses; both front-ends have the same $row_count rows; all four clippy configurations are present in both front-ends and $ci_workflow"
