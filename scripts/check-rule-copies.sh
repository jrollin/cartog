#!/usr/bin/env bash
# Fail when the snippet block in docs/agent-snippet.md drifts from the rule text
# `cartog ide --rules` writes. Optional SNIPPET and BODY arguments override the paths.
set -euo pipefail

cd "$(dirname "$0")/.."

snippet=${1:-docs/agent-snippet.md}
body=${2:-crates/cartog/src/commands/ide/rules/body.md}

die() { echo "error: $*" >&2; exit 1; }

[[ -f "$snippet" ]] || die "$snippet not found"
[[ -f "$body" ]] || die "$body not found"

# The first ```markdown fence holds the copy-paste snippet.
block=$(awk '/^```markdown$/ { inside = 1; found = 1; next }
             inside && /^```$/ { exit }
             inside { print }
             END { if (!found) exit 2 }' "$snippet") \
  || die "no \`\`\`markdown block in $snippet"

if ! diff -u --label "$snippet (snippet block)" --label "$body" \
  <(printf '%s\n' "$block") "$body"; then
  die "the snippet block in $snippet differs from $body; edit both in the same change"
fi
