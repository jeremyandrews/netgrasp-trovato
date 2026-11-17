#!/bin/sh
# Check that a running netgrasp demo serves its front page, and that the front
# page is the overview rather than something that merely answered 200.
#
# WHY THIS EXISTS
# A 200 is a weak signal on a gather page. When a gather's template raises, the
# kernel does not fail the request: it falls back to its own dump of every column
# of the base table and returns that with a 200. So "the page loads" has to mean
# "the page is the one netgrasp's template renders", which is what the markers
# below check: the overview's own page element, its figures, and the seeded
# people by name.
#
# It checks the redirect too. The kernel renders only an item front page inline
# and redirects every other path, so `/` answering 307 to /overview is what
# "the front page is the overview" means.
#
# Run it against the Docker demo once `docker compose -f docker-compose.demo.yml
# up` has finished its first run, or against scripts/serve-demo.sh --seed. It
# expects the rows scripts/seed-demo.sql writes.
#
# Usage:
#   scripts/verify-demo.sh [base-url]        # default http://localhost:3101
#
# Written for /bin/sh, like scripts/first-run.sh, so it runs anywhere curl does.

set -eu

BASE="${1:-http://localhost:3101}"
PAGE="$(mktemp)"
trap 'rm -f "$PAGE"' EXIT

fail() {
    echo "error: $*" >&2
    exit 1
}

echo "==> verifying the demo at $BASE"

# The front page redirect.
redirect="$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "$BASE/")"
case "$redirect" in
    "307 "*/overview) echo "    / redirects to /overview" ;;
    *) fail "/ answered '$redirect', not a 307 to /overview" ;;
esac

# The overview itself, rendered by netgrasp's template.
code="$(curl -s -o "$PAGE" -w '%{http_code}' "$BASE/overview")"
[ "$code" = "200" ] || fail "/overview answered $code"

expect() {
    grep -q -- "$1" "$PAGE" || fail "/overview does not contain $2"
}
expect 'data-query-id="ng_overview"' "the overview's page element (did the template fall back to the column dump?)"
expect 'class="ng-stats"' "the four figures"
expect 'href="/events/security"' "the link to the security events"
expect 'Home now' "the people-home section"
expect 'Jamie' "the seeded person Jamie, who is home"
expect 'Aurora' "the seeded person Aurora, who is home"
expect 'New this week' "the new-device section"
expect '02:00:5e:00:00:0c' "the seeded device first seen 35 minutes ago"
expect 'class="ng-menu"' "a row menu on a new device"
echo "    /overview renders the overview, with the seeded people and devices"

# The three pages the overview links to.
# The site menu: exactly Home, Devices, People and Activity from netgrasp, in
# that order, and none of the kernel's own entries (Translate, Post, Blog, Terms,
# Media, a second Home). The menu is the `site-nav__link` anchors before the
# divider; Login, Admin, Profile and Logout come after it and are the theme's.
curl -s -o "$PAGE" "$BASE/overview"
menu="$(sed -n '/<nav class="site-nav"/,/site-nav__divider/p' "$PAGE" \
    | grep -o 'class="site-nav__link[^"]*"[^>]*>[^<]*' | sed 's/.*>//' | tr '\n' '|')"
[ "$menu" = "Home|Devices|People|Activity|" ] || fail "the site menu reads '$menu'"
echo "    the menu reads Home, Devices, People, Activity"

# Every page a person can reach, old paths included, renders netgrasp's chrome.
for path in /devices /devices/online /devices/all /devices/new /devices/todo \
    /devices/type /devices/location /people /people/home /people/movements \
    /activity /events /events/security /events/new-devices; do
    code="$(curl -s -o "$PAGE" -w '%{http_code}' "$BASE$path")"
    [ "$code" = "200" ] || fail "$path answered $code"
    grep -q 'class="ng-page ' "$PAGE" || fail "$path did not render netgrasp's page chrome"
    echo "    $path renders"
done

# The one page that went away answers with a real 301.
redirect="$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "$BASE/who-is-home")"
case "$redirect" in
    "301 "*/people/home) echo "    /who-is-home is a 301 to /people/home" ;;
    *) fail "/who-is-home answered '$redirect', not a 301 to /people/home" ;;
esac

# /devices is the Online tab: the online gather's page, not the full list.
curl -s -o "$PAGE" "$BASE/devices"
grep -q 'data-query-id="ng_device_online"' "$PAGE" || fail "/devices is not the online devices"
curl -s -o "$PAGE" "$BASE/devices/all"
grep -q 'data-query-id="ng_device_list"' "$PAGE" || fail "/devices/all is not every device"
echo "    /devices is what is online, /devices/all is everything"

echo "==> the demo's front page is the overview"
