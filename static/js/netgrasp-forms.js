/*
 * Netgrasp forms: a search box over the owner select.
 *
 * Loaded with `defer` by the owner form and the bulk form (plugins/netgrasp/
 * src/forms.rs, FORMS_SCRIPT). Progressive only: the select works and posts
 * the same value with scripting off, and the box this wires is `hidden` in the
 * markup, so without this file there is no box that does nothing.
 *
 * Typing narrows the select to the people whose name contains what was typed,
 * ignoring case, and selects the first match so Enter assigns to it. "nobody"
 * stays visible: it is a choice, not a person.
 */
(function () {
    "use strict";

    var boxes = document.querySelectorAll("input[data-ng-filter]");
    Array.prototype.forEach.call(boxes, function (box) {
        var select = document.getElementById(box.getAttribute("data-ng-filter"));
        if (!select) {
            return;
        }
        box.hidden = false;

        box.addEventListener("input", function () {
            var needle = box.value.trim().toLowerCase();
            var first = null;
            Array.prototype.forEach.call(select.options, function (option) {
                if (option.value === "") {
                    option.hidden = false;
                    return;
                }
                var match = needle === "" || option.text.toLowerCase().indexOf(needle) !== -1;
                option.hidden = !match;
                if (match && first === null) {
                    first = option;
                }
            });
            if (needle !== "" && first !== null) {
                select.value = first.value;
            }
        });
    });
})();
