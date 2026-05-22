#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="Cargo.toml"
PLUGIN_JSON=".claude-plugin/plugin.json"
INSTALL_SKILL="skills/cartog-install/SKILL.md"

# ── helpers ──────────────────────────────────────────────────────────
die()  { echo "error: $*" >&2; exit 1; }
info() { echo "==> $*"; }

usage() {
  cat <<EOF
Usage: $(basename "$0") <major|minor|patch|VERSION>

Bump the version in Cargo.toml, commit, tag, and push.

Examples:
  $(basename "$0") patch        # 0.1.0 → 0.1.1
  $(basename "$0") minor        # 0.1.0 → 0.2.0
  $(basename "$0") major        # 0.1.0 → 1.0.0
  $(basename "$0") 2.3.4        # set exact version 2.3.4
EOF
  exit 1
}

# ── read current version from Cargo.toml ─────────────────────────────
current_version() {
  grep '^version' "$CARGO_TOML" | head -1 | sed 's/.*"\(.*\)"/\1/'
}

# ── compute next version ─────────────────────────────────────────────
next_version() {
  local cur="$1" bump="$2"
  local major minor patch
  IFS='.' read -r major minor patch <<< "$cur"

  case "$bump" in
    major) echo "$(( major + 1 )).0.0" ;;
    minor) echo "${major}.$(( minor + 1 )).0" ;;
    patch) echo "${major}.${minor}.$(( patch + 1 ))" ;;
    *)
      # treat as explicit version — validate semver-like format
      if [[ ! "$bump" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        die "invalid version '$bump' — expected major, minor, patch, or X.Y.Z"
      fi
      echo "$bump"
      ;;
  esac
}

# ── main ─────────────────────────────────────────────────────────────
[[ $# -eq 1 ]] || usage

# sanity checks
[[ -f "$CARGO_TOML" ]] || die "cannot find $CARGO_TOML — run from repo root"
BRANCH=$(git rev-parse --abbrev-ref HEAD)
[[ "$BRANCH" == "main" ]] || die "release must be cut from main (currently on '$BRANCH')"
git diff --quiet && git diff --cached --quiet || die "working tree is dirty — commit or stash first"

CURRENT=$(current_version)
NEW=$(next_version "$CURRENT" "$1")

if [[ "$CURRENT" == "$NEW" ]]; then
  die "version is already $CURRENT"
fi

TAG="v${NEW}"

if git rev-parse "$TAG" >/dev/null 2>&1; then
  die "tag $TAG already exists"
fi

info "bumping $CURRENT → $NEW"

# update workspace version + internal crate dependency versions in Cargo.toml
sed -e "/^\[workspace\.package\]/,/^\[/ s/version = \"${CURRENT}\"/version = \"${NEW}\"/" \
    -e "/^cartog-/ s/version = \"${CURRENT}\"/version = \"${NEW}\"/" \
    "$CARGO_TOML" > "$CARGO_TOML.tmp" && mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# update plugin.json
if [[ -f "$PLUGIN_JSON" ]]; then
  sed "s/\"version\": \".*\"/\"version\": \"${NEW}\"/" "$PLUGIN_JSON" > "$PLUGIN_JSON.tmp" && mv "$PLUGIN_JSON.tmp" "$PLUGIN_JSON"
fi

# Update the pinned PLUGIN_VERSION line inside the /cartog-install skill.
# The skill body uses `$PLUGIN_VERSION` as a token everywhere else, so we
# only need to maintain the single anchor line — no other version literals
# to keep in sync. The skill prose tells the agent to substitute the token
# at invocation time.
if [[ -f "$INSTALL_SKILL" ]]; then
  if ! grep -q "^PLUGIN_VERSION=" "$INSTALL_SKILL"; then
    die "$INSTALL_SKILL missing PLUGIN_VERSION= anchor line"
  fi
  sed "s/^PLUGIN_VERSION=.*/PLUGIN_VERSION=${NEW}/" \
      "$INSTALL_SKILL" > "$INSTALL_SKILL.tmp" && mv "$INSTALL_SKILL.tmp" "$INSTALL_SKILL"
  if ! grep -q "^PLUGIN_VERSION=${NEW}\$" "$INSTALL_SKILL"; then
    die "PLUGIN_VERSION bump did not stick in $INSTALL_SKILL"
  fi
fi

# update site version references (footers + badges)
for f in site/index.html site/usage.html; do
  if [[ -f "$f" ]]; then
    sed "s/v${CURRENT}/v${NEW}/g" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
  fi
done

# update Cargo.lock
cargo generate-lockfile --quiet 2>/dev/null || true

info "committing version bump"
git add "$CARGO_TOML" Cargo.lock "$PLUGIN_JSON" "$INSTALL_SKILL" site/
git commit -m "chore: bump version to ${NEW}"

info "tagging $TAG"
git tag -a "$TAG" -m "Release ${TAG}"

# generate / update CHANGELOG.md with git-cliff (tag must exist first)
if command -v git-cliff &>/dev/null; then
  info "generating CHANGELOG.md with git-cliff"
  git-cliff --config cliff.toml -o CHANGELOG.md
  if ! git diff --quiet CHANGELOG.md; then
    git add CHANGELOG.md
    git commit --amend --no-edit
  fi
else
  info "git-cliff not found — skipping local CHANGELOG.md update (CI will still generate release notes)"
fi

info "pushing commit and tag"
git push origin HEAD
git push origin "$TAG"

info "done — release workflow will build and publish $TAG"
