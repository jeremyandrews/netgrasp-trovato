/*
 * Netgrasp gather pages: local timestamps and the auto-reload.
 *
 * Served from the netgrasp repository's own `static/` directory, appended to
 * Trovato's STATIC_DIR search path. Loaded with `defer` by
 * templates/gather/netgrasp/page.html, so the DOM is parsed by the time it runs
 * and there is no DOMContentLoaded dance.
 *
 * Scoped to `.ng-page` deliberately. A reload timer in the site's base template
 * would reload the admin screens and every editing form on the host site; this
 * file only ever reads and rewrites inside a netgrasp page element, and only
 * arms a timer when it finds one.
 */
(function () {
    "use strict";

    var page = document.querySelector(".ng-page");
    if (!page) {
        return;
    }

    localiseTimestamps(page);
    startAutoReload(page);

    /*
     * Timestamps in the viewer's own timezone.
     *
     * The daemon stores timestamptz and the gather hands the template the
     * `_epoch` twin; Tera's `date` filter renders that as UTC, because the site
     * has no timezone setting to render it as anything else. Rather than invent
     * one, each cell carries its epoch and the browser — which definitively
     * knows where it is — rewrites it. The server-rendered UTC stays as the
     * no-JavaScript fallback, and the `title` keeps it reachable either way.
     */
    function localiseTimestamps(root) {
        var stamps = root.querySelectorAll("[data-ng-epoch]");
        for (var i = 0; i < stamps.length; i++) {
            var epoch = parseInt(stamps[i].getAttribute("data-ng-epoch"), 10);
            if (!isFinite(epoch)) {
                continue;
            }
            var when = new Date(epoch * 1000);
            stamps[i].title = stamps[i].textContent.trim() + " UTC";
            stamps[i].textContent = when.getFullYear()
                + "-" + pad(when.getMonth() + 1)
                + "-" + pad(when.getDate())
                + " " + pad(when.getHours())
                + ":" + pad(when.getMinutes())
                + (stamps[i].hasAttribute("data-ng-seconds")
                    ? ":" + pad(when.getSeconds())
                    : "");
        }
    }

    function pad(n) {
        return String(n).padStart(2, "0");
    }

    /*
     * Arm the reload, and say so on the page.
     *
     * A whole-page `location.reload()` rather than a fetch-and-replace: the page
     * number and every route facet live in the URL, so reloading keeps the
     * reader on the page of the pager, and the device type or owner, they were
     * looking at.
     *
     * At an interval of 0 no timer is created at all — not one created and
     * cleared — and the label stays hidden, so a page somebody switched
     * reloading off on says nothing about reloading.
     */
    function startAutoReload(root) {
        var seconds = resolveInterval(root);
        var label = root.querySelector("[data-ng-refresh-label]");

        if (seconds <= 0) {
            return;
        }

        if (label) {
            label.textContent = "Reloading every " + seconds + "s";
            label.hidden = false;
        }

        window.setTimeout(function () {
            window.location.reload();
        }, seconds * 1000);
    }

    /*
     * `?refresh=<seconds>` first, then the template's own default.
     *
     * `0` is a real answer meaning "do not reload", not a missing one, which is
     * why this returns a number and the caller compares it rather than checking
     * for null. Anything that is not a non-negative integer is ignored in favour
     * of the next source down: a typo in a query string should leave the page
     * behaving as configured, not silently stop a wall display from updating.
     */
    function resolveInterval(root) {
        var override = parseSeconds(
            new URLSearchParams(window.location.search).get("refresh")
        );
        if (override !== null) {
            return override;
        }

        var configured = parseSeconds(root.getAttribute("data-ng-refresh"));
        return configured === null ? 0 : configured;
    }

    function parseSeconds(raw) {
        if (raw === null || !/^\d+$/.test(raw.trim())) {
            return null;
        }
        var n = parseInt(raw, 10);
        return isFinite(n) ? n : null;
    }
})();
