#!/usr/bin/env bash
# Fail when the dependency graph grows a *new* duplicate version of a crate.
#
# `cargo tree -d` is not a usable guard on its own. It prints every package that
# appears more than once, which includes the same version built with different
# feature sets (`serde v1.0.229` twice is one version, two nodes), and it exits 0
# whether or not it found anything.
#
# This narrows it to genuinely distinct versions and compares against the
# baseline below. Recorded, not allow-listed crate by crate, because the honest
# answer for most of these is "a transitive dependency pins an old major and this
# repo has no lever".
#
# The regression this exists for: Rift used to ship `reqwest` 0.12 *and* 0.13 in
# one binary — two TLS stacks. That is gone (the direct `reqwest` is 0.13,
# matching what Tauri pulls in), and the lists below are the state *after* that
# fix, so any of them coming back fails the build.
#
# ## The baseline is per host
#
# `cargo tree` filters by the host target unless told otherwise, so the
# duplicated set genuinely differs per platform: the `windows-*` generation is
# not in the graph on Linux or macOS, macOS's Cocoa/Swift crates are not in the
# Linux graph, and Linux's second `zbus` is not in either other graph. One
# combined baseline would flag every platform but the one it was recorded on, for
# reasons that are not regressions — which is exactly what happened the first
# time this ran on CI. `baseline_shared` holds the crates duplicated on all
# three; each host list adds its own.
#
# Known duplicates with no lever here, recorded rather than left to surprise
# someone:
#
#   - Linux: `zbus` 3 **and** 5. `souvlaki` 0.8.3 (the newest release on
#     crates.io) pins zbus 3 for the `use_zbus` backend the workspace selects to
#     keep Linux pure-Rust (no libdbus); Tauri's single-instance plugin brings
#     zbus 5. The whole zbus 3 tree is in `baseline_linux`, including `rand` 0.8
#     beside the direct `rand` 0.10.
#   - Linux: `heck` 0.4 (glib-macros 0.18, from the GTK bindings) beside heck
#     0.5.
#   - macOS: `base64` 0.21 (swift-rs via tauri-utils) beside 0.22, and the
#     core-foundation / core-graphics families split across objc2 generations.
#
# To see the full picture: cargo tree -d --locked
#
# Run this where `cargo` is on PATH and the workspace resolves. Invoking it from
# PowerShell on Windows resolves `bash` to WSL's, which has no cargo, and a
# `CARGO_HOME` without the registry cannot resolve the lockfile either; both used
# to exit silently (see the capture below).
set -euo pipefail

# Duplicated on every host.
baseline_shared=$(cat <<'EOF'
bitflags
getrandom
hashbrown
http
indexmap
miniz_oxide
syn
thiserror
thiserror-impl
toml
toml_datetime
uuid
winnow
EOF
)

# Windows-only: Microsoft's windows-* generation, plus cpufeatures (also
# duplicated on Linux, not on macOS).
baseline_windows=$(cat <<'EOF'
cpufeatures
windows
windows-collections
windows-core
windows-future
windows-link
windows-numerics
windows-result
windows-strings
windows-sys
windows-targets
windows-threading
windows_x86_64_msvc
EOF
)

# Linux-only: the zbus 3 tree souvlaki pins (see the header), heck, plus
# cpufeatures.
baseline_linux=$(cat <<'EOF'
async-broadcast
async-io
async-lock
cpufeatures
event-listener
fastrand
futures-lite
heck
linux-raw-sys
memoffset
polling
proc-macro-crate
rand
rand_core
rustix
serde_spanned
socket2
toml_edit
zbus
zbus_macros
zbus_names
zvariant
zvariant_derive
zvariant_utils
EOF
)

# macOS-only: the Cocoa/Swift families.
baseline_macos=$(cat <<'EOF'
base64
core-foundation
core-graphics
core-graphics-types
foreign-types
foreign-types-shared
png
EOF
)

host=$(uname -s)
case "$host" in
  Linux*) host_baseline="$baseline_linux" ;;
  Darwin*) host_baseline="$baseline_macos" ;;
  MINGW* | MSYS* | CYGWIN*) host_baseline="$baseline_windows" ;;
  *)
    echo "::error::unrecognised host '$host'. This guard's baseline is recorded per host; add this"
    echo "::error::platform's list to the script (or run the guard where a baseline exists)."
    exit 1
    ;;
esac
baseline=$(printf '%s\n%s\n' "$baseline_shared" "$host_baseline" | sed '/^[[:space:]]*$/d' | sort)

# "<crate> <version>" per distinct version, skipping the repeated
# feature-split nodes.
#
# The capture is not cosmetic. `cargo tree` fails wherever cargo cannot resolve
# the workspace — WSL's bash has no cargo at all, and a `CARGO_HOME` without the
# registry cannot read the lockfile — and with `set -euo pipefail` plus stderr
# sent to /dev/null that failure was *silent*: the pipeline died and the script
# exited 1 having printed nothing, which reads exactly like a guard that never
# ran. Failing without saying why is the shape this whole script exists to catch,
# so the invocation is captured and checked first.
status=0
tree_output=$(cargo tree -d --locked) || status=$?
if [ "$status" -ne 0 ]; then
  echo "::error::'cargo tree -d --locked' failed (exit $status). This guard needs cargo on PATH and a"
  echo "::error::resolvable workspace; run it from the repository root with that toolchain's shell."
  echo "::error::On Windows that means Git Bash, not WSL's bash — WSL has no cargo. Nothing about the"
  echo "::error::dependency graph was checked."
  exit 1
fi

current=$(echo "$tree_output" |
  grep -E '^[a-zA-Z0-9_-]+ v[0-9]' |
  sed -E 's/^([a-zA-Z0-9_-]+) (v[0-9][^ ]*).*/\1 \2/' |
  sort -u |
  awk '{ count[$1]++ } END { for (c in count) if (count[c] > 1) print c }' |
  sort)

if [ -z "$current" ]; then
  echo "No duplicate versions found."
  exit 0
fi

# Crates duplicated now but not in the baseline.
new_duplicates=$(comm -13 <(echo "$baseline" | sort) <(echo "$current"))

# Crates in the baseline that are no longer duplicated — good news, but the
# baseline should shrink so a future regression is caught again.
fixed=$(comm -23 <(echo "$baseline" | sort) <(echo "$current"))

if [ -n "$fixed" ]; then
  echo "These crates are no longer duplicated on $host; remove them from its baseline:"
  echo "$fixed" | sed 's/^/  /'
  echo
fi

if [ -n "$new_duplicates" ]; then
  echo "::error::the dependency graph gained duplicate crate versions on $host"
  echo
  echo "$new_duplicates" | sed 's/^/  /'
  echo
  echo "Two majors of one crate means duplicated types, an extra TLS stack, or a"
  echo "second rand generation in the shipped binary. Unify the versions; if a"
  echo "transitive dependency makes that impossible, add the crate to this host's"
  echo "baseline in this script with a note saying which dependency pins the"
  echo "older major."
  echo
  cargo tree -d --locked
  exit 1
fi

echo "No new duplicate versions on $host: ${current//$'\n'/ }"
