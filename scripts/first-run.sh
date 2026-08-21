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

# The check the health check cannot make. /devices/online is a netgrasp gather
# behind a netgrasp permission, so a 200 here means the wizard is done, the
# plugin is enabled, its migrations ran, its gather is registered and the
# anonymous role can see it. Any one of those missing is a redirect or a 403.
code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/devices/online")"
if [ "$code" != "200" ]; then
    echo "error: $BASE/devices/online answered $code, not 200" >&2
    exit 1
fi

echo "==> ready: $BASE/ redirects to $BASE/devices/online"
