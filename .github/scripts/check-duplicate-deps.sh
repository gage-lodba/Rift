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
# one binary — two TLS stacks. That is gone, and the lists below are the state of
# the native app's graph, so any of them coming back fails the build.
#
# ## The baseline is per host, and records exact versions
#
# `cargo tree` filters by the target platform, so the duplicated set genuinely
# differs per platform: the `windows-*` generation is not in the graph on Linux
# or macOS, macOS's Cocoa/objc2 crates are not in the Linux graph, and Linux's
# rustix/getrandom families are not in the macOS graph. One combined baseline would
# flag
# every platform but the one it was recorded on, for reasons that are not
# regressions.
#
# Every line is `<crate> <version>` for one version that appears more than once.
# Recording names only (as this script used to) makes the exact regression it
# exists for invisible: a crate already on the list can gain a *third* version,
# or swap a version for a newer one, and the name set is unchanged. The pair set
# also makes the baseline a reviewable snapshot instead of a list that silently
# widens. A recorded pair that is no longer duplicated is a failure too:
# retaining it would let that exact pair pass as "already baselined" if it is
# reintroduced. Lines are sorted with LC_ALL=C (so `v0.12.1` sorts before
# `v0.3.8`); keep that order when editing.
#
# Generation, with the workspace resolution committed in Cargo.lock:
#
#   # Linux (native):
#   cargo tree -d --locked --workspace
#   # Windows and macOS (target override from a Linux checkout):
#   cargo tree -d --locked --workspace --target x86_64-pc-windows-msvc
#   cargo tree -d --locked --workspace --target aarch64-apple-darwin
#
# The Linux list was regenerated natively for this baseline. The Windows and
# macOS lists were regenerated from a Linux checkout with the target override,
# which is why they are recorded here in the same exact-version form: `cargo
# tree` applies the target to dependency resolution, and for those two targets
# the duplicated pairs are all normal dependencies (`cargo tree -d` and `cargo
# tree -d --edges normal` print the same pairs), whose versions are selected per
# target and do not depend on the host. A
# host-only build-dependency duplicate, if one ever appears, is exactly the kind
# of difference the native CI matrix surfaces as a new pair. There is no
# Windows/macOS host available here to run the native command, so CI running
# this guard on the native matrix hosts is the final check; regenerate on the
# host when one is available.
#
# If a host-only difference does surface, CI prints the exact new pair; record it
# with the dependency that pins it rather than deleting the guard's input.
#
# ## Known duplicates with no lever here
#
#   - Linux: `pollster` 0.4 through `rfd`'s xdg-portal stack beside the direct
#     `pollster` 1.0 (which `souvlaki` 0.9.3 also selects), `rand` 0.9 through
#     `ashpd` beside the direct `rand` 0.10, `thiserror` 1 beside 2, and the
#     rustix/linux-raw-sys/getrandom generations.
#   - Linux: `nom` 7 through `vulkano`'s build script (it parses the Vulkan XML
#     registry) beside `nom` 8 through `tree_magic_mini`, which `wl-clipboard-rs`
#     uses for MIME-type detection and `arboard`'s `wayland-data-control` feature
#     pulls in for the Wayland clipboard. Both are leaf parsers: nothing but
#     bytes and parse results crosses the boundary, and neither version is
#     selected by this workspace.
#   - All hosts: `base64` 0.22 (hyper-util) beside 0.23 (reqwest 0.13.5's
#     response decoding); both are internal wire encoders and no types cross the
#     boundary.
#   - Windows: the `windows-*` generation is split by different transitive pins;
#     `windows-sys` appears for 0.52 (`winit`), 0.60 (`rfd`) and 0.61 (`arboard`,
#     `dirs-sys`, `mio`) consumers.
#   - macOS: the objc2 / core-foundation families split across generations
#     (`winit` 0.30 pins the 0.5 / 0.9 generation; `arboard`, `cpal` and the
#     `rfd` stack pull 0.6 / 0.10), and `core-graphics-types` splits with them:
#     `souvlaki` 0.9.3's `core-graphics` 0.25 uses 0.2 while `winit`'s
#     `core-graphics` 0.23 still uses 0.1.
#
# To see the full picture: cargo tree -d --locked
#
# When the baseline legitimately needs to change (a new dependency pins an older
# major), regenerate it for all three targets and update these lists in the same
# commit, so the diff is reviewable rather than the guard silently widening.
#
# Run this where `cargo` is on PATH and the workspace resolves. Invoking it from
# PowerShell on Windows resolves `bash` to WSL's, which has no cargo, and a
# `CARGO_HOME` without the registry cannot resolve the lockfile either; both used
# to exit silently (see the capture below).
set -euo pipefail

# sort/comm must agree on ordering regardless of the invoking host's locale.
export LC_ALL=C

# Windows-only: Microsoft's windows-* generation.
baseline_windows=$(cat <<'EOF'
base64 v0.22.1
base64 v0.23.1
bitflags v1.3.2
bitflags v2.13.1
hashbrown v0.15.5
hashbrown v0.17.1
miniz_oxide v0.8.9
miniz_oxide v0.9.1
syn v2.0.119
syn v3.0.5
windows-sys v0.52.0
windows-sys v0.60.2
windows-sys v0.61.2
windows-targets v0.52.6
windows-targets v0.53.5
windows_x86_64_msvc v0.52.6
windows_x86_64_msvc v0.53.1

EOF
)

# Linux-only: the rustix/linux-raw-sys, getrandom, thiserror and rand families
# split across generations; the direct `rand` 0.10 is beside `ashpd`'s 0.9, and
# the direct `pollster` 1.0 beside `rfd`'s 0.4.
baseline_linux=$(cat <<'EOF'
base64 v0.22.1
base64 v0.23.1
bitflags v1.3.2
bitflags v2.13.1
getrandom v0.3.4
getrandom v0.4.3
hashbrown v0.15.5
hashbrown v0.17.1
linux-raw-sys v0.12.1
linux-raw-sys v0.4.15
miniz_oxide v0.8.9
miniz_oxide v0.9.1
nom v7.1.3
nom v8.0.0
pollster v0.4.0
pollster v1.0.3
rand_core v0.10.1
rand_core v0.9.5
rand v0.10.4
rand v0.9.5
rustix v0.38.44
rustix v1.1.4
syn v2.0.119
syn v3.0.5
thiserror-impl v1.0.69
thiserror-impl v2.0.21
thiserror v1.0.69
thiserror v2.0.21

EOF
)

# macOS-only: the Cocoa/objc2 families. `core-graphics-types` 0.1 comes through
# `winit`'s `core-graphics` 0.23 while `souvlaki` 0.9.3's `core-graphics` 0.25
# uses 0.2.
baseline_macos=$(cat <<'EOF'
base64 v0.22.1
base64 v0.23.1
bitflags v1.3.2
bitflags v2.13.1
block2 v0.5.1
block2 v0.6.2
core-foundation v0.10.1
core-foundation v0.9.4
core-graphics-types v0.1.3
core-graphics-types v0.2.0
core-graphics v0.23.2
core-graphics v0.25.0
hashbrown v0.15.5
hashbrown v0.17.1
miniz_oxide v0.8.9
miniz_oxide v0.9.1
objc2-app-kit v0.2.2
objc2-app-kit v0.3.2
objc2-foundation v0.2.2
objc2-foundation v0.3.2
objc2 v0.5.3
objc2 v0.6.4
syn v2.0.119
syn v3.0.5

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

# `--workspace` covers the five native members under `crates/`; the legacy web
# frontend member is gone, so there is nothing to exclude any more.
tree_args=(tree -d --locked --workspace)

baseline=$(printf '%s\n' "$host_baseline" |
  sed -E '/^[[:space:]]*$/d; s/^[[:space:]]+//; s/[[:space:]]+$//' |
  sort -u)

# A baseline line that is not `<crate> <version>` would be compared as an exact
# string and could never match a real pair, so it is almost certainly a
# half-finished edit. Fail rather than let it silently widen the guard. An empty
# list is valid: it means this host is expected to have no duplicates, which is
# the one state where the "No duplicate versions found." branch is reached with
# a real `cargo tree -d` (not just a broken invocation).
if [ -n "$baseline" ] && printf '%s\n' "$baseline" | grep -vEq '^[a-zA-Z0-9_-]+ v[0-9]'; then
  echo "::error::the $host baseline has entries that are not '<crate> <version>':"
  printf '%s\n' "$baseline" | grep -vE '^[a-zA-Z0-9_-]+ v[0-9]' | sed 's/^/  /'
  exit 1
fi

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
tree_output=$(cargo "${tree_args[@]}") || status=$?
if [ "$status" -ne 0 ]; then
  echo "::error::'cargo ${tree_args[*]}' failed (exit $status). This guard needs cargo on PATH and a"
  echo "::error::resolvable workspace; run it from the repository root with that toolchain's shell."
  echo "::error::On Windows that means Git Bash, not WSL's bash — WSL has no cargo. Nothing about the"
  echo "::error::dependency graph was checked."
  exit 1
fi

# The `|| true` is load-bearing. `cargo tree -d` prints nothing when the graph
# has no duplicates, so `grep` exits 1; under `set -euo pipefail` the assignment
# would kill the script exactly in the no-duplicates case the branch below is for.
# `sort -u` collapses the same version reported twice for different feature sets
# (one version is not a duplicate), and the awk keeps only crates that still have
# more than one distinct version.
current=$(
  printf '%s\n' "$tree_output" |
    grep -E '^[a-zA-Z0-9_-]+ v[0-9]' |
    sed -E 's/^([a-zA-Z0-9_-]+) (v[0-9][^ ]*).*/\1 \2/' |
    sort -u |
    awk '{ versions[$1] = versions[$1] $0 "\n"; count[$1]++ }
         END { for (name in count) if (count[name] > 1) printf "%s", versions[name] }' |
    sort -u || true
)

if [ -z "$current" ]; then
  echo "No duplicate versions found."
  if [ -n "$baseline" ]; then
    echo "The $host baseline is stale; remove these entries:"
    printf '%s\n' "$baseline" | sed 's/^/  /'
  fi
  exit 0
fi

if [ -n "$baseline" ]; then
  # Pairs in the graph that are not recorded for this host: a new duplicate
  # crate, a third version of an already-baselined crate, or a version bump
  # since the baseline was recorded.
  new_duplicates=$(comm -13 <(printf '%s\n' "$baseline") <(printf '%s\n' "$current"))
  # Recorded pairs that are no longer duplicated. These are a hard failure too:
  # leaving one in the list would let that exact pair pass as "already
  # baselined" if it is reintroduced later.
  stale=$(comm -23 <(printf '%s\n' "$baseline") <(printf '%s\n' "$current"))
else
  new_duplicates="$current"
  stale=""
fi

# The pair diff above already fails a crate that gained a version, but the
# distinct-version count is the invariant this guard exists for: state it
# explicitly so the diagnostic names the crate and both counts (for example
# "syn (baseline records 2, graph has 3)"), not merely the new pair.
count_regressions=""
if [ -n "$baseline" ]; then
  count_regressions=$(
    awk '
      NR == FNR { recorded[$1]++; next }
      { found[$1]++ }
      END {
        for (name in found) {
          if (name != "" && found[name] > recorded[name]) {
            printf "%s (baseline records %d, graph has %d)\n", name, recorded[name], found[name]
          }
        }
      }
    ' <(printf '%s\n' "$baseline") <(printf '%s\n' "$current") |
      sort
  )
fi

# A stale entry is a hard failure, not just advice. Keeping a pair after the
# graph stopped duplicating it would let a later reintroduction of that exact
# pair pass as "already baselined", which is the regression this snapshot exists
# to catch. A dependency update that removes a duplicate must update this list.
stale_failure=0
if [ -n "$stale" ]; then
  echo "::error::the $host baseline records pairs that are no longer duplicated; remove them so a reintroduction is caught again:"
  printf '%s\n' "$stale" | sed 's/^/  /'
  echo
  stale_failure=1
fi

if [ -n "$new_duplicates" ] || [ -n "$count_regressions" ] || [ "$stale_failure" -eq 1 ]; then
  if [ -n "$new_duplicates" ] || [ -n "$count_regressions" ]; then
    echo "::error::the dependency graph gained duplicate crate versions on $host"
    echo
    if [ -n "$count_regressions" ]; then
      echo "Crates with more distinct versions than the baseline records:"
      printf '%s\n' "$count_regressions" | sed 's/^/  /'
      echo
    fi
    if [ -n "$new_duplicates" ]; then
      echo "Duplicate versions not recorded in the baseline:"
      printf '%s\n' "$new_duplicates" | sed 's/^/  /'
      echo
    fi
    echo "Two majors of one crate means duplicated types, an extra TLS stack, or a"
    echo "second rand generation in the shipped binary. Unify the versions; if a"
    echo "transitive dependency makes that impossible, record the exact"
    echo "'<crate> <version>' pairs in this host's baseline with a note saying which"
    echo "dependency pins the older version, and update all three host lists in the"
    echo "same commit (the baseline is a version snapshot, not a name allow-list)."
    echo
    cargo "${tree_args[@]}" || true
  fi
  exit 1
fi

echo "No new duplicate versions on $host:"
printf '%s\n' "$current" | sed 's/^/  /'
