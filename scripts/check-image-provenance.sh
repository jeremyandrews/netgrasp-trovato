#!/usr/bin/env bash
# Check that a published kernel image was built from one commit, on every
# platform the demo and the cluster run.
#
# Usage:
#   scripts/check-image-provenance.sh IMAGE@sha256:DIGEST COMMIT
#
# The image index must carry a linux/amd64 and a linux/arm64 image, and the
# build provenance of each must name COMMIT as its `vcs:revision`, once. A
# platform with no provenance, no revision or another revision fails, even when
# the other platform's is right: each platform is a separate build, and a pin is
# only as good as the one that runs.
#
# scripts/sync-kernel-release.sh --set-commit calls it before rewriting the pin;
# scripts/test-check-image-provenance.sh tests it against a stand-in `docker`.
set -euo pipefail

die() {
    echo "error: $*" >&2
    exit 1
}

ref="${1:-}"
sha="${2:-}"
[ -n "$ref" ] && [ -n "$sha" ] ||
    die "usage: $0 IMAGE@sha256:DIGEST COMMIT"

# Read into a variable first: `grep -q` stops reading at its first match, and
# under pipefail the writer's SIGPIPE would fail the pipeline.
platforms="$(docker buildx imagetools inspect "$ref")" ||
    die "cannot inspect ${ref}"
for platform in linux/amd64 linux/arm64; do
    printf '%s\n' "$platforms" |
        grep -Eq "^[[:space:]]*Platform:[[:space:]]+${platform}[[:space:]]*$" ||
        die "${ref} carries no ${platform} image"
    # One platform's provenance: `index` on the per-platform map, which is
    # `null` for a platform that has none.
    provenance="$(docker buildx imagetools inspect "$ref" \
        --format "{{json (index .Provenance \"${platform}\")}}")" ||
        die "cannot read the ${platform} provenance of ${ref}"
    built="$(printf '%s' "$provenance" | grep -o '"vcs:revision": *"[0-9a-f]*"' |
        sed 's/.*"\([0-9a-f]*\)"$/\1/' | sort -u || true)"
    [ -n "$built" ] ||
        die "the ${platform} image of ${ref} names no vcs:revision"
    [ "$built" = "$sha" ] ||
        die "the ${platform} image of ${ref} was built from '$(echo "$built" | tr '\n' ' ' | sed 's/ $//')', not ${sha}"
done
echo "${ref}: linux/amd64 and linux/arm64 both built from ${sha}"
