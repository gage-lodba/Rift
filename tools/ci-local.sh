#!/usr/bin/env bash
#
# Local CI gate for Rift — POSIX front-end (Linux/macOS).
#
# This is the same gate as `pwsh tools/ci-local.ps1` on Windows: the same steps
# in the same order, the same PASS/FAIL/SKIP/N/A/WARN rows (17 in a full run), and
# the same verdict contract. Both mirror `.github/workflows/ci.yml`; a change to
# one gate's step list belongs in the other one and in ci.yml. The POSIX front-end
# calls the repository's real shell guards directly instead of reimplementing
# them, and reports the Windows PE-icon check as N/A: the icon is a Windows PE
# resource, so it cannot be checked here, but it is not a skipped check — CI's
# Windows job proves it and an N/A row does not make the run incomplete. It
# refuses a Windows host: run `pwsh tools/ci-local.ps1` there, which owns the .exe
# paths, PE icon check and GUI-subsystem process-launch semantics.
#
# Exit codes (agents.md §8/§10.17): 0 only when every step ran and none failed
# (warnings are allowed), 1 on a hard failure, 2 when any step was skipped. A
# skipped step is not a pass, so a run with SKIP rows exits 2 unless
# --allow-incomplete is passed; that switch accepts a deliberately partial run
# (exit 0) but never turns a failure green.
#
# Usage: ./tools/ci-local.sh [options]
#   --skip-tests            skip the cargo test steps
#   --skip-release          skip the release build/clippy, PE icon and screenshot steps
#   --allow-incomplete      exit 0 despite SKIP rows (the verdict is still INCOMPLETE)
#   --allow-lock-update     do not pass --locked to cargo
#   --python-path PATH      Python for the screenshot comparison (default: $RIFT_PYTHON)
#   -h, --help              show this help
#
# The script writes artifacts/ci-local-<yyyyMMdd-HHmmss>.log (gitignored) and can
# be run from any directory.

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd)"
cd "$ROOT" || {
    echo "could not change directory to the repository root" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
SKIP_TESTS=0
SKIP_RELEASE=0
ALLOW_INCOMPLETE=0
ALLOW_LOCK_UPDATE=0
PYTHON_PATH="${RIFT_PYTHON:-}"

usage() {
    cat <<'EOF'
Usage: ./tools/ci-local.sh [options]

Runs the local acceptance gate (mirrors .github/workflows/ci.yml).

Options:
  --skip-tests            skip the cargo test steps
  --skip-release          skip the release build/clippy, PE icon and screenshot steps
  --allow-incomplete      exit 0 despite SKIP rows (the verdict is still INCOMPLETE)
  --allow-lock-update     do not pass --locked to cargo
  --python-path PATH      Python for the screenshot comparison (default: $RIFT_PYTHON)
  -h, --help              show this help

Exit codes: 0 all steps ran and passed (warnings allowed); 1 hard failure;
2 skipped step(s) without --allow-incomplete. A skipped step is not a pass.
Rows marked N/A are platform-inapplicable (the Windows PE icon) and do not
affect the verdict; CI's Windows job checks them.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --skip-tests) SKIP_TESTS=1 ;;
        --skip-release) SKIP_RELEASE=1 ;;
        --allow-incomplete) ALLOW_INCOMPLETE=1 ;;
        --allow-lock-update) ALLOW_LOCK_UPDATE=1 ;;
        --python-path)
            shift
            if [ "$#" -eq 0 ]; then
                echo "--python-path needs a value" >&2
                exit 64
            fi
            PYTHON_PATH="$1"
            ;;
        --python-path=*) PYTHON_PATH="${1#*=}" ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 64
            ;;
    esac
    shift
done

# This script is the POSIX front-end: the Windows one
# (`pwsh tools/ci-local.ps1`) owns .exe paths, the PE icon check and the
# GUI-subsystem process-launch semantics the screenshot step needs. Refuse a
# Windows host rather than half-supporting it.
case "$(uname -s 2>/dev/null)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        echo "tools/ci-local.sh is the POSIX front-end; run pwsh tools/ci-local.ps1 on Windows." >&2
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# Output, log and result bookkeeping
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
    C_RED=$'\033[31m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_CYAN=$'\033[36m'
    C_RESET=$'\033[0m'
else
    C_RED=""
    C_GREEN=""
    C_YELLOW=""
    C_CYAN=""
    C_RESET=""
fi

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$ROOT/artifacts"
LOG="$ROOT/artifacts/ci-local-$TIMESTAMP.log"
: >"$LOG"

RESULTS=()
FAILED=0
WARNED=0
SKIPPED=0
NOT_APPLICABLE=0

log() {
    printf '%s\n' "$*" | tee -a "$LOG"
}

logc() {
    local color="$1"
    shift
    printf '%s%s%s\n' "$color" "$*" "$C_RESET" | tee -a "$LOG"
}

# add_result STATUS SECONDS NAME [DETAIL]
add_result() {
    local status="$1" seconds="$2" name="$3" detail="${4:-}"
    RESULTS+=("$status"$'\t'"$seconds"$'\t'"$name"$'\t'"$detail")
    case "$status" in
        FAIL) FAILED=$((FAILED + 1)) ;;
        WARN) WARNED=$((WARNED + 1)) ;;
        SKIP) SKIPPED=$((SKIPPED + 1)) ;;
        "N/A") NOT_APPLICABLE=$((NOT_APPLICABLE + 1)) ;;
    esac
}

skip_step() {
    local name="$1" detail="$2"
    log ""
    logc "$C_YELLOW" "SKIP: $name ($detail)"
    add_result SKIP 0 "$name" "$detail"
}

# A platform-inapplicable check (currently only the Windows PE icon). Unlike a
# SKIP, it must not make the run incomplete: the check cannot exist on this host
# and CI's Windows job is its proof. Only the platform row uses this; it is not a
# user-selectable way to drop a check.
na_step() {
    local name="$1" detail="$2"
    log ""
    logc "$C_CYAN" "N/A: $name ($detail)"
    add_result "N/A" 0 "$name" "$detail"
}

now() {
    date +%s
}

# invoke_step NAME [--soft] FUNCTION
#
# Streams the function's output through the log, records PASS/FAIL (or WARN for
# a soft step) and always returns 0 so the remaining steps still run.
invoke_step() {
    local name="$1"
    shift
    local kind="hard"
    if [ "${1:-}" = "--soft" ]; then
        kind="soft"
        shift
    fi
    local start end rc status detail=""
    start="$(now)"
    log ""
    log "=== $name ==="
    "$@" 2>&1 | tee -a "$LOG"
    rc=${PIPESTATUS[0]}
    end="$(now)"
    if [ "$rc" -eq 0 ]; then
        status=PASS
        logc "$C_GREEN" "PASS: $name ($((end - start))s)"
    elif [ "$kind" = "soft" ]; then
        status=WARN
        detail="exit code $rc"
        logc "$C_YELLOW" "WARN: $name — $detail ($((end - start))s)"
    else
        status=FAIL
        detail="exit code $rc"
        logc "$C_RED" "FAIL: $name — $detail ($((end - start))s)"
    fi
    add_result "$status" "$((end - start))" "$name" "$detail"
}

# ---------------------------------------------------------------------------
# Command configuration
# ---------------------------------------------------------------------------
PACKAGES=(-p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop)

# A plain string (not an array) because it is either `--locked` or empty: the
# unquoted expansion in the cargo calls is deliberate, and avoids the empty-array
# expansion differences between bash 3.2 (macOS) and newer bash.
LOCKED="--locked"
if [ "$ALLOW_LOCK_UPDATE" -eq 1 ]; then
    LOCKED=""
fi

# Assembled from fragments so the retired framework's brand does not appear in
# the project text; the guard itself stays. Keep in step with ci-local.ps1.
LEGACY_PREFIX="tau""ri|y""ew|wasm-bindgen|wasm_bindgen"

SCREENSHOT="$ROOT/artifacts/screens/ci-home.png"

release_binary() {
    printf '%s\n' "target/release/rift-music"
}

# A Python with Pillow + numpy is needed for the image comparison only. The
# comparison is skipped (not failed) when none is available.
PYTHON=""
find_python() {
    local candidate
    for candidate in "$PYTHON_PATH" python3 python; do
        [ -n "$candidate" ] || continue
        command -v "$candidate" >/dev/null 2>&1 || continue
        if "$candidate" -c 'import numpy, PIL' >/dev/null 2>&1; then
            PYTHON="$candidate"
            return 0
        fi
    done
    PYTHON=""
    return 1
}
find_python

# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------
step_toolchain() {
    local channel version active
    channel="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' rust-toolchain.toml | head -n 1)"
    if [ -z "$channel" ]; then
        echo "could not read 'channel' from rust-toolchain.toml"
        return 1
    fi
    version="$(rustc --version 2>&1)" || {
        echo "rustc --version failed: $version"
        return 1
    }
    active="${version#rustc }"
    active="${active%% *}"
    if ! printf '%s\n' "$active" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "could not parse the active compiler version from '$version'"
        return 1
    fi
    echo "active rustc: $active; rust-toolchain.toml channel: $channel"
    if [ "$active" != "$channel" ]; then
        echo "active rustc $active does not match the rust-toolchain.toml channel $channel."
        echo "The gate must build with the pinned compiler; run 'rustup override unset' (or remove RUSTUP_TOOLCHAIN) so rustup selects $channel, then re-run."
        return 1
    fi
    bash .github/scripts/check-toolchain-consistency.sh
}

step_fmt() {
    cargo fmt --all -- --check
}

step_clippy() {
    cargo clippy "${PACKAGES[@]}" $LOCKED --all-targets -- -D warnings
}

step_clippy_ui() {
    cargo clippy -p rift-desktop --no-default-features $LOCKED --all-targets -- -D warnings
}

step_doc() {
    RUSTDOCFLAGS="-D warnings" cargo doc -p rift -p rift-types -p rift-ytmusic -p rift-discord --no-deps $LOCKED
}

step_tests() {
    cargo test "${PACKAGES[@]}" $LOCKED
}

step_tests_ui() {
    cargo test -p rift-desktop --no-default-features $LOCKED
}

step_release_build() {
    cargo build --release -p rift-desktop --features backend $LOCKED || return $?
    local bin size
    bin="$(release_binary)"
    if [ ! -f "$bin" ]; then
        echo "release binary missing: $bin"
        return 1
    fi
    size="$(wc -c <"$bin" | tr -d '[:space:]')"
    echo "binary: $bin ($size bytes)"
}

step_clippy_release() {
    cargo clippy --release -p rift-desktop $LOCKED --all-targets -- -D warnings || return $?
    cargo clippy --release -p rift-desktop --features backend,preview $LOCKED --all-targets -- -D warnings
}

step_preview_stripped() {
    local bin out status
    bin="$(release_binary)"
    if [ ! -f "$bin" ]; then
        echo "release binary missing: $bin"
        return 1
    fi
    out="$("$bin" --preview 2>&1)"
    status=$?
    printf '%s\n' "$out"
    if [ "$status" -eq 0 ]; then
        echo "release binary accepted --preview (preview mode is shipping)"
        return 1
    fi
    case "$out" in
        *development-only*) ;;
        *)
            echo "release binary rejected --preview without the expected development-only message"
            return 1
            ;;
    esac
}

step_screenshot() {
    local bin
    bin="$(release_binary)"
    mkdir -p "$(dirname "$SCREENSHOT")"
    rm -f "$SCREENSHOT"
    # The single-instance guard makes an offscreen capture exit 0 without
    # writing the PNG, so turn a running app into an actionable failure.
    if command -v pgrep >/dev/null 2>&1 && pgrep -x rift-music >/dev/null 2>&1; then
        echo "another rift-music instance is running; the single-instance guard makes an offscreen capture exit without writing the PNG. Close it and re-run."
        return 1
    fi
    cargo build --release -p rift-desktop --features backend,preview $LOCKED || return $?
    "$bin" --preview --screenshot "$SCREENSHOT" --size 1160x740 --view home || return $?
    if [ ! -f "$SCREENSHOT" ]; then
        echo "screenshot missing: $SCREENSHOT"
        return 1
    fi
    echo "screenshot: $SCREENSHOT ($(wc -c <"$SCREENSHOT" | tr -d '[:space:]') bytes)"
    # Leave target/release holding the shipped configuration again, so a later
    # hand-run of the binary does not silently accept preview flags.
    cargo build --release -p rift-desktop --features backend $LOCKED || {
        echo "could not restore the shipped release build after the acceptance capture"
        return 1
    }
    echo "restored shipped release build (preview feature disabled)"
}

step_compare() {
    if [ ! -f "tools/reference-old-ui.png" ]; then
        echo "reference image missing: tools/reference-old-ui.png"
        return 1
    fi
    "$PYTHON" tools/compare_images.py tools/reference-old-ui.png "$SCREENSHOT" --label ci-home
}

step_legacy_tree() {
    local tree hits
    tree="$(cargo tree -p rift-desktop $LOCKED)" || return $?
    hits="$(printf '%s\n' "$tree" | grep -E "$LEGACY_PREFIX" || true)"
    if [ -n "$hits" ]; then
        printf '%s\n' "$hits"
        return 1
    fi
    echo "cargo tree -p rift-desktop: no legacy webview crates"
}

step_manifests() {
    local manifest line line_no code key count=0 bad=0 manifests
    if ! command -v git >/dev/null 2>&1; then
        echo "git is required for the manifest scan"
        return 1
    fi
    if ! manifests="$(git ls-files --cached --others --exclude-standard -- '*Cargo.toml')"; then
        echo "git ls-files failed; cannot scan workspace manifests"
        return 1
    fi
    while IFS= read -r manifest; do
        [ -n "$manifest" ] || continue
        case "$manifest" in
            */ui/*) continue ;;
        esac
        [ -f "$manifest" ] || continue
        count=$((count + 1))
        line_no=0
        while IFS= read -r line || [ -n "$line" ]; do
            line_no=$((line_no + 1))
            code="${line%%#*}"
            key="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*"\{0,1\}\([A-Za-z0-9_.-]*\)"\{0,1\}[[:space:]]*=.*/\1/p')"
            [ -n "$key" ] || continue
            key="$(printf '%s' "$key" | tr '[:upper:]' '[:lower:]')"
            if [[ "$key" =~ ^($LEGACY_PREFIX) ]]; then
                echo "$manifest:$line_no: $line"
                bad=1
            fi
        done <"$manifest"
    done <<<"$manifests"
    if [ "$bad" -ne 0 ]; then
        echo "forbidden dependency declarations found (see lines above)"
        return 1
    fi
    echo "scanned $count workspace Cargo.toml file(s): clean"
}

step_duplicate() {
    bash .github/scripts/check-duplicate-deps.sh
}

step_deny() {
    cargo deny check all
}

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
logc "$C_CYAN" "Rift local CI gate (bash) — $TIMESTAMP"
log "root: $ROOT"
log "log : $LOG"
log "rustc: $(rustc --version 2>&1 | head -n 1)"
log "cargo: $(cargo --version 2>&1 | head -n 1)"
log "locked: $( [ "$ALLOW_LOCK_UPDATE" -eq 0 ] && echo true || echo false ); skip-tests: $SKIP_TESTS; skip-release: $SKIP_RELEASE; allow-incomplete: $ALLOW_INCOMPLETE"
log "python (image compare): $(if [ -n "$PYTHON" ]; then printf '%s' "$PYTHON"; else printf 'NOT FOUND (comparison will be skipped)'; fi)"
log "git HEAD: $(git rev-parse --short HEAD 2>/dev/null || echo unknown); changed files: $(git status --porcelain 2>/dev/null | wc -l | tr -d '[:space:]') (informational — the gate checks the working tree, not a commit)"

# ---------------------------------------------------------------------------
# Run the gate (same order and names as tools/ci-local.ps1)
# ---------------------------------------------------------------------------
invoke_step "toolchain consistency" step_toolchain
invoke_step "cargo fmt --all -- --check" step_fmt
invoke_step "cargo clippy (-D warnings)" step_clippy
invoke_step "cargo clippy -p rift-desktop --no-default-features (-D warnings)" step_clippy_ui
invoke_step "cargo doc -p rift -p rift-types -p rift-ytmusic -p rift-discord" step_doc

if [ "$SKIP_TESTS" -eq 1 ]; then
    skip_step "cargo test (skipped)" "--skip-tests"
else
    invoke_step "cargo test" step_tests
    invoke_step "cargo test -p rift-desktop --no-default-features" step_tests_ui
fi

if [ "$SKIP_RELEASE" -eq 1 ]; then
    skip_step "release build (skipped)" "--skip-release"
    skip_step "cargo clippy (release configurations, skipped)" "--skip-release"
    skip_step "windows executable file icon (skipped)" "--skip-release"
else
    invoke_step "release build (-p rift-desktop --features backend)" step_release_build
    invoke_step "cargo clippy (release configurations, -D warnings)" step_clippy_release
    na_step "windows executable file icon" "Windows PE resource; CI's Windows job checks it"
    invoke_step "preview mode is stripped from the release build" step_preview_stripped
    invoke_step "offscreen release screenshot" step_screenshot
    if [ -z "$PYTHON" ]; then
        skip_step "screenshot vs reference (skipped)" "no Pillow+numpy Python"
    else
        invoke_step "screenshot vs reference (metrics only)" --soft step_compare
    fi
fi

invoke_step "no legacy webview crates in the app dependency tree" step_legacy_tree
invoke_step "no legacy webview/WASM manifests in the workspace" step_manifests
invoke_step "duplicate dependency baseline" step_duplicate

if command -v cargo-deny >/dev/null 2>&1; then
    invoke_step "cargo deny check all" step_deny
else
    skip_step "cargo deny check (skipped)" "cargo-deny not installed"
fi

# ---------------------------------------------------------------------------
# Summary and verdict
# ---------------------------------------------------------------------------
print_summary() {
    local entry status seconds name detail color
    log ""
    logc "$C_CYAN" "================ SUMMARY ================"
    if [ "${#RESULTS[@]}" -gt 0 ]; then
        for entry in "${RESULTS[@]}"; do
            IFS=$'\t' read -r status seconds name detail <<<"$entry"
            case "$status" in
                PASS) color="$C_GREEN" ;;
                FAIL) color="$C_RED" ;;
                SKIP | WARN) color="$C_YELLOW" ;;
                "N/A") color="$C_CYAN" ;;
                *) color="" ;;
            esac
            printf '%s%-6s %-70s %5ss  %s%s\n' "$color" "$status" "$name" "$seconds" "$detail" "$C_RESET" | tee -a "$LOG"
        done
    fi
    logc "$C_CYAN" "========================================="
    if [ "$FAILED" -gt 0 ]; then
        logc "$C_RED" "GATE: FAILED ($FAILED hard failure(s), $WARNED warning(s))"
    elif [ "$SKIPPED" -gt 0 ]; then
        logc "$C_YELLOW" "GATE: INCOMPLETE ($SKIPPED skipped step(s))"
        if [ "$ALLOW_INCOMPLETE" -eq 0 ]; then
            log "hint: pass --allow-incomplete to accept the skipped step(s); the run is still not a full pass"
        fi
    elif [ "$WARNED" -gt 0 ]; then
        logc "$C_YELLOW" "GATE: PASSED with $WARNED warning(s)"
    else
        logc "$C_GREEN" "GATE: PASSED"
    fi
    if [ "$NOT_APPLICABLE" -gt 0 ]; then
        log "note: $NOT_APPLICABLE check(s) not applicable on this host; CI's Windows job covers the PE icon"
    fi
    log "log: $LOG"
}

print_summary

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
if [ "$SKIPPED" -gt 0 ] && [ "$ALLOW_INCOMPLETE" -eq 0 ]; then
    exit 2
fi
exit 0
