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
# repo has no lever" — 50-odd crates, mostly Microsoft's `windows-*` generation
# and the `syn`/`toml`/`winnow` families that every proc-macro tree carries.
#
# The regression this exists for: Rift used to ship `reqwest` 0.12 *and* 0.13,
# `rand` 0.8 *and* 0.10, and `zbus` 3 *and* 5 in one binary — two TLS stacks and
# two rand generations. Those are gone (the direct `reqwest` is 0.13, matching
# what Tauri pulls in), and the list below is the state *after* that fix, so any
# of them coming back fails the build.
#
# To see the full picture: cargo tree -d --locked
#
# Run this where `cargo` is on PATH and the workspace resolves. Invoking it from
# PowerShell on Windows resolves `bash` to WSL's, which has no cargo, and a
# `CARGO_HOME` without the registry cannot resolve the lockfile either; both used
# to exit silently (see the capture below).
set -euo pipefail

baseline=$(cat <<'EOF'
bitflags
cpufeatures
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
winnow
EOF
)

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
  echo "These crates are no longer duplicated; remove them from the baseline:"
  echo "$fixed" | sed 's/^/  /'
  echo
fi

if [ -n "$new_duplicates" ]; then
  echo "::error::the dependency graph gained duplicate crate versions"
  echo
  echo "$new_duplicates" | sed 's/^/  /'
  echo
  echo "Two majors of one crate means duplicated types, an extra TLS stack, or a"
  echo "second rand generation in the shipped binary. Unify the versions; if a"
  echo "transitive dependency makes that impossible, add the crate to the"
  echo "baseline in this script with a note saying which dependency pins the"
  echo "older major."
  echo
  cargo tree -d --locked
  exit 1
fi

echo "No new duplicate versions: ${current//$'\n'/ }"
