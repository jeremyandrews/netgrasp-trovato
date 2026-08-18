#!/usr/bin/env bash
# Build the plugin and lay out the overlay a Trovato deployment consumes.
#
# WHAT AN OVERLAY IS
# Trovato reads PLUGINS_DIR, TEMPLATES_DIR and STATIC_DIR as colon-separated
# SEARCH PATHS, and a later directory wins on a name collision. That is the whole
# integration seam: netgrasp appends its own directories and Trovato is not
# touched, not copied into, not patched.
#
# Two of the three need no build step, so this script only assembles the third:
#
#   templates/   used in place        →  append to TEMPLATES_DIR
#   static/      used in place        →  append to STATIC_DIR
#   overlay/plugins/netgrasp/        →  append to PLUGINS_DIR
#
# The plugin directory is assembled rather than used in place because the wasm is
# a build artifact under target/ while the manifest and the migrations are source,
# and `trovato plugin install` expects to find all three side by side:
#
#   netgrasp.wasm         the compiled module
#   netgrasp.info.toml    the manifest (taps, capabilities, record types)
#   migrations/           the SQL, run once each and tracked in plugin_migration
#
# Usage:
#   scripts/build-overlay.sh            # release build, then assemble
#   scripts/build-overlay.sh --no-build # assemble from an existing artifact

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET=wasm32-wasip1
SRC="$ROOT/plugins/netgrasp"
DEST="$ROOT/overlay/plugins/netgrasp"
WASM="$ROOT/target/$TARGET/release/netgrasp.wasm"

if [ "${1:-}" != "--no-build" ]; then
    echo "==> building netgrasp.wasm"
    rustup target add "$TARGET" >/dev/null 2>&1 || true
    cargo build --target "$TARGET" --release
fi

if [ ! -f "$WASM" ]; then
    echo "error: missing build artifact $WASM" >&2
    exit 1
fi

echo "==> assembling $DEST"
mkdir -p "$DEST"
cp "$WASM" "$DEST/netgrasp.wasm"
cp "$SRC/netgrasp.info.toml" "$DEST/netgrasp.info.toml"

# Mirror rather than merge, so a migration deleted here also disappears there.
# A stale migration left behind in an overlay is worse than a missing one: it is
# already recorded as applied, so nothing ever runs it again and nothing reports
# that it should not exist.
rm -rf "$DEST/migrations"
cp -R "$SRC/migrations" "$DEST/migrations"

# The manifest's capability list has to match the artifact beside it, and this is
# the moment both exist in one directory.
"$ROOT/scripts/check-host-imports.sh" "$DEST/netgrasp.wasm"

echo "==> done"
echo "    module      $(wc -c <"$DEST/netgrasp.wasm" | tr -d ' ') bytes"
echo "    migrations  $(find "$DEST/migrations" -name '*.sql' | wc -l | tr -d ' ')"
echo
echo "Install into a Trovato deployment with:"
echo "    PLUGINS_DIR=<trovato>/plugins:$ROOT/overlay/plugins \\"
echo "      trovato plugin install netgrasp"
echo "and serve it with TEMPLATES_DIR and STATIC_DIR extended the same way."
echo "scripts/serve-demo.sh does all of that against a Trovato checkout."
