#!/usr/bin/env bash
# Tests for the anonymous check at the top of scripts/verify-demo.sh, against a
# stand-in site that answers each case's way. Needs bash, curl and python3.
#
# A site that refuses a visitor everywhere gets past the check (and then fails
# at the sign-in, which the stand-in does not offer); a site that lists a seeded
# device by name, or answers with a server error, fails the check itself.
#
# Usage:
#   scripts/test-verify-demo-anonymous.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
server_pid=""
cleanup() {
    [ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/stub"
cat > "$work/stub/site.py" <<'PY'
import http.server, os, sys

MODE = os.environ["MODE"]
LEAKY = "/api/v1/items/ng_device"

class Site(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, code, body, location=None):
        data = body.encode()
        self.send_response(code)
        if location:
            self.send_header("Location", location)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/user/login":
            return self.answer(200, "<form></form>")
        if path == LEAKY and MODE == "title":
            return self.answer(200, '{"data":[{"title":"Jamie\'s laptop"}]}')
        if path == LEAKY and MODE == "error":
            return self.answer(500, "Internal Server Error")
        if path.startswith("/api/"):
            return self.answer(401, '{"error":"unauthorized"}')
        return self.answer(303, "", "/user/login")

    do_POST = do_GET

http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Site).serve_forever()
PY

port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"

failures=0
# expect MODE "text the failure must contain"
expect() {
    local mode="$1" wanted="$2"
    MODE="$mode" python3 -I "$work/stub/site.py" "$port" &
    server_pid=$!
    for _ in $(seq 50); do
        curl -s -o /dev/null "http://127.0.0.1:$port/user/login" && break
        sleep 0.1
    done
    local out
    out="$("$here/verify-demo.sh" "http://127.0.0.1:$port" 2>&1 || true)"
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
    if printf '%s\n' "$out" | grep -Fq -- "$wanted"; then
        echo "ok   ${mode}: ${wanted}"
    else
        echo "FAIL ${mode}: wanted '${wanted}', got:"
        printf '%s\n' "$out" | sed 's/^/     /'
        failures=$((failures + 1))
    fi
}

expect clean "the login form carries no token"
expect title "an anonymous visitor can read device data on /api/v1/items/ng_device"
expect error "/api/v1/items/ng_device answered '500 ' to an anonymous visitor"

[ "$failures" -eq 0 ] || {
    echo "${failures} case(s) failed" >&2
    exit 1
}
echo "all cases passed"
