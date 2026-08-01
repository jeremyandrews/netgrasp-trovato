//! Turning a device's raw spans into what its page actually shows.
//!
//! `ng_presence`, `ng_location_history` and `ng_ip_history` are all the same
//! shape — a label and a half-open interval ([`crate::model::Span`]) — so one
//! set of functions serves all three timelines. Keeping the arithmetic here
//! rather than in the renderer means the awkward cases (an open span, a clock
//! step, a device that has never been seen, spans the daemon wrote out of
//! order) are settled by unit test rather than discovered on a page.
//!
//! Nothing here emits markup. The renderer in the plugin owns escaping and
//! layout; this module owns "what does this device's week look like".

use crate::model::Span;

/// Spans shown per timeline before the list is cut.
///
/// A busy device on a flaky access point can produce hundreds of presence
/// sessions a day. The page shows the recent ones; the full history is the
/// record admin's job.
pub const MAX_SPANS: usize = 24;

/// One row of a rendered timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    /// The span's label (AP name, IP address), possibly empty.
    pub label: String,
    /// When it started, unix seconds.
    pub start: i64,
    /// When it ended, or `None` while open.
    pub end: Option<i64>,
    /// How long it lasted, in seconds, as of the render clock.
    pub duration_secs: i64,
    /// Whether it is the still-open span.
    pub open: bool,
}

/// Summary figures for a device, derived from its presence history.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresenceSummary {
    /// Number of sessions considered.
    pub sessions: usize,
    /// Total seconds present across them.
    pub total_secs: i64,
    /// Longest single session, in seconds.
    pub longest_secs: i64,
    /// Whether the device is present right now (it has an open session).
    pub present: bool,
    /// Most recent moment the device was seen, across all sessions.
    pub last_seen: Option<i64>,
}

/// Build a timeline: newest first, capped, durations resolved against `now`.
///
/// The input is not assumed sorted — the daemon writes rows as it observes them
/// and a re-import can interleave them — so this sorts rather than trusting an
/// `ORDER BY` that a future caller might drop.
#[must_use]
pub fn build(spans: &[Span], now: i64) -> Vec<TimelineRow> {
    let mut ordered: Vec<&Span> = spans.iter().collect();
    // Newest first. Ties broken by end so an open span (which is the newest
    // thing that can exist) sorts above a closed one that started at the same
    // second.
    ordered.sort_by(|a, b| {
        b.start
            .cmp(&a.start)
            .then_with(|| b.end.is_none().cmp(&a.end.is_none()))
    });

    ordered
        .into_iter()
        .take(MAX_SPANS)
        .map(|s| TimelineRow {
            label: s.label.clone(),
            start: s.start,
            end: s.end,
            duration_secs: s.duration_secs(now),
            open: s.is_open(),
        })
        .collect()
}

/// Whether [`build`] had to drop rows.
#[must_use]
pub fn is_truncated(spans: &[Span]) -> bool {
    spans.len() > MAX_SPANS
}

/// Summarize a device's presence history.
///
/// Computed over **every** span, not just the ones [`build`] shows, so the
/// figures on the page are not silently a function of the display cap.
#[must_use]
pub fn summarize(spans: &[Span], now: i64) -> PresenceSummary {
    let mut summary = PresenceSummary {
        sessions: spans.len(),
        ..PresenceSummary::default()
    };
    for span in spans {
        let secs = span.duration_secs(now);
        summary.total_secs = summary.total_secs.saturating_add(secs);
        summary.longest_secs = summary.longest_secs.max(secs);
        if span.is_open() {
            summary.present = true;
        }
        let seen = span.end.unwrap_or(now).max(span.start);
        summary.last_seen = Some(summary.last_seen.map_or(seen, |prev| prev.max(seen)));
    }
    summary
}

/// A duration as a short human string: `3d 4h`, `2h 15m`, `45s`.
///
/// Two units at most — a device page is scanned, not read — and never an empty
/// string, because a blank cell in a duration column reads as a bug.
#[must_use]
pub fn humanize_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else if minutes > 0 {
        if seconds > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{minutes}m")
        }
    } else {
        format!("{seconds}s")
    }
}

/// How long ago `then` was, as a short human string, or `never` for `None`.
#[must_use]
pub fn humanize_ago(then: Option<i64>, now: i64) -> String {
    match then {
        None => "never".to_string(),
        // A timestamp in the future is a clock disagreement between the daemon
        // host and the database, not a device that will be seen later.
        Some(t) if t >= now => "just now".to_string(),
        Some(t) => format!("{} ago", humanize_duration(now - t)),
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    #[test]
    fn a_timeline_is_newest_first_whatever_order_the_rows_arrived_in() {
        let spans = vec![
            Span::closed("a", 100, 200),
            Span::closed("c", 500, 600),
            Span::closed("b", 300, 400),
        ];
        let rows = build(&spans, NOW);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["c", "b", "a"]);
    }

    #[test]
    fn an_open_span_sorts_above_a_closed_one_that_started_at_the_same_second() {
        let spans = vec![Span::closed("closed", 500, 600), Span::open("open", 500)];
        let rows = build(&spans, NOW);
        assert_eq!(rows[0].label, "open");
        assert!(rows[0].open);
    }

    #[test]
    fn an_open_span_is_measured_to_now_and_flagged() {
        let rows = build(&[Span::open("living-room", 999_400)], NOW);
        assert_eq!(rows[0].duration_secs, 600);
        assert!(rows[0].open);
        assert!(rows[0].end.is_none());
    }

    #[test]
    fn a_timeline_is_capped_and_reports_that_it_was() {
        let spans: Vec<Span> = (0..MAX_SPANS as i64 + 5)
            .map(|i| Span::closed("ap", i * 100, i * 100 + 50))
            .collect();
        assert_eq!(build(&spans, NOW).len(), MAX_SPANS);
        assert!(is_truncated(&spans));
        assert!(!is_truncated(&spans[..MAX_SPANS]));
    }

    #[test]
    fn an_empty_history_yields_an_empty_timeline_not_a_panic() {
        assert!(build(&[], NOW).is_empty());
        assert!(!is_truncated(&[]));
    }

    // --- summary ----------------------------------------------------------

    #[test]
    fn a_summary_totals_every_span_not_only_the_displayed_ones() {
        let spans: Vec<Span> = (0..MAX_SPANS as i64 + 10)
            .map(|i| Span::closed("ap", i * 1_000, i * 1_000 + 100))
            .collect();
        let summary = summarize(&spans, NOW);
        assert_eq!(summary.sessions, spans.len());
        assert_eq!(summary.total_secs, 100 * spans.len() as i64);
        assert_eq!(summary.longest_secs, 100);
    }

    #[test]
    fn a_device_with_an_open_session_reads_as_present() {
        let summary = summarize(&[Span::closed("a", 0, 50), Span::open("b", 900_000)], NOW);
        assert!(summary.present);
        assert_eq!(summary.last_seen, Some(NOW));
    }

    #[test]
    fn a_device_with_only_closed_sessions_is_not_present_and_last_seen_is_its_latest_end() {
        let summary = summarize(
            &[Span::closed("a", 0, 50), Span::closed("b", 100, 900)],
            NOW,
        );
        assert!(!summary.present);
        assert_eq!(summary.last_seen, Some(900));
    }

    #[test]
    fn a_device_never_seen_has_no_last_seen() {
        let summary = summarize(&[], NOW);
        assert_eq!(summary, PresenceSummary::default());
        assert!(summary.last_seen.is_none());
        assert_eq!(humanize_ago(summary.last_seen, NOW), "never");
    }

    // --- humanize ---------------------------------------------------------

    #[test]
    fn durations_render_two_units_at_most_and_never_blank() {
        assert_eq!(humanize_duration(0), "0s");
        assert_eq!(humanize_duration(45), "45s");
        assert_eq!(humanize_duration(120), "2m");
        assert_eq!(humanize_duration(125), "2m 5s");
        assert_eq!(humanize_duration(3_600), "1h");
        assert_eq!(humanize_duration(8_100), "2h 15m");
        assert_eq!(humanize_duration(86_400), "1d");
        assert_eq!(humanize_duration(273_600), "3d 4h");
    }

    #[test]
    fn a_negative_duration_renders_as_zero_rather_than_as_a_minus_sign() {
        assert_eq!(humanize_duration(-500), "0s");
    }

    /// The daemon and the database can disagree about the clock. A device seen
    /// "in 3 minutes" is a configuration problem, not something to print.
    #[test]
    fn a_future_timestamp_reads_as_just_now() {
        assert_eq!(humanize_ago(Some(NOW + 200), NOW), "just now");
        assert_eq!(humanize_ago(Some(NOW), NOW), "just now");
    }

    #[test]
    fn a_past_timestamp_reads_as_an_interval_ago() {
        assert_eq!(humanize_ago(Some(NOW - 8_100), NOW), "2h 15m ago");
    }
}
