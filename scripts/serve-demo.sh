#!/usr/bin/env bash
# Install netgrasp onto a stock Trovato checkout and serve it.
#
# This is the install recipe, executable. It touches nothing inside the Trovato
# checkout: the three directories netgrasp contributes are APPENDED to Trovato's
# search paths, and a later entry wins on a name collision, so netgrasp's plugin,
# templates and assets take precedence without a single file being copied into
# Trovato or a line being patched there.
#
#   PLUGINS_DIR    <trovato>/plugins    : <netgrasp>/overlay/plugins
#   TEMPLATES_DIR  <trovato>/templates  : <netgrasp>/templates
#   STATIC_DIR     <trovato>/static     : <netgrasp>/static
#
# Usage:
#   scripts/serve-demo.sh <path-to-trovato-checkout> [--seed] [--bg]
#
#   --seed   also load scripts/seed-demo.sql, so the pages have rows. Needs psql.
#   --bg     background the server and wait for /health rather than blocking.
#
# It also walks Trovato's first-run wizard (scripts/first-run.sh), because a
# fresh install answers every page with a redirect to /install until somebody
# does. docker-compose.demo.yml runs the same script against the published
# kernel image, with no Trovato checkout at all.
#
# Environment:
#   DATABASE_URL   defaults to postgres://trovato:trovato@localhost:5432/netgrasp
#   REDIS_URL      defaults to redis://127.0.0.1:6379
#   PORT           defaults to 3101
#
# Trovato itself needs Postgres and Redis reachable; its own docker-compose.yml
# brings both up.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ $# -lt 1 ]; then
    sed -n '2,31p' "${BASH_SOURCE[0]}" >&2
    exit 1
fi

TROVATO="$(cd "$1" && pwd)"; shift
SEED=0
BG=0
for arg in "$@"; do
    case "$arg" in
        --seed) SEED=1 ;;
        --bg)   BG=1 ;;
        *) echo "error: unknown argument $arg" >&2; exit 1 ;;
    esac
done

if [ ! -f "$TROVATO/crates/kernel/Cargo.toml" ]; then
    echo "error: $TROVATO does not look like a Trovato checkout" >&2
    exit 1
fi

export DATABASE_URL="${DATABASE_URL:-postgres://trovato:trovato@localhost:5432/netgrasp}"
export REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}"
export PORT="${PORT:-3101}"
export RUST_LOG="${RUST_LOG:-info,sqlx=warn,tower_http=warn}"

export PLUGINS_DIR="$TROVATO/plugins:$ROOT/overlay/plugins"
export TEMPLATES_DIR="$TROVATO/templates:$ROOT/templates"
export STATIC_DIR="$TROVATO/static:$ROOT/static"

TROVATO_BIN="$TROVATO/target/release/trovato"

echo "==> netgrasp overlay"
"$ROOT/scripts/build-overlay.sh"

if [ ! -x "$TROVATO_BIN" ]; then
    echo "==> building the Trovato kernel (once)"
    (cd "$TROVATO" && cargo build --release --bin trovato)
fi

# Start the server in the background and wait for /health. The caller stops it.
boot_server() {
    "$TROVATO_BIN" serve >/dev/null 2>&1 &
    BOOT_PID=$!
    for _ in $(seq 1 60); do
        if curl -fsS "http://localhost:$PORT/health" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    echo "error: the server did not become healthy on port $PORT" >&2
    return 1
}

stop_server() {
    kill "$BOOT_PID" 2>/dev/null || true
    wait "$BOOT_PID" 2>/dev/null || true
}

# `trovato plugin install` runs the plugin's migrations, which need the kernel's
# own tables to exist. It applies the kernel's own migrations first, so nothing
# has to boot the server to get them: install can go straight at a brand new
# database.
echo "==> installing the netgrasp plugin"
"$TROVATO_BIN" plugin install netgrasp

# Now a startup with the plugin enabled, which is where two things happen that
# nothing else does:
#
#   * the kernel registers the plugin's Item types (ng_device, ng_person) from
#     tap_item_info. `item.type` is a foreign key onto `item_type`, so the seed's
#     ng_person INSERTs fail before this;
#   * Trovato's first-run wizard can be completed. Until it is, every path except
#     /health, /static and /install answers 303 to /install, so every netgrasp
#     page would be an installer form.
#
# Doing it in this order (install, boot, seed, serve) is why this is a script
# and not four lines in a README.
echo "==> first run"
boot_server
"$ROOT/scripts/first-run.sh" "http://localhost:$PORT"
stop_server

if [ "$SEED" = "1" ]; then
    echo "==> seeding demo rows"
    psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$ROOT/scripts/seed-demo.sql"
fi

echo "==> serving on http://localhost:$PORT"
echo "    /  redirects to /devices/online"
if [ "$BG" = "1" ]; then
    "$TROVATO_BIN" serve >/tmp/netgrasp-trovato.log 2>&1 &
    echo "    background pid $!, logs /tmp/netgrasp-trovato.log"
    for _ in $(seq 1 60); do
        if curl -fsS "http://localhost:$PORT/health" >/dev/null 2>&1; then
            echo "    ready"
            exit 0
        fi
        sleep 1
    done
    echo "server did not become healthy; see /tmp/netgrasp-trovato.log" >&2
    exit 1
fi

exec "$TROVATO_BIN" serve
