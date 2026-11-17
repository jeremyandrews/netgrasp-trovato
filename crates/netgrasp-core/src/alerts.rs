//! The Alerts page in words: what you will be told about, whether it will reach
//! you, and what was sent.
//!
//! Pure: rows in, sentences out. The plugin reads the rows (`queries`, the
//! `SELECT_NOTIFY_STATUS` family) and draws the page; everything a person reads
//! is decided here, where a unit test can hold it to the exact text.
//!
//! The daemon is the only sender. These sentences describe what it did, from
//! `ng_notify_status` (whether delivery is configured, and how the last attempt
//! went), `ng_notify_requests` (a test somebody asked for) and
//! `ng_events.notified` (what it delivered). They never name the ntfy topic,
//! the server or the token, because none of those is in any table the plugin
//! can read.
//!
//! People are named, never referred to by pronoun: netgrasp stores no pronouns
//! and must not guess them (`docs/UI.md`, Alerts).

use serde::Deserialize;

use crate::assist::device_label;

/// The daemon's status row (`SELECT_NOTIFY_STATUS`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct NotifyStatus {
    /// A notifier is built and the daemon's master switch is on.
    #[serde(default)]
    pub notify_configured: bool,
    /// Which transport, `ntfy`; absent when there is none.
    #[serde(default)]
    pub notify_backend: Option<String>,
    /// Whether the most recent attempt, an alert or a test, was accepted.
    #[serde(default)]
    pub last_delivery_ok: Option<bool>,
    /// When that attempt was made, epoch seconds.
    #[serde(default)]
    pub last_delivery_at: Option<i64>,
    /// Its calendar day, `YYYY-MM-DD`, in the database's time zone.
    #[serde(default)]
    pub last_delivery_day: Option<String>,
    /// Its time of day, `HH:MM`.
    #[serde(default)]
    pub last_delivery_clock: Option<String>,
    /// When the daemon last wrote the row, epoch seconds.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// The most recent test request (`SELECT_LATEST_TEST_REQUEST`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TestRequest {
    /// Row id.
    pub id: i64,
    /// When it was asked for, epoch seconds.
    #[serde(default)]
    pub requested_at: Option<i64>,
    /// When the daemon answered it; absent while it waits.
    #[serde(default)]
    pub handled_at: Option<i64>,
    /// `sent`, `failed` or `not_configured`; absent while it waits.
    #[serde(default)]
    pub outcome: Option<String>,
    /// `HH:MM` it was asked for.
    #[serde(default)]
    pub requested_clock: Option<String>,
    /// The day it was answered, `YYYY-MM-DD`.
    #[serde(default)]
    pub handled_day: Option<String>,
    /// `HH:MM` it was answered.
    #[serde(default)]
    pub handled_clock: Option<String>,
}

/// One delivered alert (`SELECT_RECENTLY_SENT`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SentAlert {
    /// Row id.
    pub id: i64,
    /// The daemon's event type.
    pub event_type: String,
    /// When it happened, epoch seconds.
    #[serde(default)]
    pub ts: Option<i64>,
    /// Its day, `YYYY-MM-DD`.
    #[serde(default)]
    pub day: Option<String>,
    /// Its time of day, `HH:MM`.
    #[serde(default)]
    pub clock: Option<String>,
    /// For a person event, who.
    #[serde(default)]
    pub person_name: Option<String>,
    /// For a person event, the device the daemon named.
    #[serde(default)]
    pub device_name: Option<String>,
    /// For a departure, when the device was last heard from, `HH:MM`.
    #[serde(default)]
    pub last_seen_clock: Option<String>,
    /// The device's address.
    #[serde(default)]
    pub device_mac: Option<String>,
    /// The name a person typed for it.
    #[serde(default)]
    pub device_display_name: Option<String>,
    /// The name the daemon resolved.
    #[serde(default)]
    pub device_resolved_name: Option<String>,
    /// Its DHCP hostname.
    #[serde(default)]
    pub device_hostname: Option<String>,
}

/// The device counts (`SELECT_ALERTING_COUNTS`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct AlertingCounts {
    /// Devices whose `notify` is on, hidden ones excepted.
    #[serde(default)]
    pub alerting: i64,
    /// Every device, hidden ones excepted.
    #[serde(default)]
    pub devices: i64,
}

/// "today at 18:42", "Tue 18:42", or "2026-11-03 18:42".
///
/// `day` and `clock` are the database's rendering of one instant; `today` is
/// its own calendar day, and `weekday` the instant's three-letter day name
/// when it fell within the last week.
#[must_use]
pub fn when(
    day: Option<&str>,
    clock: Option<&str>,
    today: &str,
    within_week: Option<&str>,
) -> String {
    match (day, clock) {
        (Some(d), Some(c)) if d == today => format!("today at {c}"),
        (Some(_), Some(c)) if within_week.is_some() => {
            format!("{} {c}", within_week.unwrap_or_default())
        }
        (Some(d), Some(c)) => format!("{d} {c}"),
        (None, Some(c)) => c.to_string(),
        _ => "at a time not recorded".to_string(),
    }
}

/// The delivery section: one to three sentences, plainest first.
///
/// `today` is the database's calendar day, so "today at 18:42" means the same
/// day the times were rendered in.
#[must_use]
pub fn delivery_sentences(status: Option<&NotifyStatus>, today: &str) -> Vec<String> {
    let Some(status) = status else {
        return vec![
            "netgraspd has not said whether alerts can be delivered.".to_string(),
            "A netgraspd with schema V4 says so when it starts. An older one, or one not \
             restarted since the upgrade, never does."
                .to_string(),
        ];
    };
    if !status.notify_configured {
        return vec![
            "Alerts are not delivered anywhere yet.".to_string(),
            "netgraspd has no ntfy destination, or its notifications are switched off. \
             Add [notify.ntfy] to its config and restart it."
                .to_string(),
        ];
    }
    let backend = status.notify_backend.as_deref().unwrap_or("ntfy");
    let mut out = vec![format!("Alerts go to {backend}.")];
    let at = when(
        status.last_delivery_day.as_deref(),
        status.last_delivery_clock.as_deref(),
        today,
        None,
    );
    match status.last_delivery_ok {
        None => out.push("Last alert delivered: never.".to_string()),
        Some(true) => out.push(format!("Last alert delivered {at}.")),
        Some(false) => out.push(format!(
            "The last alert, {at}, could not be delivered. netgraspd's log says why."
        )),
    }
    out
}

/// What became of the most recent test, or `None` when nobody has asked.
#[must_use]
pub fn test_sentence(request: Option<&TestRequest>, today: &str) -> Option<String> {
    let request = request?;
    let asked = request.requested_clock.as_deref().unwrap_or("earlier");
    let answered = when(
        request.handled_day.as_deref(),
        request.handled_clock.as_deref(),
        today,
        None,
    );
    Some(match request.outcome.as_deref() {
        None => format!(
            "Test alert asked for at {asked}: waiting for netgraspd, which answers within a few seconds."
        ),
        Some("sent") => format!("Test alert sent {answered}. Check your phone."),
        Some("failed") => {
            format!("Test alert {answered} could not be delivered. netgraspd's log says why.")
        }
        Some("not_configured") => {
            format!("Test alert {answered}: netgraspd has nowhere to send it.")
        }
        Some(other) => format!("Test alert {answered}: {other}."),
    })
}

/// "Tell me when Jamie gets home."
#[must_use]
pub fn arrive_rule(name: &str) -> String {
    format!("Tell me when {name} gets home.")
}

/// "Tell me when Jamie leaves."
#[must_use]
pub fn depart_rule(name: &str) -> String {
    format!("Tell me when {name} leaves.")
}

/// "Tell me when Jamie's iPhone comes online."
#[must_use]
pub fn device_rule(label: &str) -> String {
    format!("Tell me when {label} comes online.")
}

/// "166 devices alert when they come back online."
#[must_use]
pub fn devices_sentence(counts: AlertingCounts) -> String {
    match (counts.alerting, counts.devices) {
        (0, 0) => "There are no devices yet.".to_string(),
        (0, _) => "No device alerts when it comes back online.".to_string(),
        (1, 1) => "The one device alerts when it comes back online.".to_string(),
        (a, d) if a == d => format!("All {a} devices alert when they come back online."),
        (1, d) => format!("1 device of {d} alerts when it comes back online."),
        (a, d) => format!("{a} devices of {d} alert when they come back online."),
    }
}

/// One delivered alert, in the words the daemon sent it with.
///
/// The person events read exactly as netgraspd's ntfy titles do: "Jamie is
/// home: Jamie's iPhone joined at 18:42", "Jamie left: last seen 07:55". An
/// arrival recorded by a daemon from before it named the device falls back to
/// the device row's label.
#[must_use]
pub fn sent_sentence(alert: &SentAlert) -> String {
    let device = alert.device_name.clone().unwrap_or_else(|| {
        alert.device_mac.as_deref().map_or_else(
            || "a device".to_string(),
            |mac| {
                device_label(
                    alert.device_display_name.as_deref(),
                    alert.device_resolved_name.as_deref(),
                    alert.device_hostname.as_deref(),
                    None,
                    mac,
                )
            },
        )
    });
    let person = alert.person_name.as_deref().unwrap_or("Somebody");
    let clock = alert.clock.as_deref().unwrap_or("an unrecorded time");
    match alert.event_type.as_str() {
        "person_arrived" => format!("{person} is home: {device} joined at {clock}"),
        "person_departed" => format!(
            "{person} left: last seen {}",
            alert.last_seen_clock.as_deref().unwrap_or(clock)
        ),
        "new_device" => format!("New device: {device}"),
        "returned" => format!("Back online: {device}"),
        "went_offline" => format!("Offline: {device}"),
        "ip_changed" => format!("Address changed: {device}"),
        "name_updated" => format!("Identified: {device}"),
        "arp_scan" => format!("ARP scan from {device}"),
        "arp_spoof" => format!("ARP spoof by {device}"),
        "rogue_dhcp" => format!("Rogue DHCP server: {device}"),
        "ip_conflict" => format!("Address conflict: {device}"),
        "gratuitous_arp" => format!("Gratuitous ARP flood from {device}"),
        "identity_change" => format!("Device changed identity: {device}"),
        "device_location_changed" => format!("Moved: {device}"),
        other => format!("{}: {device}", other.replace('_', " ")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-11-17";

    fn configured() -> NotifyStatus {
        NotifyStatus {
            notify_configured: true,
            notify_backend: Some("ntfy".into()),
            last_delivery_ok: Some(true),
            last_delivery_at: Some(1),
            last_delivery_day: Some(TODAY.into()),
            last_delivery_clock: Some("18:42".into()),
            updated_at: Some(1),
        }
    }

    #[test]
    fn a_working_setup_says_where_alerts_go_and_when_the_last_one_went() {
        assert_eq!(
            delivery_sentences(Some(&configured()), TODAY),
            ["Alerts go to ntfy.", "Last alert delivered today at 18:42."]
        );
    }

    #[test]
    fn a_configured_setup_that_never_sent_says_never() {
        let status = NotifyStatus {
            last_delivery_ok: None,
            last_delivery_at: None,
            last_delivery_day: None,
            last_delivery_clock: None,
            ..configured()
        };
        assert_eq!(
            delivery_sentences(Some(&status), TODAY),
            ["Alerts go to ntfy.", "Last alert delivered: never."]
        );
    }

    #[test]
    fn a_failed_delivery_is_said_plainly_and_points_at_the_log() {
        let status = NotifyStatus {
            last_delivery_ok: Some(false),
            last_delivery_day: Some("2026-11-16".into()),
            ..configured()
        };
        assert_eq!(
            delivery_sentences(Some(&status), TODAY)[1],
            "The last alert, 2026-11-16 18:42, could not be delivered. netgraspd's log says why."
        );
    }

    #[test]
    fn no_destination_names_the_missing_config_section() {
        let status = NotifyStatus {
            notify_configured: false,
            notify_backend: None,
            ..NotifyStatus::default()
        };
        let text = delivery_sentences(Some(&status), TODAY).join(" ");
        assert!(text.contains("not delivered anywhere"), "{text}");
        assert!(text.contains("[notify.ntfy]"), "{text}");
    }

    #[test]
    fn a_daemon_that_never_reported_is_not_mistaken_for_a_broken_one() {
        let text = delivery_sentences(None, TODAY).join(" ");
        assert!(text.starts_with("netgraspd has not said"), "{text}");
    }

    #[test]
    fn the_test_result_follows_the_request_through_to_its_outcome() {
        let mut request = TestRequest {
            id: 1,
            requested_at: Some(1),
            requested_clock: Some("18:43".into()),
            ..TestRequest::default()
        };
        assert_eq!(
            test_sentence(Some(&request), TODAY).unwrap(),
            "Test alert asked for at 18:43: waiting for netgraspd, which answers within a few seconds."
        );
        request.outcome = Some("sent".into());
        request.handled_day = Some(TODAY.into());
        request.handled_clock = Some("18:43".into());
        assert_eq!(
            test_sentence(Some(&request), TODAY).unwrap(),
            "Test alert sent today at 18:43. Check your phone."
        );
        request.outcome = Some("not_configured".into());
        assert_eq!(
            test_sentence(Some(&request), TODAY).unwrap(),
            "Test alert today at 18:43: netgraspd has nowhere to send it."
        );
        assert_eq!(test_sentence(None, TODAY), None);
    }

    #[test]
    fn the_rules_are_sentences_with_the_name_and_no_pronoun() {
        assert_eq!(arrive_rule("Jamie"), "Tell me when Jamie gets home.");
        assert_eq!(depart_rule("Jamie"), "Tell me when Jamie leaves.");
        assert_eq!(
            device_rule("Jamie's iPhone"),
            "Tell me when Jamie's iPhone comes online."
        );
    }

    #[test]
    fn the_device_count_reads_as_a_sentence_at_every_size() {
        let c = |alerting, devices| devices_sentence(AlertingCounts { alerting, devices });
        assert_eq!(
            c(166, 166),
            "All 166 devices alert when they come back online."
        );
        assert_eq!(
            c(3, 166),
            "3 devices of 166 alert when they come back online."
        );
        assert_eq!(
            c(1, 166),
            "1 device of 166 alerts when it comes back online."
        );
        assert_eq!(c(0, 166), "No device alerts when it comes back online.");
        assert_eq!(c(0, 0), "There are no devices yet.");
    }

    fn arrival() -> SentAlert {
        SentAlert {
            id: 1,
            event_type: "person_arrived".into(),
            clock: Some("18:42".into()),
            person_name: Some("Jamie".into()),
            device_name: Some("Jamie's iPhone".into()),
            device_mac: Some("02:00:5e:00:00:02".into()),
            ..SentAlert::default()
        }
    }

    #[test]
    fn a_sent_arrival_reads_exactly_as_the_daemon_sent_it() {
        assert_eq!(
            sent_sentence(&arrival()),
            "Jamie is home: Jamie's iPhone joined at 18:42"
        );
    }

    #[test]
    fn an_arrival_recorded_before_the_daemon_named_devices_uses_the_row() {
        let alert = SentAlert {
            device_name: None,
            device_display_name: Some("Jamie's phone".into()),
            ..arrival()
        };
        assert_eq!(
            sent_sentence(&alert),
            "Jamie is home: Jamie's phone joined at 18:42"
        );
    }

    #[test]
    fn a_sent_departure_gives_the_last_seen_time() {
        let alert = SentAlert {
            event_type: "person_departed".into(),
            last_seen_clock: Some("07:55".into()),
            clock: Some("08:25".into()),
            ..arrival()
        };
        assert_eq!(sent_sentence(&alert), "Jamie left: last seen 07:55");
        let older = SentAlert {
            last_seen_clock: None,
            ..alert
        };
        assert_eq!(sent_sentence(&older), "Jamie left: last seen 08:25");
    }

    #[test]
    fn a_sent_device_alert_reads_like_its_ntfy_title() {
        let alert = SentAlert {
            event_type: "returned".into(),
            person_name: None,
            device_name: None,
            device_display_name: Some("Office printer".into()),
            ..arrival()
        };
        assert_eq!(sent_sentence(&alert), "Back online: Office printer");
        let new = SentAlert {
            event_type: "new_device".into(),
            ..alert
        };
        assert_eq!(sent_sentence(&new), "New device: Office printer");
    }

    #[test]
    fn times_are_short_and_say_today_when_they_are() {
        assert_eq!(
            when(Some(TODAY), Some("18:42"), TODAY, None),
            "today at 18:42"
        );
        assert_eq!(
            when(Some("2026-11-16"), Some("18:42"), TODAY, Some("Mon")),
            "Mon 18:42"
        );
        assert_eq!(
            when(Some("2026-11-01"), Some("18:42"), TODAY, None),
            "2026-11-01 18:42"
        );
    }
}
