#!/usr/bin/env bash
# Fail unless the package version in Cargo.toml and the plugin version in
# herdr-plugin.toml are the same; both are bumped by hand.
#
#   scripts/check-versions.sh [CARGO_TOML] [PLUGIN_MANIFEST]
set -euo pipefail

cargo_toml=${1:-Cargo.toml}
manifest=${2:-herdr-plugin.toml}

# The first top-level `version = "..."` of each file: in Cargo.toml it belongs
# to [package], as dependencies use inline tables.
top_level_version() {
  sed -nE '/^version[[:space:]]*=/{s/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p;q;}' "$1"
}

cargo_version=$(top_level_version "$cargo_toml")
plugin_version=$(top_level_version "$manifest")

if [[ -z $cargo_version || -z $plugin_version ]]; then
  echo "error: could not read the version from $cargo_toml or $manifest" >&2
  exit 1
fi

if [[ $cargo_version != "$plugin_version" ]]; then
  echo "error: $cargo_toml has version $cargo_version but $manifest has $plugin_version" >&2
  exit 1
fi

echo "versions match: $cargo_version"
