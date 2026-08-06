//! The device page fragment: what `tap_item_view` appends to an `ng_device`
//! Item's page.
//!
//! # Why a plugin renders its own HTML here
//!
//! The device's presence, location and address timelines live in daemon tables,
//! not in the Item's fields, so the kernel's generic field rendering has nothing
//! to render. A plugin cannot ship a Tera template (the theme engine loads from
//! the site's `templates/` directory and has no plugin path) and cannot serve a
//! request (`G-NO-PLUGIN-HTTP`). `tap_item_view`'s return value is appended to
//! the item page's children, so building the fragment here is the surface the
//! frozen contract offers.
//!
//! # Why the markup uses single-quoted attributes
//!
//! A tap's return value is JSON-serialized by the `#[plugin_tap]` macro
//! (`crates/plugin-sdk-macros/src/lib.rs`, `serde_json::to_string(&result)`), and
//! the item route appends that serialized form to the page **without decoding
//! it**. So a `String`-returning view tap reaches the page as a JSON string
//! literal: wrapped in quotes, with every inner `"` turned into `\"`.
//! Double-quoted attributes would arrive as `class=\` followed by a stray
//! `ng-device\`, which mangles the markup outright.
//!
//! Single-quoted attributes, and an [`escape`] that emits `&quot;`/`&#x27;`
//! rather than raw quotes, mean this fragment contains **no character serde
//! escapes** — so the only damage the round trip does is the pair of quotes
//! wrapping the whole fragment. Same defect and same mitigation as Argus M3's
//! story fragment: `G-VIEW-OUTPUT-JSON-ENCODED`.
//!
//! # Escaping is this module's job
//!
//! That append is **verbatim** — the kernel neither escapes nor sanitizes a
//! plugin's view output, and the SDK ships no escaping helper (`G-SDK-NO-ESCAPE`).
//! Every value below goes through [`escape`], and for Netgrasp that matters more
//! than it did for Argus: a hostname is DHCP option 12, which is whatever the
//! client on the LAN says it is, and an access-point name and a vendor string are
//! similarly unvalidated third-party text. A device page is the one place in a
//! Trovato install where an unauthenticated device on the network gets to put
//! characters in front of an admin.
//!
//! The rendering is a pure function of its inputs, so it is unit-tested natively
//! with no host in sight.

use netgrasp_core::model::Span;
use netgrasp_core::timeline::{self, TimelineRow};

use crate::sync_host::{DeviceHistory, DeviceState};

/// Escape text for interpolation into HTML.
///
/// Covers the five characters that matter in element content and in an attribute
/// value, and additionally removes the two things HTML does not care about but
/// **JSON does**: a literal backslash becomes `&#x5C;` and any control character
/// becomes a space. Neither is an HTML concern; both are here because the kernel
/// appends this fragment's JSON-serialized form to the page undecoded
/// (`G-VIEW-OUTPUT-JSON-ENCODED`), so a `\` or a newline in a hostname would
/// reach an admin as a literal `\\` or `\n`.
#[must_use]
pub fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            '\\' => out.push_str("&#x5C;"),
            c if c.is_control() => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the whole fragment.
///
/// `state` is `None` when the sync has not linked this Item to a daemon row —
/// either the plugin is installed with no daemon running, or an admin created a
/// device Item by hand. The page says so rather than rendering empty timelines
/// that would read as "this device has never been seen".
#[must_use]
pub fn render(
    state: Option<&DeviceState>,
    history: &DeviceHistory,
    owner_name: Option<&str>,
    now: i64,
) -> String {
    let Some(state) = state else {
        return "<section class='ng-device ng-device--unlinked'>\
                <p>No daemon record is linked to this device yet. \
                It will appear here once the netgrasp daemon reports it.</p>\
                </section>"
            .to_string();
    };

    let mut out = String::with_capacity(4096);
    out.push_str("<section class='ng-device'>");
    render_identity(&mut out, state, owner_name, now);
    render_timeline(
        &mut out,
        "presence",
        "Presence",
        &history.presence,
        now,
        "Never seen online.",
        false,
    );
    render_timeline(
        &mut out,
        "location",
        "Location history",
        &history.locations,
        now,
        "No location history.",
        true,
    );
    render_timeline(
        &mut out,
        "address",
        "Address history",
        &history.addresses,
        now,
        "No addresses recorded.",
        true,
    );
    render_events_link(&mut out, state.id);
    out.push_str("</section>");
    out
}

/// The identity and live-state block.
fn render_identity(out: &mut String, state: &DeviceState, owner_name: Option<&str>, now: i64) {
    let status = state.state.as_deref().unwrap_or("unknown");
    // The status word is part of a class name, so it is constrained to a known
    // set rather than escaped: a daemon that wrote a state of `x' onclick=` would
    // otherwise put an attribute on the page even after escaping, because the
    // escaped form is still a valid class-name-ish string in a single-quoted
    // attribute. An unrecognised state renders as `unknown`.
    let status_class = match status {
        "online" | "offline" | "new" => status,
        _ => "unknown",
    };

    out.push_str("<dl class='ng-device__identity'>");
    row(
        out,
        "Status",
        &format!("{status_class} ({})", escape(status)),
    );
    row(out, "MAC", &escape(&state.mac));
    if let Some(v) = present(state.hostname.as_deref()) {
        row(out, "Hostname", &escape(v));
    }
    if let Some(v) = present(state.vendor.as_deref()) {
        row(out, "Vendor", &escape(v));
    }
    if let Some(v) = present(state.device_type.as_deref()) {
        row(out, "Type", &escape(v));
    }
    if let Some(v) = present(state.os_family.as_deref()) {
        row(out, "OS", &escape(v));
    }
    if let Some(v) = present(state.last_ip.as_deref()) {
        row(out, "Last IP", &escape(v));
    }
    if let Some(v) = present(state.current_location.as_deref()) {
        row(out, "Location", &escape(v));
    }
    if let Some(v) = present(owner_name) {
        row(out, "Owner", &escape(v));
    }
    row(
        out,
        "First seen",
        &escape(&timeline::humanize_ago(state.first_seen, now)),
    );
    row(
        out,
        "Last seen",
        &escape(&timeline::humanize_ago(state.last_seen, now)),
    );
    out.push_str("</dl>");
}

/// One definition-list row. Both halves are escaped by the caller.
fn row(out: &mut String, label: &str, value: &str) {
    out.push_str("<dt>");
    out.push_str(&escape(label));
    out.push_str("</dt><dd>");
    out.push_str(value);
    out.push_str("</dd>");
}

/// One timeline table.
///
/// `labelled` controls whether the span's label gets a column: a presence
/// session has no label, so a "What" column of empty cells would be noise.
fn render_timeline(
    out: &mut String,
    slug: &str,
    heading: &str,
    spans: &[Span],
    now: i64,
    empty_text: &str,
    labelled: bool,
) {
    out.push_str("<section class='ng-device__timeline ng-device__timeline--");
    out.push_str(&escape(slug));
    out.push_str("'><h3>");
    out.push_str(&escape(heading));
    out.push_str("</h3>");

    let rows = timeline::build(spans, now);
    if rows.is_empty() {
        out.push_str("<p class='ng-device__empty'>");
        out.push_str(&escape(empty_text));
        out.push_str("</p></section>");
        return;
    }

    let summary = timeline::summarize(spans, now);
    out.push_str("<p class='ng-device__summary'>");
    out.push_str(&escape(&format!(
        "{} sessions, {} total, longest {}",
        summary.sessions,
        timeline::humanize_duration(summary.total_secs),
        timeline::humanize_duration(summary.longest_secs)
    )));
    out.push_str("</p>");

    out.push_str("<table class='ng-device__spans'><thead><tr>");
    if labelled {
        out.push_str("<th>What</th>");
    }
    out.push_str("<th>Started</th><th>Duration</th></tr></thead><tbody>");
    for r in &rows {
        render_span_row(out, r, labelled, now);
    }
    out.push_str("</tbody></table>");

    if timeline::is_truncated(spans) {
        // Never silently truncate: a list that stops at 24 without saying so
        // reads as "this is everything".
        out.push_str("<p class='ng-device__truncated'>");
        out.push_str(&escape(&format!(
            "Showing the {} most recent of {} recorded.",
            timeline::MAX_SPANS,
            spans.len()
        )));
        out.push_str("</p>");
    }
    out.push_str("</section>");
}

/// One row of a timeline table.
fn render_span_row(out: &mut String, r: &TimelineRow, labelled: bool, now: i64) {
    out.push_str(if r.open {
        "<tr class='ng-device__span ng-device__span--open'>"
    } else {
        "<tr class='ng-device__span'>"
    });
    if labelled {
        out.push_str("<td>");
        out.push_str(&escape(&r.label));
        out.push_str("</td>");
    }
    out.push_str("<td>");
    out.push_str(&escape(&timeline::humanize_ago(Some(r.start), now)));
    out.push_str("</td><td>");
    out.push_str(&escape(&timeline::humanize_duration(r.duration_secs)));
    if r.open {
        out.push_str(" (ongoing)");
    }
    out.push_str("</td></tr>");
}

/// The link to this device's event log.
///
/// The href is built from the device row's id, which is an `i64` — a type with
/// no character in it that HTML or JSON could care about, so this is the one
/// interpolation in the module that does not go through [`escape`]. It is not
/// escaped because it *cannot* carry a hostile character, not because it is
/// trusted; the day `state.id` becomes a string again, that reasoning stops
/// holding and the compiler says so.
fn render_events_link(out: &mut String, device_id: i64) {
    out.push_str("<p class='ng-device__events'><a href='/events/device?device=");
    out.push_str(&device_id.to_string());
    out.push_str("'>Event history for this device</a></p>");
}

/// A value if it is present and not blank.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;
    const DEVICE_ID: i64 = 4_444;

    fn state() -> DeviceState {
        DeviceState {
            id: DEVICE_ID,
            mac: "aa:bb:cc:dd:ee:ff".to_string(),
            hostname: Some("jeremys-phone".to_string()),
            vendor: Some("Apple".to_string()),
            device_type: Some("phone".to_string()),
            os_family: Some("iOS".to_string()),
            state: Some("online".to_string()),
            last_ip: Some("192.168.1.42".to_string()),
            current_location: Some("living-room-ap".to_string()),
            first_seen: Some(NOW - 900_000),
            last_seen: Some(NOW - 30),
        }
    }

    fn history() -> DeviceHistory {
        DeviceHistory {
            presence: vec![Span::closed("", 100, 4_000), Span::open("", 990_000)],
            locations: vec![Span::closed("kitchen-ap", 100, 4_000)],
            addresses: vec![Span::closed("192.168.1.42", 100, 4_000)],
        }
    }

    fn empty_history() -> DeviceHistory {
        DeviceHistory {
            presence: vec![],
            locations: vec![],
            addresses: vec![],
        }
    }

    // --- the JSON round-trip constraint -----------------------------------

    /// The constraint the whole module's markup style exists to satisfy. If the
    /// fragment contains a `"` or a `\`, `serde_json::to_string` escapes it and
    /// the escape reaches the page as literal text
    /// (`G-VIEW-OUTPUT-JSON-ENCODED`).
    #[test]
    fn the_fragment_contains_no_character_that_serde_would_escape() {
        let html = render(Some(&state()), &history(), Some("Jeremy"), NOW);
        assert!(!html.contains('"'), "fragment contains a double quote");
        assert!(!html.contains('\\'), "fragment contains a backslash");
        assert!(!html.contains('\n'), "fragment contains a newline");
    }

    /// The same, for the unlinked path and the empty-history path, since they
    /// build different strings.
    #[test]
    fn every_render_path_survives_the_json_round_trip() {
        for html in [
            render(None, &empty_history(), None, NOW),
            render(Some(&state()), &empty_history(), None, NOW),
        ] {
            assert!(!html.contains('"'));
            assert!(!html.contains('\\'));
        }
    }

    /// Belt and braces: serialize the fragment the way the macro does and check
    /// the only escapes are the wrapping quotes.
    #[test]
    fn serializing_the_fragment_adds_only_the_two_wrapping_quotes() {
        let html = render(Some(&state()), &history(), Some("Jeremy"), NOW);
        let serialized = serde_json::to_string(&html).unwrap();
        assert_eq!(serialized.len(), html.len() + 2);
        assert_eq!(&serialized[1..serialized.len() - 1], html);
    }

    // --- escaping ---------------------------------------------------------

    #[test]
    fn escape_covers_the_html_five_plus_the_two_json_hazards() {
        assert_eq!(escape("a&b"), "a&amp;b");
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("say \"hi\""), "say &quot;hi&quot;");
        assert_eq!(escape("it's"), "it&#x27;s");
        assert_eq!(escape("a\\b"), "a&#x5C;b");
        assert_eq!(escape("a\nb"), "a b");
    }

    /// A hostname is DHCP option 12: whatever the client claims. This is the
    /// attack the escaping exists for.
    #[test]
    fn a_hostile_hostname_cannot_break_out_of_its_cell() {
        let mut s = state();
        s.hostname = Some("</dd><script>alert('x')</script>".to_string());
        let html = render(Some(&s), &empty_history(), None, NOW);
        assert!(!html.contains("<script>"));
        assert!(!html.contains("</dd><script"));
        assert!(html.contains("&lt;script&gt;"));
    }

    /// Single-quoted attributes mean an apostrophe is the character that would
    /// break out, so it must be entity-encoded rather than left raw.
    ///
    /// The assertion is on the *delimiter*, not on the payload: the words
    /// `onmouseover=alert(1)` do survive into the page as inert text, and that
    /// is correct — escaping neutralises the quote that would have made them an
    /// attribute, it does not censor the string.
    #[test]
    fn a_hostile_access_point_name_cannot_open_an_attribute() {
        let hostile = "ap' onmouseover=alert(1) x='";
        let history = DeviceHistory {
            presence: vec![],
            locations: vec![Span::closed(hostile, 100, 200)],
            addresses: vec![],
        };
        let html = render(Some(&state()), &history, None, NOW);
        // The whole fragment is free of raw apostrophes except the ones this
        // module wrote as attribute delimiters, and those are all in the
        // `attr='value'` pattern. The hostile string's own quotes are entities.
        assert!(html.contains(&escape(hostile)));
        assert!(!html.contains("ap' onmouseover"));
        assert!(html.contains("ap&#x27; onmouseover"));
    }

    /// The status word reaches a `class` attribute, so it is constrained to a
    /// known set rather than merely escaped — an escaped-but-arbitrary string in
    /// a class name is still an arbitrary string in a class name.
    #[test]
    fn an_unrecognised_daemon_state_renders_as_unknown_in_the_class_name() {
        let mut s = state();
        s.state = Some("x' onclick=evil".to_string());
        let html = render(Some(&s), &empty_history(), None, NOW);
        // The class name is the constrained word...
        assert!(html.contains("<dd>unknown ("));
        // ...and the daemon's raw string appears only escaped, in the text.
        assert!(!html.contains("x' onclick"));
        assert!(html.contains("x&#x27; onclick=evil"));
    }

    // --- content ----------------------------------------------------------

    #[test]
    fn the_identity_block_shows_what_the_daemon_knows() {
        let html = render(Some(&state()), &history(), Some("Jeremy"), NOW);
        for expected in [
            "aa:bb:cc:dd:ee:ff",
            "jeremys-phone",
            "Apple",
            "192.168.1.42",
            "living-room-ap",
            "Jeremy",
        ] {
            assert!(html.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn an_absent_or_blank_daemon_field_is_omitted_rather_than_shown_empty() {
        let mut s = state();
        s.hostname = None;
        s.vendor = Some("   ".to_string());
        let html = render(Some(&s), &empty_history(), None, NOW);
        assert!(!html.contains("Hostname"));
        assert!(!html.contains("Vendor"));
        // The mandatory rows are still there.
        assert!(html.contains("MAC"));
        assert!(html.contains("Last seen"));
    }

    #[test]
    fn an_unlinked_item_says_so_instead_of_rendering_empty_timelines() {
        let html = render(None, &empty_history(), None, NOW);
        assert!(html.contains("ng-device--unlinked"));
        assert!(!html.contains("ng-device__timeline"));
    }

    #[test]
    fn an_empty_timeline_says_so_rather_than_rendering_an_empty_table() {
        let html = render(Some(&state()), &empty_history(), None, NOW);
        assert!(html.contains("Never seen online."));
        assert!(!html.contains("<tbody></tbody>"));
    }

    #[test]
    fn the_open_session_is_marked_ongoing() {
        let html = render(Some(&state()), &history(), None, NOW);
        assert!(html.contains("ng-device__span--open"));
        assert!(html.contains("(ongoing)"));
    }

    /// A truncated list that does not say it truncated reads as "this is
    /// everything".
    #[test]
    fn a_truncated_timeline_reports_what_it_dropped() {
        let many: Vec<Span> = (0..timeline::MAX_SPANS as i64 + 10)
            .map(|i| Span::closed("", i * 100, i * 100 + 50))
            .collect();
        let total = many.len();
        let html = render(
            Some(&state()),
            &DeviceHistory {
                presence: many,
                locations: vec![],
                addresses: vec![],
            },
            None,
            NOW,
        );
        assert!(html.contains("ng-device__truncated"));
        assert!(html.contains(&format!("of {total} recorded")));
    }

    /// The summary is computed over every span, not only the displayed ones, so
    /// a truncated page still reports honest totals.
    #[test]
    fn the_summary_counts_every_session_not_only_the_displayed_ones() {
        let many: Vec<Span> = (0..timeline::MAX_SPANS + 10)
            .map(|i| Span::closed("", i as i64 * 100, i as i64 * 100 + 50))
            .collect();
        let total = many.len();
        let html = render(
            Some(&state()),
            &DeviceHistory {
                presence: many,
                locations: vec![],
                addresses: vec![],
            },
            None,
            NOW,
        );
        assert!(html.contains(&format!("{total} sessions")));
    }

    #[test]
    fn the_page_links_to_the_per_device_event_route() {
        let html = render(Some(&state()), &history(), None, NOW);
        assert!(html.contains(&format!("/events/device?device={DEVICE_ID}")));
    }
}
