//! The rows and payloads the Netgrasp core reasons about.
//!
//! These mirror the daemon's `ng_` tables closely enough to be deserialized
//! straight out of the `db` host's `query_raw` JSON, and deliberately no more
//! than that: anything derived (a title, a timeline, a retention cutoff) is a
//! function elsewhere in this crate, not a field here.

use serde::{Deserialize, Serialize};

/// A row of `ng_devices`, as the sync pass reads it.
///
/// Every daemon-owned column is `Option` because the daemon fills them in as it
/// learns them: a device is first seen as a MAC and an IP, and acquires a
/// hostname, a vendor and an OS guess later (or never). `mac` is the one
/// non-optional column — a device with no MAC is not a device.
///
/// The two timestamps are `i64` and are read from the daemon's generated
/// `<column>_epoch` companions, aliased back to these names in
/// [`crate::queries::SELECT_DIRTY_DEVICES`]. Reading `first_seen_at` itself
/// would deserialize as `null`: the `db` host has no `timestamptz` decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    /// Primary key of the `ng_devices` row. A `bigint` identity, not a uuid —
    /// the uuids on this row are the Item links.
    pub id: i64,
    /// Hardware address. The daemon's identity for the device.
    pub mac: String,
    /// Reverse-DNS or mDNS name, when one resolves.
    #[serde(default)]
    pub hostname: Option<String>,
    /// OUI lookup result.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Daemon's classification (`phone`, `laptop`, `iot`, …).
    #[serde(default)]
    pub device_type: Option<String>,
    /// Daemon's OS guess.
    #[serde(default)]
    pub os_family: Option<String>,
    /// `online` / `offline` / `new`, as the daemon last saw it.
    #[serde(default)]
    pub state: Option<String>,
    /// Most recent address.
    #[serde(default)]
    pub last_ip: Option<String>,
    /// Access point or segment the device was last seen on.
    #[serde(default)]
    pub current_location: Option<String>,
    /// First observation, unix seconds.
    #[serde(default)]
    pub first_seen: Option<i64>,
    /// Most recent observation, unix seconds.
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// The human's label for this device, written back from the Item's title.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The linked `ng_device` Item, once the sync pass has created one.
    #[serde(default)]
    pub trovato_item_id: Option<String>,
}

impl DeviceRow {
    /// A minimal row, for tests and for building one field at a time.
    #[must_use]
    pub fn new(id: i64, mac: impl Into<String>) -> Self {
        Self {
            id,
            mac: mac.into(),
            hostname: None,
            vendor: None,
            device_type: None,
            os_family: None,
            state: None,
            last_ip: None,
            current_location: None,
            first_seen: None,
            last_seen: None,
            display_name: None,
            trovato_item_id: None,
        }
    }
}

/// The user-owned overlay carried by an `ng_device` Item.
///
/// This is the whole of what an admin edits, and the whole of what the
/// write-back writes. It is a struct rather than loose JSON so that adding a
/// user-owned field is a compile error everywhere it has to be handled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceOverlay {
    /// The device's label — the Item's **title**, stored as `display_name`.
    pub display_name: String,
    /// Item id of the owning `ng_person`, or empty for unowned.
    pub owner_item_id: Option<String>,
    /// Free text the admin keeps about the device.
    pub notes: Option<String>,
    /// Hide from the default device lists.
    pub hidden: bool,
    /// Whether arrival/departure of this device is worth telling someone about.
    pub notify: bool,
}

/// The fields of an `ng_person` Item, mirrored into `ng_people` for the daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonFields {
    /// The person's name — the Item's title.
    pub name: String,
    /// Free text.
    pub notes: Option<String>,
    /// Tell someone when one of this person's devices appears.
    pub notify_arrive: bool,
    /// Tell someone when the last of this person's devices disappears.
    pub notify_depart: bool,
}

/// A row of `ng_events`, as the event log and the device page read it.
///
/// Not `Eq`: `details` is arbitrary JSON, and [`serde_json::Value`] is only
/// `PartialEq` because a float can be `NaN`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    /// `device_seen`, `device_new`, `device_offline`, `mac_conflict`, …
    pub event_type: String,
    /// Unix seconds, read from `timestamp_epoch`.
    pub timestamp: i64,
    /// The daemon's structured detail for the event.
    ///
    /// `ng_events.details` is `JSONB NOT NULL DEFAULT '{}'`, and the `db` host
    /// decodes JSONB, so this arrives already parsed — an object, not a string
    /// containing JSON. An event with nothing to add carries `{}`.
    #[serde(default)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

impl EventRow {
    /// One key of `details` as a string.
    ///
    /// Strings are unwrapped; anything else is rendered as its JSON form, so a
    /// numeric or boolean detail reads as `42` rather than as nothing. A key the
    /// daemon did not write is `None`.
    #[must_use]
    pub fn detail(&self, key: &str) -> Option<String> {
        match self.details.get(key)? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Null => None,
            other => Some(other.to_string()),
        }
    }
}

/// Event types the UI treats as security-relevant.
///
/// Kept here rather than in the migration's `IN (…)` list alone so the plugin
/// and the gather cannot drift apart silently — a test asserts they match.
pub const SECURITY_EVENT_TYPES: &[&str] = &[
    "device_new",
    "ip_conflict",
    "mac_conflict",
    "mac_spoof",
    "unknown_device",
];

/// Whether an event type is one the security views surface.
#[must_use]
pub fn is_security_event(event_type: &str) -> bool {
    SECURITY_EVENT_TYPES.contains(&event_type)
}

/// The daemon-owned state a device page shows above its timelines.
///
/// The projection is [`crate::queries::SELECT_DEVICE_STATE`]; the two times are
/// the epoch twins, aliased. Lives here rather than in the plugin so the test
/// that runs that query against the daemon's own DDL decodes it with the same
/// struct the plugin does, rather than with a restatement of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceState {
    /// `ng_devices.id`, used to link the per-device event route. A `bigint`
    /// identity — the per-device event gather filters `ng_events.device_id`,
    /// which is the same type.
    pub id: i64,
    /// Hardware address.
    pub mac: String,
    /// Resolved name, if any.
    #[serde(default)]
    pub hostname: Option<String>,
    /// OUI lookup result.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Daemon classification.
    #[serde(default)]
    pub device_type: Option<String>,
    /// Daemon OS guess.
    #[serde(default)]
    pub os_family: Option<String>,
    /// `online` / `offline` / `new`.
    #[serde(default)]
    pub state: Option<String>,
    /// Most recent address.
    #[serde(default)]
    pub last_ip: Option<String>,
    /// Access point or segment.
    #[serde(default)]
    pub current_location: Option<String>,
    /// First observation, unix seconds — `first_seen_at_epoch`, aliased.
    #[serde(default)]
    pub first_seen: Option<i64>,
    /// Most recent observation, unix seconds — `last_seen_at_epoch`, aliased.
    #[serde(default)]
    pub last_seen: Option<i64>,
}

/// Row shape shared by the three timeline queries, so one decode serves all.
///
/// `start` and `end` are the epoch twins of the interval's `timestamptz`
/// columns, aliased. `end` is `None` only for a genuinely open interval:
/// `ended_at_epoch` is generated from a null `ended_at` and is null with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanRow {
    /// The span's label: an access point, an IP, or empty for bare presence.
    #[serde(default)]
    pub label: Option<String>,
    /// Unix seconds the interval opened.
    pub start: i64,
    /// Unix seconds it closed, or `None` while it is still open.
    #[serde(default)]
    pub end: Option<i64>,
}

impl From<SpanRow> for Span {
    fn from(r: SpanRow) -> Self {
        Span {
            label: r.label.unwrap_or_default(),
            start: r.start,
            end: r.end,
        }
    }
}

/// A half-open interval on a device's history: a presence session, a location
/// stay, or the period an address was held.
///
/// `end` is `None` for the interval that is still open. Everything the device
/// page shows about presence, location and addressing is this one shape, which
/// is why [`crate::timeline`] has one set of functions rather than three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// What the interval is about: an AP name, an IP address, or empty for a
    /// bare presence session.
    #[serde(default)]
    pub label: String,
    /// Unix seconds the interval opened.
    pub start: i64,
    /// Unix seconds it closed, or `None` while it is still open.
    #[serde(default)]
    pub end: Option<i64>,
}

impl Span {
    /// A closed span.
    #[must_use]
    pub fn closed(label: impl Into<String>, start: i64, end: i64) -> Self {
        Self {
            label: label.into(),
            start,
            end: Some(end),
        }
    }

    /// A span that is still open.
    #[must_use]
    pub fn open(label: impl Into<String>, start: i64) -> Self {
        Self {
            label: label.into(),
            start,
            end: None,
        }
    }

    /// Duration in seconds as of `now`, never negative.
    ///
    /// An open span is measured to `now`; a closed span whose `end` precedes its
    /// `start` (a clock step, or a daemon writing them out of order) is reported
    /// as zero rather than as a negative duration that would render as garbage.
    #[must_use]
    pub fn duration_secs(&self, now: i64) -> i64 {
        let end = self.end.unwrap_or(now);
        (end - self.start).max(0)
    }

    /// Whether the span is still open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.end.is_none()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_device_row_deserializes_from_a_sparse_db_host_row() {
        // What `query_raw` returns for a device the daemon has only just seen:
        // an id, a mac, and nothing else resolved yet. The id is a JSON number,
        // because `ng_devices.id` is a bigint identity and the host decodes INT8
        // as a number.
        let row: DeviceRow =
            serde_json::from_str(r#"{"id":41,"mac":"aa:bb:cc:dd:ee:ff"}"#).unwrap();
        assert_eq!(row.id, 41);
        assert_eq!(row.mac, "aa:bb:cc:dd:ee:ff");
        assert!(row.hostname.is_none());
        assert!(row.trovato_item_id.is_none());
    }

    /// The row shape the sync pass actually decodes: a bigint id, epoch twins
    /// aliased onto the timestamp names, and a uuid Item link.
    #[test]
    fn a_device_row_decodes_the_epoch_twins_as_the_timestamps() {
        let row: DeviceRow = serde_json::from_str(
            r#"{"id":7,"mac":"aa:bb:cc:dd:ee:ff","first_seen":1000,"last_seen":2000,
                "trovato_item_id":"22222222-2222-4222-8222-222222222222"}"#,
        )
        .unwrap();
        assert_eq!(row.first_seen, Some(1_000));
        assert_eq!(row.last_seen, Some(2_000));
        assert_eq!(
            row.trovato_item_id.as_deref(),
            Some("22222222-2222-4222-8222-222222222222")
        );
    }

    /// `details` is JSONB, and the host decodes JSONB. A row that arrives with a
    /// string there — which is what the plugin used to expect — no longer
    /// deserializes, and that is the point.
    #[test]
    fn an_event_rows_details_decode_as_an_object_not_as_a_string() {
        let row: EventRow = serde_json::from_str(
            r#"{"event_type":"mac_spoof","timestamp":1000,
                "details":{"claimed_mac":"aa:bb:cc:dd:ee:ff","seen":3}}"#,
        )
        .unwrap();
        assert_eq!(
            row.detail("claimed_mac").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(row.detail("seen").as_deref(), Some("3"));
        assert!(row.detail("absent").is_none());

        assert!(
            serde_json::from_str::<EventRow>(
                r#"{"event_type":"x","timestamp":1,"details":"a string"}"#
            )
            .is_err(),
            "details decoded as a string — the JSONB column would silently lose its shape"
        );
    }

    #[test]
    fn an_event_with_no_detail_carries_an_empty_object() {
        let row: EventRow =
            serde_json::from_str(r#"{"event_type":"device_seen","timestamp":1000}"#).unwrap();
        assert!(row.details.is_empty());
        assert!(row.detail("anything").is_none());
    }

    #[test]
    fn an_open_span_is_measured_to_now() {
        let span = Span::open("living-room-ap", 1_000);
        assert_eq!(span.duration_secs(1_600), 600);
        assert!(span.is_open());
    }

    #[test]
    fn a_closed_span_ignores_now() {
        let span = Span::closed("kitchen-ap", 1_000, 1_300);
        assert_eq!(span.duration_secs(9_999), 300);
        assert!(!span.is_open());
    }

    /// A clock step or an out-of-order daemon write must not render as a
    /// negative duration on the device page.
    #[test]
    fn a_span_that_ends_before_it_starts_reports_zero_not_a_negative() {
        let span = Span::closed("ap", 2_000, 1_000);
        assert_eq!(span.duration_secs(3_000), 0);
    }

    #[test]
    fn security_event_membership_is_exactly_the_declared_list() {
        assert!(is_security_event("mac_spoof"));
        assert!(is_security_event("device_new"));
        assert!(!is_security_event("device_seen"));
        assert!(!is_security_event(""));
    }

    #[test]
    fn security_event_types_are_sorted_and_unique() {
        let mut sorted = SECURITY_EVENT_TYPES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, SECURITY_EVENT_TYPES.to_vec());
    }
}
