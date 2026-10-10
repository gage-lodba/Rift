#!/usr/bin/env bash
# Keeps the three places that state the toolchain version in agreement:
#
#   Cargo.toml          `rust-version` in [workspace.package]  — the declaration
#   rust-toolchain.toml `channel`                              — the local pin
#   .github/workflows/  `toolchain:` inputs                    — the CI pin
#
# A workflow that uses the action with *no* `toolchain:` input is the quietest
# way to lose the third one: there is no line for a comparison to find, so the
# count of uses against the count of pins is checked too.
#
# They are not redundant. `rust-version` is what Cargo enforces and what clippy
# reads to avoid suggesting APIs newer than the floor; the pin is what makes
# every ordinary build *be* that floor. If the pin drifts above the declaration,
# the declared minimum is never exercised and quietly stops being true — which is
# how this repository ended up pinned to 1.98 while nothing in the graph needed
# more than 1.88.
#
# Raising the floor is fine; it just has to happen in all three at once, which is
# what this fails on.
set -euo pipefail

fail=0

# `rust-version` is conventionally written `1.88` and a toolchain channel
# `1.88.0`; those are the same version, so compare on major.minor.
normalize() {
  echo "$1" | awk -F. '{ printf "%s.%s", $1, $2 }'
}

cargo_msrv=$(grep -m1 '^rust-version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
toolchain_pin=$(grep -m1 '^channel' rust-toolchain.toml | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$cargo_msrv" ] || [ -z "$toolchain_pin" ]; then
  echo "::error::could not read rust-version from Cargo.toml ($cargo_msrv) or channel from rust-toolchain.toml ($toolchain_pin)"
  exit 1
fi

echo "declared rust-version = $cargo_msrv"
echo "rust-toolchain channel = $toolchain_pin"

if [ "$(normalize "$cargo_msrv")" != "$(normalize "$toolchain_pin")" ]; then
  echo "::error::rust-toolchain.toml pins $toolchain_pin but Cargo.toml declares rust-version = $cargo_msrv."
  echo "Pin the toolchain to the declared floor so every build checks it, or raise both together."
  fail=1
fi

# Every workflow pin must also match, or CI and the release job build with a
# toolchain other than the one this file claims.
while IFS= read -r found; do
  file=${found%%:*}
  # Everything after `path:line:` — taking the version from the whole grep line
  # would read the *line number*'s colon as the version separator.
  version=$(echo "${found#*:*:}" | sed -E 's/^[[:space:]]*toolchain:[[:space:]]*//; s/[[:space:]]*#.*$//; s/[[:space:]]*$//')
  # `toolchain: stable` is the defect this whole file exists to prevent, and the
  # action accepts it silently. There is no version to compare, so it has to
  # fail here rather than fall through to the comparison below.
  if ! echo "$version" | grep -qE '^[0-9]+\.[0-9]+'; then
    echo "::error::$file pins toolchain '$version', which is not a version. Pin the declared floor explicitly (e.g. '$toolchain_pin')."
    fail=1
    continue
  fi
  if [ "$(normalize "$version")" != "$(normalize "$cargo_msrv")" ]; then
    echo "::error::$file pins toolchain $version but Cargo.toml declares $cargo_msrv"
    fail=1
  fi
done < <(grep -rnE '^[[:space:]]*toolchain: ' .github/workflows/ || true)

# The loop above only compares pins that exist. A workflow that uses the action
# with no `toolchain:` input states no version at all, so it would leave that
# loop silent and this file would still print "consistent" while the action
# installed whatever `stable` is that week — the same drift, by omission.
# Counted per file so the failure names the file that has to be fixed.
while IFS= read -r wf; do
  uses_count=$(grep -cE '^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]+dtolnay/rust-toolchain@' "$wf" || true)
  pin_count=$(grep -cE '^[[:space:]]*toolchain: ' "$wf" || true)
  if [ "$uses_count" != "$pin_count" ]; then
    echo "::error::$wf uses dtolnay/rust-toolchain $uses_count time(s) but has $pin_count 'toolchain:' input(s). Every use needs an explicit version, or the action installs the moving 'stable' branch."
    fail=1
  fi
done < <(grep -rlE '^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]+dtolnay/rust-toolchain@' .github/workflows/ || true)

if [ "$fail" -ne 0 ]; then
  exit 1
fi

echo "toolchain version is consistent across Cargo.toml, rust-toolchain.toml, and the workflows"
