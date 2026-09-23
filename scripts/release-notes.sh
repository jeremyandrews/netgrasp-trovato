#!/usr/bin/env bash
#
# Print one version's section of CHANGELOG.md, without its heading, for the
# GitHub Release notes. The section ends at the next second-level heading, so
# the "Before 1.0.0" history below the first release is not part of its notes.
#
# Usage: scripts/release-notes.sh 1.0.0

set -euo pipefail

version="${1:?usage: $0 <version>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

notes="$(awk -v heading="## [$version]" '
    index($0, heading) == 1 { found = 1; next }
    found && /^## /          { exit }
    found                    { print }
' "$root/CHANGELOG.md")"

# Trim the leading blank lines, and refuse an empty section.
notes="$(printf '%s\n' "$notes" | sed -e '/./,$!d')"
if ! printf '%s' "$notes" | grep -q '[^[:space:]]'; then
    echo "error: CHANGELOG.md has no section for [$version]" >&2
    exit 1
fi

printf '%s\n' "$notes"
