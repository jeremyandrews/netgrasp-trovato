#!/usr/bin/env bash
# Tests for the anonymous check at the top of scripts/verify-demo.sh, against a
# stand-in site that answers each case's way. Needs bash, curl and python3.
#
# A site that refuses a visitor everywhere, by a redirect to the login page or
# by a 403 page carrying the login form, gets past the check (and then fails at
# the sign-in, which the stand-in does not offer); a site that lists a seeded
# device by name (to a script, to a browser only, or on a single gather's
# endpoint), answers with a server error or a 429, or refuses with a 403 that
# has no login form in it, fails the check itself.
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
        html = "text/html" in self.headers.get("Accept", "")
        if path == "/user/login":
            return self.answer(200, "<form></form>")
        # A refusal that is clean to a script and leaks to a browser.
        if MODE == "htmlleak" and html:
            return self.answer(
                403, '<form action="/user/login" method="post"></form><p>Jamie\'s laptop</p>'
            )
        # A leak on one gather's endpoint and nowhere else.
        if MODE == "gatherleak" and path == "/api/v1/query/ng_device_list/execute":
            return self.answer(200, '{"rows":[{"title":"Hallway smart plug"}]}')
        if MODE == "ratelimited" and path == "/api/v1/query/ng_event_log/execute":
            return self.answer(429, "Too Many Requests")
        if path == LEAKY and MODE == "title":
            return self.answer(200, '{"data":[{"title":"Jamie\'s laptop"}]}')
        if path == LEAKY and MODE == "error":
            return self.answer(500, "Internal Server Error")
        if path.startswith("/api/"):
            return self.answer(401, '{"error":"unauthorized"}')
        # Kernel B156: a page that needs a sign in is a 403 with the login
        # form on it, rather than a redirect to /user/login.
        if MODE == "login403":
            return self.answer(403, '<form action="/user/login" method="post"></form>')
        # The login form hands the asked-for address back, query and all.
        if MODE == "echo403":
            return self.answer(
                403,
                '<form action="/user/login" method="post">'
                f'<input type="hidden" name="destination" value="{self.path}"></form>',
            )
        # The same, with a seeded name in the page outside the destination.
        if MODE == "echoleak":
            return self.answer(
                403,
                '<form action="/user/login" method="post">'
                f'<input type="hidden" name="destination" value="{self.path}"></form>'
                "<p>Aurora</p>",
            )
        if MODE == "bare403":
            return self.answer(403, "Access denied")
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
expect login403 "the login form carries no token"
expect bare403 "/devices/online answered 403 to an anonymous visitor with no login form in it"
expect echo403 "the login form carries no token"
expect echoleak "an anonymous visitor can read device data on / (403 , Accept: */*)"
expect htmlleak "an anonymous visitor can read device data on / (403 , Accept: text/html)"
expect gatherleak "an anonymous visitor can read device data on /api/v1/query/ng_device_list/execute"
expect ratelimited "/api/v1/query/ng_event_log/execute answered '429 ' to an anonymous visitor"

[ "$failures" -eq 0 ] || {
    echo "${failures} case(s) failed" >&2
    exit 1
}
echo "all cases passed"
