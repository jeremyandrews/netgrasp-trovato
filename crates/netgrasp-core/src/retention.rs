//! Event pruning: how far back the event log keeps, and how much it deletes at
//! a time.
//!
//! Events are lightweight records (`DESIGN.md` Decision 2), so a retention pass
//! is one bounded `DELETE` rather than 300 `delete-item` host calls a day. Two
//! bounds matter and both are here rather than inline in the SQL:
//!
//! - the **cutoff**, which must match the daemon's own retention so the two
//!   processes do not fight over the same rows;
//! - the **batch size**, because the `db` host imposes a 5 s `statement_timeout`
//!   and a first pass over a long-neglected install could otherwise be a
//!   multi-million-row delete that times out on every attempt and therefore
//!   never makes progress.

/// Default retention window: 90 days, matching the daemon's own event retention.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// Shortest retention the plugin will honour.
///
/// Below this the event log stops being useful for the thing it exists for
/// (looking back at what happened on the network), and a misconfigured zero
/// would silently delete the log as fast as the daemon wrote it.
pub const MIN_RETENTION_DAYS: i64 = 1;

/// Longest retention the plugin will honour.
pub const MAX_RETENTION_DAYS: i64 = 3_650;

/// Rows deleted per cron tick.
///
/// Sized against the 5 s `statement_timeout` on the `db` host: a delete of this
/// many indexed rows completes well inside it, and a backlog drains over
/// successive ticks instead of timing out forever on one enormous statement.
pub const PRUNE_BATCH: i64 = 5_000;

/// Seconds in a day.
const DAY: i64 = 86_400;

/// Clamp a configured retention to a sane window.
///
/// Coercion rather than rejection, because `tap_item_presave` cannot refuse a
/// save (`G-NO-PRESAVE-VETO`) and the same reasoning applies to a variable: the
/// plugin's only options are to use a bad value or to use a near one.
#[must_use]
pub fn clamp_days(days: i64) -> i64 {
    days.clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS)
}

/// The timestamp before which events are eligible for deletion.
///
/// Saturating, so a nonsense clock cannot produce a cutoff in the far future
/// that would delete the whole log.
#[must_use]
pub fn cutoff(now: i64, days: i64) -> i64 {
    now.saturating_sub(clamp_days(days).saturating_mul(DAY))
}

/// Whether an event at `timestamp` is past its retention as of `now`.
#[must_use]
pub fn is_expired(timestamp: i64, now: i64, days: i64) -> bool {
    timestamp < cutoff(now, days)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_default_window_is_ninety_days_behind_now() {
        assert_eq!(cutoff(NOW, DEFAULT_RETENTION_DAYS), NOW - 90 * DAY);
    }

    #[test]
    fn a_configured_window_is_clamped_rather_than_refused() {
        assert_eq!(clamp_days(0), MIN_RETENTION_DAYS);
        assert_eq!(clamp_days(-5), MIN_RETENTION_DAYS);
        assert_eq!(clamp_days(1_000_000), MAX_RETENTION_DAYS);
        assert_eq!(clamp_days(30), 30);
    }

    /// A zero or negative retention would otherwise delete the log as fast as
    /// the daemon wrote it.
    #[test]
    fn a_zero_retention_still_keeps_a_days_worth() {
        assert_eq!(cutoff(NOW, 0), NOW - DAY);
        assert!(!is_expired(NOW - 3_600, NOW, 0));
    }

    #[test]
    fn expiry_is_strictly_older_than_the_cutoff() {
        let c = cutoff(NOW, 90);
        assert!(is_expired(c - 1, NOW, 90));
        assert!(!is_expired(c, NOW, 90));
        assert!(!is_expired(NOW, NOW, 90));
    }

    /// A clock at or near zero must not produce a cutoff that wraps into a
    /// value larger than every timestamp in the table.
    #[test]
    fn a_nonsense_clock_cannot_produce_a_cutoff_that_deletes_everything() {
        assert_eq!(cutoff(0, DEFAULT_RETENTION_DAYS), -(90 * DAY));
        assert!(!is_expired(0, 0, DEFAULT_RETENTION_DAYS));
        assert_eq!(cutoff(i64::MIN, MAX_RETENTION_DAYS), i64::MIN);
    }

    /// A compile-time bound rather than a runtime one: the point is to make
    /// raising [`PRUNE_BATCH`] past what the 5 s `db` statement timeout can
    /// finish fail the build, not a test run.
    #[test]
    fn the_batch_is_bounded_so_a_backlog_drains_instead_of_timing_out() {
        const {
            assert!(PRUNE_BATCH > 0);
            assert!(
                PRUNE_BATCH <= 50_000,
                "a batch this size risks the 5s db statement timeout"
            );
        }
    }
}
