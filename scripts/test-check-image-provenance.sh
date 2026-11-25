#!/usr/bin/env bash
# Tests for scripts/check-image-provenance.sh, against a stand-in `docker` that
# answers `buildx imagetools inspect` from fixture text. Needs nothing but bash.
#
# Usage:
#   scripts/test-check-image-provenance.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

good=1111111111111111111111111111111111111111
other=2222222222222222222222222222222222222222

# The stand-in reads the platform list and each platform's revision ("none" for
# a platform with no provenance) from files the case writes.
mkdir -p "$work/bin"
cat > "$work/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -eu
fixture="${FIXTURE:?}"
case "$*" in
    *--format*)
        platform="$(printf '%s' "$*" | sed -n 's/.*Provenance "\([^"]*\)".*/\1/p')"
        revision="$(cat "$fixture/$(echo "$platform" | tr / _)" 2>/dev/null || echo none)"
        if [ "$revision" = none ]; then
            echo null
        else
            printf '{"SLSA":{"buildDefinition":{"externalParameters":{"request":{"args":{"vcs:revision": "%s"}}}}}}\n' "$revision"
        fi
        ;;
    *)
        echo "Name:      example/image@sha256:0"
        echo "MediaType: application/vnd.oci.image.index.v1+json"
        echo "Manifests:"
        while read -r platform; do
            echo "  Platform:  ${platform}"
        done < "$fixture/platforms"
        ;;
esac
STUB
chmod +x "$work/bin/docker"

failures=0
# expect PASS|FAIL NAME AMD64 ARM64 [PLATFORMS...]
expect() {
    local want="$1" name="$2" amd64="$3" arm64="$4"
    shift 4
    local fixture="$work/$name"
    mkdir -p "$fixture"
    printf '%s\n' "$@" > "$fixture/platforms"
    [ "$amd64" = none ] || echo "$amd64" > "$fixture/linux_amd64"
    [ "$arm64" = none ] || echo "$arm64" > "$fixture/linux_arm64"
    local got=PASS
    PATH="$work/bin:$PATH" FIXTURE="$fixture" \
        "$here/check-image-provenance.sh" example/image@sha256:0 "$good" \
        >"$fixture/out" 2>&1 || got=FAIL
    if [ "$got" = "$want" ]; then
        echo "ok   ${name}"
    else
        echo "FAIL ${name}: wanted ${want}, got ${got}"
        sed 's/^/     /' "$fixture/out"
        failures=$((failures + 1))
    fi
}

expect PASS both-platforms-built-from-the-commit "$good" "$good" linux/amd64 linux/arm64
expect FAIL arm64-has-no-provenance "$good" none linux/amd64 linux/arm64
expect FAIL amd64-has-no-provenance none "$good" linux/amd64 linux/arm64
expect FAIL arm64-built-from-another-commit "$good" "$other" linux/amd64 linux/arm64
expect FAIL both-built-from-another-commit "$other" "$other" linux/amd64 linux/arm64
expect FAIL no-arm64-image "$good" "$good" linux/amd64

[ "$failures" -eq 0 ] || {
    echo "${failures} case(s) failed" >&2
    exit 1
}
echo "all cases passed"
