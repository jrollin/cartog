#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="Cargo.toml"

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

# update Cargo.toml
sed "s/^version = \".*\"/version = \"${NEW}\"/" "$CARGO_TOML" > "$CARGO_TOML.tmp" && mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# update Cargo.lock
cargo generate-lockfile --quiet 2>/dev/null || true

info "committing version bump"
git add "$CARGO_TOML" Cargo.lock
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
