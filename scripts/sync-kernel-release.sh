#!/usr/bin/env bash
#
# Write the pinned Trovato release from kernel-release.toml into every file that
# names it. kernel-release.toml is the only place the release is authored; this
# script is how that one fact reaches the places that repeat it, and the tests in
# plugins/netgrasp/src/lib.rs are what fail when they disagree.
#
#   scripts/sync-kernel-release.sh                  rewrite from the current pin
#   scripts/sync-kernel-release.sh --set-version X.Y.Z
#                                                   resolve the tag, then rewrite
#
# This does NOT touch the plugin's own version ([workspace.package] version),
# which is semantic from 1.0.0 on and moves independently of the kernel.
#
# Run it from anywhere: the repository root is resolved from this file's location.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="$root/kernel-release.toml"

die() {
    echo "sync-kernel-release: $*" >&2
    exit 1
}

# Read a bare `key = "value"` out of the contract file.
field() {
    local key="$1"
    sed -n "s/^${key} = \"\\(.*\\)\"\$/\\1/p" "$contract" | head -n 1
}

# Replace the whole file with the result of a sed program, leaving the file
# untouched if the program changes nothing. Written through a temporary file
# because `sed -i` spells its argument differently on BSD and on GNU.
rewrite() {
    local path="$1"
    shift
    local tmp
    tmp="$(mktemp)"
    sed "$@" "$path" > "$tmp"
    if cmp -s "$path" "$tmp"; then
        rm -f "$tmp"
    else
        cat "$tmp" > "$path"
        rm -f "$tmp"
        echo "  rewrote ${path#"$root"/}"
    fi
}

# Replace the lines between `<!-- kernel-release:begin -->` and its matching end
# marker with the contents of the given file, keeping the markers themselves.
# The body arrives as a file because BSD awk will not take a newline in `-v`.
write_block() {
    local path="$1" body_file="$2"
    grep -q '<!-- kernel-release:begin -->' "$path" ||
        die "$path has no kernel-release:begin marker"
    local tmp
    tmp="$(mktemp)"
    awk -v body_file="$body_file" '
        /<!-- kernel-release:begin -->/ {
            print
            while ((getline line < body_file) > 0) print line
            close(body_file)
            skip = 1
            next
        }
        /<!-- kernel-release:end -->/   { skip = 0 }
        !skip                          { print }
    ' "$path" > "$tmp"
    if cmp -s "$path" "$tmp"; then
        rm -f "$tmp"
    else
        cat "$tmp" > "$path"
        rm -f "$tmp"
        echo "  rewrote ${path#"$root"/}"
    fi
}

[ -f "$contract" ] || die "$contract is missing"

if [ "${1:-}" = "--set-version" ]; then
    new="${2:-}"
    echo "$new" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
        die "--set-version wants a version like 0.102.0, got '${new}'"
    echo "resolving v${new} ..."
    # `^{}` asks for the commit a tag points at, so an annotated tag resolves to
    # the commit rather than to the tag object.
    resolved="$(git ls-remote https://github.com/jeremyandrews/trovato.git \
        "refs/tags/v${new}^{}" | cut -f 1)"
    [ -n "$resolved" ] ||
        die "v${new} does not resolve to a commit in jeremyandrews/trovato"
    rewrite "$contract" \
        -e "s/^version = \".*\"\$/version = \"${new}\"/" \
        -e "s/^rev = \".*\"\$/rev = \"${resolved}\"/"
elif [ -n "${1:-}" ]; then
    die "unknown argument '$1'"
fi

version="$(field version)"
rev="$(field rev)"
[ -n "$version" ] || die "kernel-release.toml has no version"
[ -n "$rev" ] || die "kernel-release.toml has no rev"

api="${version%.*}"
major="${version%%.*}"
minor="${api#*.}"

echo "Trovato ${version} (rev ${rev}), plugin API ${api} = (${major}, ${minor})"

# Both dependency revisions: the SDK the module is built against and the kernel
# the host-in-the-loop test drives. They must stay equal.
rewrite "$root/Cargo.toml" \
    -e "/^trovato-sdk = /s/rev = \"[0-9a-f]*\"/rev = \"${rev}\"/" \
    -e "/^trovato-kernel = /s/rev = \"[0-9a-f]*\"/rev = \"${rev}\"/"

# The plugin's declared API version. Not the plugin's own version, which is left
# alone on purpose.
rewrite "$root/plugins/netgrasp/netgrasp.info.toml" \
    -e "s/^api_version = \".*\"\$/api_version = \"${api}\"/"

# The published kernel image the demo runs.
rewrite "$root/docker-compose.demo.yml" \
    -e "s|image: ghcr.io/jeremyandrews/trovato:.*|image: ghcr.io/jeremyandrews/trovato:${version}|"

readme_block="$(mktemp)"
cat > "$readme_block" <<BLOCK
| | |
|---|---|
| pinned \`rev\` | \`${rev}\` |
| Trovato version | ${version} |
| \`KERNEL_API_VERSION\` | (${major}, ${minor}) |
| manifest \`api_version\` | \`${api}\` |
| demo kernel image | \`ghcr.io/jeremyandrews/trovato:${version}\` |
BLOCK
write_block "$root/README.md" "$readme_block"
rm -f "$readme_block"

echo "done"
