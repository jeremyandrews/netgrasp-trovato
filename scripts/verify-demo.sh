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
JAR="$(mktemp)"
trap 'rm -f "$PAGE" "$JAR"' EXIT

# The demo's administrator, as docker-compose.demo.yml and scripts/first-run.sh
# create it. Device data is for a viewer holding `view netgrasp devices` (013),
# so everything after the anonymous check below is read signed in.
ADMIN_USER="${NETGRASP_ADMIN_USER:-admin}"
ADMIN_PASSWORD="${NETGRASP_ADMIN_PASSWORD:-netgrasp-demo-password}"

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
# Anonymous: the page answers, and carries no device data at all. A gather has
# no permission of its own, so before 013 this returned every MAC on the network.
code="$(curl -s -o "$PAGE" -w '%{http_code}' "$BASE/devices/online")"
[ "$code" = "200" ] || fail "/devices/online answered $code to an anonymous visitor"
if grep -Eiq '([0-9a-f]{2}:){5}[0-9a-f]{2}' "$PAGE"; then
    fail "an anonymous visitor can read MAC addresses on /devices/online"
fi
grep -q 'Sign in to see the devices on this network.' "$PAGE" \
    || fail "/devices/online does not ask an anonymous visitor to sign in"
echo "    an anonymous visitor sees no MAC address, and is asked to sign in"

# Sign in, through the kernel's own form and its token.
token="$(curl -s -c "$JAR" "$BASE/user/login" \
    | sed -n 's/.*name="_token" value="\([^"]*\)".*/\1/p' | head -n 1)"
[ -n "$token" ] || fail "the login form carries no token"
code="$(curl -s -b "$JAR" -c "$JAR" -o /dev/null -w '%{http_code}' \
    --data-urlencode "_token=$token" \
    --data-urlencode "username=$ADMIN_USER" \
    --data-urlencode "password=$ADMIN_PASSWORD" \
    "$BASE/user/login")"
case "$code" in
    30*) echo "    signed in as $ADMIN_USER" ;;
    *) fail "signing in as $ADMIN_USER answered $code" ;;
esac

code="$(curl -s -b "$JAR" -o "$PAGE" -w '%{http_code}' "$BASE/overview")"
[ "$code" = "200" ] || fail "/overview answered $code"

expect() {
    grep -q -- "$1" "$PAGE" || fail "/overview does not contain $2"
}
expect 'data-query-id="ng_overview"' "the overview's page element (did the template fall back to the column dump?)"
expect 'Needs attention' "the needs-attention section"
expect 'href="/events/security"' "the link to the security events"
expect 'Home now' "the people-home section"
expect 'Jamie' "the seeded person Jamie, who is home"
expect 'Aurora' "the seeded person Aurora, who is not tracked"
expect 'Not tracked:' "the line naming the people nobody is tracking"
expect '02:00:5e:00:00:0c' "the seeded device first seen 35 minutes ago"
expect 'class="ng-menu"' "a row menu on a new device"
expect 'more devices have no owner' "the line counting the devices left to sort"
# Home shows no raw security total, and at most six devices to sort.
if grep -q 'security events\?, ' "$PAGE" || grep -q 'class="ng-stats"' "$PAGE"; then
    fail "/overview still shows the raw security total"
fi
attention="$(grep -c 'class="ng-assign"' "$PAGE" || true)"
[ "$attention" -ge 1 ] && [ "$attention" -le 6 ] \
    || fail "Needs attention lists $attention devices, not one to six"
echo "    /overview renders the overview, with the seeded people and devices"

# The three pages the overview links to.
# The site menu: exactly Home, Devices, People, Alerts and Activity from netgrasp, in
# that order, and none of the kernel's own entries (Translate, Post, Blog, Terms,
# Media, a second Home). The menu is the `site-nav__link` anchors before the
# divider; Login, Admin, Profile and Logout come after it and are the theme's.
curl -s -b "$JAR" -o "$PAGE" "$BASE/overview"
menu="$(sed -n '/<nav class="site-nav"/,/site-nav__divider/p' "$PAGE" \
    | grep -o 'class="site-nav__link[^"]*"[^>]*>[^<]*' | sed 's/.*>//' | tr '\n' '|')"
[ "$menu" = "Home|Devices|People|Alerts|Activity|" ] || fail "the site menu reads '$menu'"
echo "    the menu reads Home, Devices, People, Alerts, Activity"

# Every page a person can reach, old paths included, renders netgrasp's chrome.
for path in /devices /devices/online /devices/all /devices/new /devices/todo \
    /devices/hidden /devices/type /devices/location /people /people/home /people/movements \
    /activity /events /events/security /events/new-devices; do
    code="$(curl -s -b "$JAR" -o "$PAGE" -w '%{http_code}' "$BASE$path")"
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
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices"
grep -q 'data-query-id="ng_device_online"' "$PAGE" || fail "/devices is not the online devices"
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices/all"
grep -q 'data-query-id="ng_device_list"' "$PAGE" || fail "/devices/all is not every device"
echo "    /devices is what is online, /devices/all is everything"

# The search box: "motivo" finds the seeded Raspberry Pi and nothing else, and a
# blank search is the whole list. Counted by row, since each device row is one
# `<tr data-id=`.
rows() { grep -c '<tr data-id=' "$PAGE" || true; }
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices/all?search=motivo"
[ "$(rows)" = "1" ] || fail "searching motivo found $(rows) rows, not 1"
grep -q '02:00:5e:00:00:10' "$PAGE" || fail "searching motivo did not find motivo"
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices/all?search="
blank="$(rows)"
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices/all"
[ "$blank" = "$(rows)" ] || fail "a blank search found $blank rows, not every device"
echo "    searching motivo finds motivo alone, and a blank search finds all $blank"
curl -s -b "$JAR" -o "$PAGE" "$BASE/devices/hidden"
[ "$(rows)" -gt 0 ] || fail "/devices/hidden has no rows"
echo "    /devices/hidden lists the hidden devices"

# The People forms (U2). Read-only checks: each page renders signed in, carries
# the kernel's token, and asks what docs/UI.md says it asks. Nothing is posted,
# so the demo's rows are left as the seed wrote them.
person="$(curl -s -b "$JAR" "$BASE/people" \
    | grep -o '/netgrasp/person/merge?back=[^"]*target=[0-9a-f-]*' | head -n 1 \
    | sed 's/.*target=//')"
[ -n "$person" ] || fail "/people offers no Merge into... entry"
form() {
    code="$(curl -s -b "$JAR" -o "$PAGE" -w '%{http_code}' "$BASE$1")"
    [ "$code" = "200" ] || fail "$1 answered $code"
    grep -q 'name="_token"' "$PAGE" || [ "${3:-}" = "no-token" ] || fail "$1 carries no form token"
    grep -Eq -- "$2" "$PAGE" || fail "$1 does not contain $2"
    echo "    $1 renders"
}
form /people/add 'Alert me when they get home'
# Presence (U18): the three states on /people/home, Aurora never away, and the
# two forms that set who is tracked and which devices count.
curl -s -b "$JAR" -o "$PAGE" "$BASE/people/home"
for state in 'Home since' 'Away since' 'Not tracked'; do
    grep -q "$state" "$PAGE" || fail "/people/home does not say $state"
done
echo "    /people/home lists Home, Away and Not tracked"
form "/netgrasp/device/presence?target=02:00:5e:00:00:04" 'Counts: chosen for this device'
form "/netgrasp/device/presence?target=02:00:5e:00:00:02" 'Counts: phone'
form "/netgrasp/person/presence?target=$person" 'Track whether they are home'
form /devices/assign 'name="device"'
form "/netgrasp/device/owner?target=02:00:5e:00:00:0c" 'name="new_person"'
# The chooser, or, once only one person is left, the sentence saying so.
form "/netgrasp/person/merge?target=$person" 'name="into"|the only person here' no-token
form "/netgrasp/person/delete?target=$person" 'Delete '
code="$(curl -s -b "$JAR" -o "$PAGE" -w '%{http_code}' "$BASE/item/$person")"
[ "$code" = "200" ] || fail "the person page answered $code"
grep -q 'ng-person-page' "$PAGE" || fail "the person page has no netgrasp section"
echo "    the person page renders"
# Anonymous: refused by the kernel's route gate before the plugin runs, which
# answers 401 to a visitor with no session (a signed-in user without the
# permission gets 403, from the kernel or from the plugin's own check).
for path in /people/add /devices/assign "/netgrasp/person/delete?target=$person"; do
    code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE$path")"
    case "$code" in
        401|403) ;;
        *) fail "$path answered $code to an anonymous visitor" ;;
    esac
done
echo "    an anonymous visitor is refused the People forms ($code)"

# Alerts (U3). Read-only, like the People checks: the page says where alerts go
# (the seed's status row), every rule as a sentence, and what was sent, and its
# two forms render. Send test is not pressed: nothing here answers it.
form /alerts 'Tell me when Jamie gets home\.'
grep -q 'Alerts go to ntfy\.' "$PAGE" || fail "/alerts does not say where alerts go"
grep -q 'Send a test alert' "$PAGE" || fail "/alerts has no Send test button"
grep -q 'alert when they come back online\.' "$PAGE" || fail "/alerts does not count the devices"
grep -q 'Aurora is home: ' "$PAGE" || fail "/alerts does not list what was sent"
form /alerts/add 'name="when"'
form /alerts/devices 'name="listed"'
for path in /alerts /alerts/add /alerts/devices; do
    code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE$path")"
    case "$code" in
        401|403) ;;
        *) fail "$path answered $code to an anonymous visitor" ;;
    esac
done
echo "    an anonymous visitor is refused the Alerts pages ($code)"

echo "==> the demo's front page is the overview"
