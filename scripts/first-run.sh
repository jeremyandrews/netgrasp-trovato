#!/bin/sh
# Walk a fresh Trovato through its first-run wizard, without a human.
#
# WHY THIS EXISTS
# The kernel gates the entire site on one row: until `site_config` carries
# `installed`, its `check_installation` middleware answers every path except
# /health, /static and /install with a 303 to /install. A netgrasp deployment can
# therefore be completely correct (plugin installed, migrations applied, gathers
# registered, demo rows loaded, /health answering) and still show a visitor an
# installer form and not one netgrasp page.
#
# The wizard is two form POSTs and no CSRF token, so it can be completed from a
# script. That is all this does, plus the check that matters afterwards: a 303 to
# /install is a perfectly healthy response as far as a health check is concerned,
# so the only way to know the site is really serving is to ask for a page that
# only exists when netgrasp is installed and the site is out of setup.
#
# Idempotent. Once the site is installed both POSTs are redirected to / without
# touching anything, so rerunning this is a no-op.
#
# Nothing here patches or configures Trovato: the admin account and the site name
# are the two things its own installer asks any operator for.
#
# Usage:
#   scripts/first-run.sh [base-url]        # default http://localhost:3101
#
# Environment:
#   NETGRASP_ADMIN_USER      default "admin"
#   NETGRASP_ADMIN_EMAIL     default "admin@example.invalid"
#   NETGRASP_ADMIN_PASSWORD  default "netgrasp-demo-password" (min 12 characters)
#   NETGRASP_SITE_NAME       default "Netgrasp"
#
# Written for /bin/sh rather than bash: it runs inside the demo's curl container,
# which has neither bash nor anything else.

set -eu

BASE="${1:-http://localhost:3101}"
USER_NAME="${NETGRASP_ADMIN_USER:-admin}"
USER_MAIL="${NETGRASP_ADMIN_EMAIL:-admin@example.invalid}"
USER_PASS="${NETGRASP_ADMIN_PASSWORD:-netgrasp-demo-password}"
SITE_NAME="${NETGRASP_SITE_NAME:-Netgrasp}"

echo "==> first run against $BASE"

# Step 2 of the wizard: the administrator account. Step 1 is a requirements page
# with nothing to submit. The password minimum is twelve characters.
curl -fsS -o /dev/null \
    --data-urlencode "username=$USER_NAME" \
    --data-urlencode "email=$USER_MAIL" \
    --data-urlencode "password=$USER_PASS" \
    --data-urlencode "password_confirm=$USER_PASS" \
    "$BASE/install/admin"
echo "    admin account: $USER_NAME"

# Step 3: the site name, and the POST that writes `installed` and lifts the
# redirect off every other route.
curl -fsS -o /dev/null \
    --data-urlencode "site_name=$SITE_NAME" \
    "$BASE/install/site"
echo "    site name: $SITE_NAME"

# The check the health check cannot make. Until the wizard is done every page
# redirects to /install; after it, /overview is a netgrasp gather on a private
# site (025), so a visitor with no session is asked to sign in. Since kernel
# B156 that is a 403 page carrying the login form, for a browser (asking for
# HTML); before it, a redirect to /user/login. Either is accepted, a 403 only
# with the login form in it. A redirect anywhere else, or any other answer,
# means the wizard, the plugin or its migrations did not finish.
login_page="$(mktemp)"
answer="$(curl -s -o "$login_page" -H 'Accept: text/html' \
    -w '%{http_code} %{redirect_url}' "$BASE/overview")"
case "$answer" in
    30[0-9]" "*/user/login*) ;;
    "403 ")
        grep -q 'action="/user/login' "$login_page" || {
            rm -f "$login_page"
            echo "error: $BASE/overview answered 403 with no login form in it" >&2
            exit 1
        }
        ;;
    *)
        rm -f "$login_page"
        echo "error: $BASE/overview answered '$answer', not the login page" >&2
        exit 1
        ;;
esac
rm -f "$login_page"

echo "==> ready: $BASE/ is the overview, after signing in"
