#!/usr/bin/env bash
# Keeps the `wasm-bindgen` version Cargo.lock resolves in agreement with the
# `wasm-bindgen-cli` the frontend test job installs.
#
# They are not redundant, and the failure mode is worse than ordinary version
# skew. The runner that executes a wasm test binary refuses one built against a
# different `wasm-bindgen` than itself — it errors out rather than misbehaving,
# but the message names a schema version and a binary path and says nothing
# about which of the two numbers to change. `wasm-bindgen` is an ordinary
# transitive dependency that Dependabot will happily bump on its own, so without
# this check the frontend test job breaks on an unrelated pull request and the
# fix is a guess.
#
# Same shape as check-toolchain-consistency.sh: the version is stated in two
# places, and the only way to keep them honest is to compare them.
set -euo pipefail

fail=0

# `Cargo.lock` is a sequence of `[[package]]` blocks with `name` then `version`.
# Indent first so the version line can be matched on leading whitespace whatever
# the file's line endings are (a Windows checkout has CRLF, and a `$`-anchored
# match on the name line would silently find nothing there).
lock_version=$(awk '/^\[\[package\]\]/{p=0} /^[[:space:]]*name = "wasm-bindgen"$/{p=1} p && /^[[:space:]]*version = /{print; exit}' Cargo.lock |
  tr -d '\r"' | awk '{print $3}')

if [ -z "$lock_version" ]; then
  echo "::error::could not find a wasm-bindgen package version in Cargo.lock"
  exit 1
fi
echo "Cargo.lock wasm-bindgen = $lock_version"

# The test job installs it as `tool: wasm-bindgen-cli@<version>`.
cli_versions=$(grep -rhoE 'wasm-bindgen-cli@[0-9]+\.[0-9]+\.[0-9]+' .github/workflows/ | sed -E 's/.*@//' | sort -u)

if [ -z "$cli_versions" ]; then
  echo "::error::no wasm-bindgen-cli@<version> install found in .github/workflows/."
  echo "The frontend test job needs the runner, version-matched to Cargo.lock."
  exit 1
fi

for cli_version in $cli_versions; do
  echo "workflow wasm-bindgen-cli = $cli_version"
  if [ "$cli_version" != "$lock_version" ]; then
    echo "::error::the workflow installs wasm-bindgen-cli $cli_version but Cargo.lock resolves wasm-bindgen $lock_version."
    echo "They must match exactly: the test runner rejects a binary built against a different wasm-bindgen."
    echo "Raise both together — the pin in .github/workflows/ci.yml, and Cargo.lock via 'cargo update -p wasm-bindgen'."
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  exit 1
fi

echo "wasm-bindgen version is consistent between Cargo.lock and the frontend test job"
