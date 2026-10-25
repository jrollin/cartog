#!/usr/bin/env bash
# Fail when a version anchor bumped by release.sh drifts from Cargo.toml's
# [workspace.package] version. An optional TAG argument (e.g. v0.34.2) must match too.
set -euo pipefail

cd "$(dirname "$0")/.."

die() { echo "error: $*" >&2; exit 1; }

expected=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml)
[[ -n "$expected" ]] || die "no version under [workspace.package] in Cargo.toml"

failures=0
check() {
  local label=$1 actual=$2
  if [[ -z "$actual" ]]; then
    echo "missing: no version found in $label" >&2
    failures=$((failures + 1))
  elif [[ "$actual" != "$expected" ]]; then
    echo "mismatch: $label is $actual, Cargo.toml [workspace.package] is $expected" >&2
    failures=$((failures + 1))
  fi
}

while read -r crate version; do
  check "Cargo.toml dependency $crate" "$version"
done < <(sed -n 's/^\(cartog-[a-z-]*\) = .*version = "\([^"]*\)".*/\1 \2/p' Cargo.toml)

check ".claude-plugin/plugin.json" \
  "$(sed -n 's/.*"version": "\([^"]*\)".*/\1/p' .claude-plugin/plugin.json | head -1)"
# First "version" key only: release.sh bumps the top-level one, never a nested field.
check "editors/vscode/package.json" \
  "$(sed -n 's/.*"version": "\([^"]*\)".*/\1/p' editors/vscode/package.json | head -1)"
check "skills/cartog-install/SKILL.md PLUGIN_VERSION" \
  "$(sed -n 's/^PLUGIN_VERSION=//p' skills/cartog-install/SKILL.md)"

if [[ $# -gt 0 ]]; then
  check "tag $1" "${1#v}"
fi

((failures == 0)) || die "$failures version anchor(s) out of sync with $expected"
echo "version anchors in sync: $expected"
