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
    /// The daemon's own resolved identity for the device, and the name every
    /// page shows: the answer its identity resolution settled on, with
    /// `identity_source` saying which signal produced it. It outranks the raw
    /// signals below, and leaving it out of the title derivation is what titled
    /// a Brother printer after a Singapore OUI holder (`docs/JOINT-RUN.md`,
    /// plugin finding 2).
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Reverse-DNS or DHCP hostname, when one resolves.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name, when one was advertised.
    #[serde(default)]
    pub mdns_name: Option<String>,
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

    // The rest of the user-owned set. Read by the sync pass for one purpose:
    // an Item minted without them reads them back as absent, and `field_bool`
    // maps an absent boolean to `false` — so a device Item created carrying
    // only its MAC turns `notify` off (`NOT NULL DEFAULT TRUE` in the daemon's
    // schema) the first time anything writes the whole overlay back. The mint
    // carries the row's values so the two tiers agree from birth.
    //
    // `Option<bool>` rather than `bool`, because these are read by one
    // projection and not by the other: `None` is "this statement did not read
    // it", which is not the same claim as `false`.
    /// Free text an admin keeps about the device.
    #[serde(default)]
    pub notes: Option<String>,
    /// Hidden from the default listings. `None` when unread.
    #[serde(default)]
    pub hidden: Option<bool>,
    /// Arrival and departure alerts. `None` when unread.
    #[serde(default)]
    pub notify: Option<bool>,
    /// Item id of the owning `ng_person`. `None` for unowned or unread.
    #[serde(default)]
    pub owner_item_id: Option<String>,
}

impl DeviceRow {
    /// A minimal row, for tests and for building one field at a time.
    #[must_use]
    pub fn new(id: i64, mac: impl Into<String>) -> Self {
        Self {
            id,
            mac: mac.into(),
            resolved_name: None,
            hostname: None,
            mdns_name: None,
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
            notes: None,
            hidden: None,
            notify: None,
            owner_item_id: None,
        }
    }

    /// The user-owned overlay this row currently carries, as a sparse edit.
    ///
    /// Used when the sync pass mints an Item for a row that has none: the Item
    /// is created carrying these values rather than carrying only the MAC, so
    /// the two tiers agree from the moment the Item exists. A column the
    /// projection did not read is absent here rather than defaulted, so a
    /// narrower read cannot invent a value.
    #[must_use]
    pub fn overlay(&self) -> DeviceEdit {
        DeviceEdit {
            display_name: None,
            owner_item_id: self.owner_item_id.clone().map(Some),
            notes: self.notes.clone(),
            hidden: self.hidden,
            notify: self.notify,
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

/// A **sparse** device overlay: the user-owned columns one edit names, and no
/// others.
///
/// [`DeviceOverlay`] is the whole overlay and is the right shape for the admin
/// content form, which submits every field: the saved Item *is* the new state.
/// It is the wrong shape for an assistant tool call, which names one thing —
/// "rename this to Office printer" says nothing about the alerts — and this is
/// what that difference cost:
///
/// A rename built a full overlay, filling the fields the call did not name from
/// the device's Item. The cron sync mints a device Item carrying only
/// `field_mac`, so `field_notify` was absent, `field_bool` read an absent
/// boolean as `false`, and the write-back wrote `notify = false` over a column
/// the daemon's schema defaults to `TRUE`. A rename turned the device's alerts
/// off, and the proposal card said nothing about it. Observed twice in one run,
/// on a rename and on an owner assignment, with 33 of 34 device Items still in
/// the state that reproduces it (`docs/JOINT-RUN.md`, plugin finding 1).
///
/// So an edit says, per column, either "set it to this" or nothing at all, and
/// [`crate::writeback::build_partial_update`] builds its `SET` list from
/// [`DeviceEdit::columns`]. A column nobody named is not in the statement, so
/// there is no value for it to be wrong about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEdit {
    /// The device's label — the Item's title, stored as `display_name`.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The owner. `Some(None)` unassigns; the outer `None` leaves it alone.
    ///
    /// Two levels because "give this device to nobody" and "this call is not
    /// about the owner" are different instructions with different statements,
    /// and one level cannot tell them apart.
    #[serde(default)]
    pub owner_item_id: Option<Option<String>>,
    /// The notes. An empty or blank string clears them.
    #[serde(default)]
    pub notes: Option<String>,
    /// Hidden from the default listings.
    #[serde(default)]
    pub hidden: Option<bool>,
    /// Arrival and departure alerts.
    #[serde(default)]
    pub notify: Option<bool>,
}

impl DeviceEdit {
    /// Every user-owned column, named. The shape an admin's whole-Item edit has.
    #[must_use]
    pub fn from_overlay(overlay: &DeviceOverlay) -> Self {
        Self {
            display_name: Some(overlay.display_name.clone()),
            owner_item_id: Some(overlay.owner_item_id.clone()),
            // A blank string and a `None` both become SQL `NULL`, which is what
            // the whole-Item path has always written for absent notes.
            notes: Some(overlay.notes.clone().unwrap_or_default()),
            hidden: Some(overlay.hidden),
            notify: Some(overlay.notify),
        }
    }

    /// Whether this edit names `column`.
    ///
    /// A column this type does not carry answers `false`, which is what makes
    /// "a daemon column cannot be written" hold here as well as in the
    /// statement builder.
    #[must_use]
    pub fn names(&self, column: &str) -> bool {
        match column {
            "display_name" => self.display_name.is_some(),
            "owner_item_id" => self.owner_item_id.is_some(),
            "notes" => self.notes.is_some(),
            "hidden" => self.hidden.is_some(),
            "notify" => self.notify.is_some(),
            _ => false,
        }
    }

    /// The user-owned columns this edit writes, in [`crate::columns::USER_OWNED`]
    /// order.
    ///
    /// The one answer to "what will change", read by the statement builder and
    /// by the proposal card alike, so what somebody clicks Apply on is what
    /// happens. Iterating `USER_OWNED` rather than listing the fields again is
    /// what keeps this in step with the statement's `SET` list.
    #[must_use]
    pub fn columns(&self) -> Vec<&'static str> {
        crate::columns::USER_OWNED
            .iter()
            .copied()
            .filter(|column| self.names(column))
            .collect()
    }

    /// Whether this edit would change nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns().is_empty()
    }
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
///
/// These are the daemon's own strings, and the set is the one the daemon's
/// `recent_security_events` selects: `arp_scan`, `arp_spoof`, `rogue_dhcp`,
/// `identity_change`, `ip_conflict`, `gratuitous_arp`. The daemon's
/// `EventType::as_str` is the only source of a value that ever lands in
/// `ng_events.event_type`, so a name absent from it can only ever match zero
/// rows.
///
/// The first run of the plugin against a live daemon database found four of the
/// five names here matched nothing: `device_new`, `mac_conflict`, `mac_spoof`
/// and `unknown_device` were never in the daemon's vocabulary (`new_device` and
/// `arp_spoof` are the two that were meant), so `/events/security` silently
/// showed `ip_conflict` alone and looked like a working page. The in-tree test
/// could not have caught it: it checks that the Rust list and the SQL list
/// agree with each other, and they did — both were wrong in the same way.
/// Nothing in this repository holds the daemon's vocabulary to compare against.
///
/// Note that `new_device` is deliberately *not* here. It is a routine event on
/// any network with a visitor on it, the daemon does not count it as security
/// relevant, and the event log at `/events` already carries it.
///
/// Sorted, because a test asserts it is.
pub const SECURITY_EVENT_TYPES: &[&str] = &[
    "arp_scan",
    "arp_spoof",
    "gratuitous_arp",
    "identity_change",
    "ip_conflict",
    "rogue_dhcp",
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
    /// The place the daemon resolved the device's access point to ("Studio").
    /// Null whenever the daemon's UniFi enrichment is off or has not placed
    /// this device, which on most installs is always.
    #[serde(default)]
    pub current_location: Option<String>,
    /// The access point the device is associated with, as the controller names
    /// it ("Studio AP"). Null for the same reasons, and also for a wired device.
    #[serde(default)]
    pub current_ap: Option<String>,
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

    // --- the sparse edit --------------------------------------------------

    /// The change set is in `USER_OWNED` order because it is *read out of*
    /// `USER_OWNED`, which is the same thing that orders the statement's `SET`
    /// list. Two lists in one order by construction rather than by agreement.
    #[test]
    fn an_edits_columns_are_the_user_owned_ones_it_names_in_that_order() {
        let edit = DeviceEdit {
            notify: Some(false),
            display_name: Some("Office printer".into()),
            ..DeviceEdit::default()
        };
        assert_eq!(edit.columns(), ["display_name", "notify"]);
        assert!(edit.names("display_name"));
        assert!(edit.names("notify"));
        assert!(!edit.names("hidden"));
        assert!(!edit.is_empty());
    }

    #[test]
    fn an_edit_that_names_nothing_is_empty_and_an_edit_of_everything_is_full() {
        assert!(DeviceEdit::default().is_empty());
        assert!(DeviceEdit::default().columns().is_empty());
        assert_eq!(
            DeviceEdit::from_overlay(&DeviceOverlay::default()).columns(),
            crate::columns::USER_OWNED.to_vec()
        );
    }

    /// Unassigning names the owner column. One level of `Option` could not tell
    /// "give it to nobody" from "this call is not about the owner", and the
    /// difference is a statement.
    #[test]
    fn unassigning_names_the_owner_and_silence_about_it_does_not() {
        let unassign = DeviceEdit {
            owner_item_id: Some(None),
            ..DeviceEdit::default()
        };
        assert_eq!(unassign.columns(), ["owner_item_id"]);
        assert!(DeviceEdit::default().columns().is_empty());
    }

    /// A column outside the user-owned set is not named by any edit, whatever
    /// it is asked about — the same guarantee the statement builder makes, made
    /// one layer earlier so the card cannot display one either.
    #[test]
    fn an_edit_never_names_a_daemon_column() {
        let full = DeviceEdit::from_overlay(&DeviceOverlay::default());
        for daemon in crate::columns::DAEMON_OWNED {
            assert!(!full.names(daemon), "an edit claims to name {daemon}");
        }
        assert!(!full.names("trovato_item_id"));
        assert!(!full.names(""));
    }

    /// A row's overlay carries what the projection read and stays silent about
    /// the rest: a narrower read must not assert `notify = false` about a device
    /// whose `notify` column it never looked at.
    #[test]
    fn a_rows_overlay_is_silent_about_a_column_the_projection_did_not_read() {
        let mut row = DeviceRow::new(1, "aa:bb:cc:dd:ee:ff");
        assert!(row.overlay().is_empty());

        row.notify = Some(true);
        row.hidden = Some(false);
        row.notes = Some("in the hall cupboard".into());
        let overlay = row.overlay();
        assert_eq!(overlay.columns(), ["hidden", "notes", "notify"]);
        assert_eq!(overlay.notify, Some(true));
        // Still silent about the owner, which this row has not read.
        assert!(!overlay.names("owner_item_id"));
        // And never about the title: a row's `display_name` is what the sync
        // derives a title FROM, not something a mint writes back.
        assert!(!overlay.names("display_name"));
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
        assert!(is_security_event("arp_spoof"));
        assert!(is_security_event("ip_conflict"));
        assert!(!is_security_event("device_seen"));
        assert!(!is_security_event(""));
    }

    /// The names that were in this list before it was checked against a running
    /// daemon. None of them is a value `EventType::as_str` can return, so each
    /// one could only ever have matched zero rows.
    #[test]
    fn the_names_the_daemon_never_writes_are_gone() {
        for stale in ["device_new", "mac_conflict", "mac_spoof", "unknown_device"] {
            assert!(
                !is_security_event(stale),
                "{stale} is not a netgrasp daemon event type; it matches nothing"
            );
        }
    }

    /// `new_device` is a real daemon event and deliberately not a security one:
    /// the daemon's own `recent_security_events` omits it, and `/events` lists
    /// it already.
    #[test]
    fn new_device_is_a_real_event_but_not_a_security_one() {
        assert!(!is_security_event("new_device"));
    }

    #[test]
    fn security_event_types_are_sorted_and_unique() {
        let mut sorted = SECURITY_EVENT_TYPES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, SECURITY_EVENT_TYPES.to_vec());
    }
}
