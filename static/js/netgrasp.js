/*
 * Netgrasp gather pages: local timestamps, the row menu and the auto-reload.
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
    wireRowMenus(page);
    startAutoReload(page);

    /*
     * Right-click opens the row's menu.
     *
     * The menu is a <details> element that already works: clicking its summary
     * opens it, Enter and Space open it from the keyboard, and every entry in
     * it is an ordinary link. Nothing below adds an action, a destination or a
     * capability that is not reachable with scripting switched off — it moves
     * the same menu under the pointer, which is the one thing a right-click can
     * offer that a click on the button cannot.
     *
     * Touch has no contextmenu event worth binding, which is why the button is
     * visible rather than a hover affordance: on a touch screen the button IS
     * the interface, and this function never runs.
     */
    function wireRowMenus(root) {
        root.addEventListener("contextmenu", function (event) {
            var row = event.target.closest("tr, .ng-person");
            if (!row) {
                return;
            }
            var menu = row.querySelector("details.ng-menu");
            if (!menu) {
                // A row with no menu — an event with no device — keeps the
                // browser's own context menu. Suppressing it there would take
                // something away and give nothing back.
                return;
            }

            event.preventDefault();
            closeMenus(root, menu);
            menu.open = true;

            // Focus the summary rather than the first entry: that is where a
            // <details> puts focus when opened by click or by keyboard, so the
            // three ways in agree, and Escape below has somewhere to return to.
            var summary = menu.querySelector("summary");
            if (summary) {
                summary.focus();
            }
        });

        /*
         * Escape closes the menu the focus is in and puts focus back on its
         * button, which is where it came from. Without this a keyboard user who
         * opens a menu and changes their mind has no way out but Tab.
         */
        root.addEventListener("keydown", function (event) {
            if (event.key !== "Escape") {
                return;
            }
            var open = event.target.closest("details.ng-menu[open]");
            if (!open) {
                return;
            }
            open.open = false;
            var summary = open.querySelector("summary");
            if (summary) {
                summary.focus();
            }
        });
    }

    /* Close every open row menu but `keep`. */
    function closeMenus(root, keep) {
        var open = root.querySelectorAll("details.ng-menu[open]");
        for (var i = 0; i < open.length; i++) {
            if (open[i] !== keep) {
                open[i].open = false;
            }
        }
    }

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
     *
     * The reload DEFERS rather than fires while somebody is using the page.
     * A whole-page reload with a menu open closes the menu, and with a form
     * half filled in throws the typing away — on a ten second timer, which is
     * less time than it takes to read the entries. So when the moment arrives
     * and the page is in use, the timer is simply armed again: the reader keeps
     * what they were doing, and the page refreshes as soon as they are done.
     *
     * Deferring rather than cancelling is the whole point. Cancelling on first
     * interaction would leave a wall display frozen after somebody walked past
     * and opened a menu, which is the failure this feature is most likely to
     * have and least likely to be noticed.
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

        var arm = function () {
            window.setTimeout(function () {
                if (inUse(root)) {
                    arm();
                    return;
                }
                window.location.reload();
            }, seconds * 1000);
        };
        arm();
    }

    /*
     * Whether the reader is in the middle of something a reload would destroy.
     *
     * Two cases, and the second is not implied by the first: a menu standing
     * open, and focus sitting in a form control — which on a netgrasp page is
     * the pager or a plugin-served form embedded in one, and in general is
     * anything with a value somebody typed.
     */
    function inUse(root) {
        if (root.querySelector("details.ng-menu[open]")) {
            return true;
        }
        var focused = document.activeElement;
        if (!focused || !root.contains(focused)) {
            return false;
        }
        return !!focused.closest("details.ng-menu, input, select, textarea");
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
