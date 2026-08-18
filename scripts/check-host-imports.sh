#!/usr/bin/env bash
# Assert the compiled module imports exactly the host interfaces its manifest
# declares.
#
# WHY THIS EXISTS
# `[capabilities] host_interfaces` in netgrasp.info.toml is a deny-unless-declared
# allowlist: the kernel builds a per-plugin linker from it and refuses the module
# at load time if it imports anything not on the list. The list therefore has to
# be derived from the ARTIFACT, not from reading the source — the SDK's tap
# macros generate host calls that no tap body contains, so "grep the source for
# host functions" gets the wrong answer.
#
# Declaring one too MANY is the failure this catches in the other direction: the
# manifest then hands the plugin a capability it does not use, which is a widened
# blast radius nobody asked for and nothing else would notice.
#
# The module names appear verbatim in the wasm import section, so a plain grep
# over the binary is enough and this needs no wasm tooling installed.
#
# Usage: scripts/check-host-imports.sh [path-to-wasm]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM="${1:-$ROOT/target/wasm32-wasip1/release/netgrasp.wasm}"
MANIFEST="$ROOT/plugins/netgrasp/netgrasp.info.toml"

if [ ! -f "$WASM" ]; then
    echo "error: no module at $WASM — run cargo build --target wasm32-wasip1 --release" >&2
    exit 1
fi

# What the module actually imports.
imported="$(LC_ALL=C grep -ao 'trovato:kernel/[a-z0-9-]*' "$WASM" \
    | sed 's|trovato:kernel/||' | sort -u)"

# What the manifest declares. Read only the host_interfaces array: the manifest
# has other quoted-string arrays (migrations, db_tables, taps) and a naive grep
# for quoted words would collect all of them.
declared="$(awk '
    /^host_interfaces *= *\[/ { inside = 1; next }
    inside && /\]/            { inside = 0 }
    inside                    { if (match($0, /"[^"]+"/)) print substr($0, RSTART + 1, RLENGTH - 2) }
' "$MANIFEST" | sort -u)"

if [ "$imported" != "$declared" ]; then
    echo "error: host_interfaces does not match the module's imports" >&2
    echo "--- module imports ---" >&2
    echo "$imported" >&2
    echo "--- manifest declares ---" >&2
    echo "$declared" >&2
    diff <(echo "$declared") <(echo "$imported") >&2 || true
    exit 1
fi

echo "host interfaces match ($(echo "$imported" | tr '\n' ' '))"
