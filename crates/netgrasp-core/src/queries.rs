//! Every statement the plugin issues against the daemon's tables, in one place.
//!
//! These live here rather than inline in the plugin's host port for one reason:
//! the plugin's SQL is written against a schema **another process owns**, and
//! nothing on this side type-checks it. Hoisting the statements into the
//! host-agnostic core lets a test apply the daemon's own DDL to a scratch schema
//! and run these exact strings against it — not paraphrases of them, which is
//! what a test that restates the SQL would be checking.
//!
//! # The two shapes every statement here obeys
//!
//! **Device ids are `bigint`.** `ng_devices.id` is
//! `BIGINT GENERATED ALWAYS AS IDENTITY`, so a device id binds `::bigint`. The
//! uuid columns that remain are the Item links (`trovato_item_id`,
//! `owner_item_id`, `ng_people.item_id`), which are uuids because the kernel's
//! `item.id` is.
//!
//! **Times are read from the epoch twin, never from the `timestamptz`.** The
//! `db` host decodes a fixed list of Postgres types and falls through to a
//! `String` decode for everything else (`crates/kernel/src/host/db.rs`); a
//! `timestamptz` cannot decode as a string, so it arrives as `null`. The gather
//! path is inconsistent with it rather than better — it wraps the query in
//! Postgres' `row_to_json`, so the same column arrives there as an ISO 8601
//! string. Neither is the `i64` this plugin renders from. So every timestamp is
//! read through its generated `<column>_epoch` companion and aliased to the name
//! the row struct already expects.

/// Dirty device rows for one sync pass. `$1` is the page size.
///
/// Columns are named rather than `SELECT *`, so a daemon that adds one cannot
/// change what this plugin decodes — which matters more here than it would
/// anywhere else, since the daemon adds columns on its own schedule and the
/// plugin finds out afterwards.
///
/// Ordered by the timestamp rather than by its epoch twin: they sort
/// identically, and the daemon indexes the timestamp.
///
/// It reads the **whole user-owned set**, not just `display_name`, for the one
/// step of the pass that needs it: minting an Item for a row that has none. An
/// Item created carrying only its MAC reads every other user-owned field back
/// as absent, and `field_bool` reads an absent boolean as `false` — so the next
/// whole-Item write-back turned `notify` off on a column the daemon defaults to
/// `TRUE`. The mint carries the row's values instead, so the two tiers agree
/// from the moment the Item exists (`docs/JOINT-RUN.md`, plugin finding 1).
///
/// `resolved_name` and `mdns_name` are read for the title: they are what the
/// daemon's identity resolution settled on, and the derivation that skipped
/// them titled a Brother printer after the OUI holder it bought its address
/// block from (finding 2).
pub const SELECT_DIRTY_DEVICES: &str = "SELECT id, mac, resolved_name, hostname, mdns_name, \
     vendor, device_type, os_family, state, last_ip, current_location, \
     first_seen_at_epoch AS first_seen, last_seen_at_epoch AS last_seen, \
     display_name, notes, hidden, notify, owner_item_id::text AS owner_item_id, \
     trovato_item_id \
     FROM ng_devices WHERE sync_state = 'dirty' ORDER BY last_seen_at DESC LIMIT $1";

/// Point a device row at its Item. `$1` is the Item id, `$2` the device id.
///
/// Writes `trovato_item_id` only — a link-owned column, so it touches neither
/// the daemon's set nor the user's.
pub const UPDATE_LINK_ITEM: &str =
    "UPDATE ng_devices SET trovato_item_id = $1::uuid WHERE id = $2::bigint";

/// Lower the daemon's dirty flag for one row. `$1` is the device id.
pub const UPDATE_MARK_CLEAN: &str =
    "UPDATE ng_devices SET sync_state = 'clean' WHERE id = $1::bigint";

/// The daemon's own naming inputs for the device behind an Item. `$1` is the
/// Item id.
///
/// Every column [`crate::sync::daemon_title`] reads, which is the point: the
/// write-back compares an admin's title against what the daemon alone would
/// call the device, and a projection missing a name column makes that
/// comparison answer "the admin typed this" about a name the admin never
/// touched — pinning it as `display_name`.
pub const SELECT_DAEMON_TITLE_FIELDS: &str = "SELECT mac, resolved_name, hostname, mdns_name, \
     vendor FROM ng_devices WHERE trovato_item_id = $1::uuid LIMIT 1";

/// Unassign every device owned by a person being retired. `$1` is their Item id.
pub const UPDATE_CLEAR_OWNER: &str =
    "UPDATE ng_devices SET owner_item_id = NULL WHERE owner_item_id = $1::uuid";

/// Drop a retired person's mirror row. `$1` is their Item id.
pub const DELETE_PERSON_MIRROR: &str = "DELETE FROM ng_people WHERE item_id = $1::uuid";

/// Unlink a device row whose Item an admin deleted, and queue it for a fresh
/// one. `$1` is the deleted Item id.
pub const UPDATE_UNLINK_DEVICE: &str = "UPDATE ng_devices SET trovato_item_id = NULL, \
     sync_state = 'dirty' WHERE trovato_item_id = $1::uuid";

/// Delete a bounded batch of expired events. `$1` is the cutoff in unix
/// seconds, `$2` the batch size.
///
/// Compares `timestamp_epoch`, not `"timestamp"`: the cutoff is computed in unix
/// seconds by [`crate::retention::cutoff`], and binding an integer against a
/// `timestamptz` column would have to go through `to_timestamp` on every row.
pub const DELETE_EXPIRED_EVENTS: &str = "DELETE FROM ng_events WHERE id IN (\
     SELECT id FROM ng_events WHERE timestamp_epoch < $1::bigint LIMIT $2::bigint)";

/// The daemon's row for a device Item. `$1` is the Item id.
pub const SELECT_DEVICE_STATE: &str = "SELECT id, mac, hostname, vendor, device_type, os_family, \
     state, last_ip, current_location, current_ap, first_seen_at_epoch AS first_seen, \
     last_seen_at_epoch AS last_seen \
     FROM ng_devices WHERE trovato_item_id = $1::uuid LIMIT 1";

/// A device's presence sessions, newest first. `$1` is the device id, `$2` the
/// row limit.
///
/// Summary rows are excluded on all three timelines that have them: a summary
/// row is a compacted day, not a session, and the page counts sessions and
/// reports a longest session. A summary row can also carry a null `ended_at`
/// without being open — the daemon's partial unique index on the open row
/// excludes them — so including one would render a permanent "(ongoing)".
pub const SELECT_PRESENCE_SPANS: &str = "SELECT ''::text AS label, started_at_epoch AS start, ended_at_epoch AS end \
     FROM ng_presence WHERE device_id = $1::bigint AND is_summary = FALSE \
     ORDER BY started_at DESC LIMIT $2::bigint";

/// A device's location stays, newest first, labelled by location.
///
/// Labelled by `location` rather than by `ap_name`: `location` is `NOT NULL` and
/// is the daemon's resolved answer, where `ap_name` is the raw access point and
/// may be null.
pub const SELECT_LOCATION_SPANS: &str = "SELECT location AS label, started_at_epoch AS start, ended_at_epoch AS end \
     FROM ng_location_history WHERE device_id = $1::bigint AND is_summary = FALSE \
     ORDER BY started_at DESC LIMIT $2::bigint";

/// A device's address holdings, newest first, labelled by IP.
///
/// `ng_ip_history` has no `is_summary` and its `last_seen` is `NOT NULL`, so an
/// address span is always closed — unlike a presence session, an address
/// holding is never rendered as ongoing.
pub const SELECT_ADDRESS_SPANS: &str = "SELECT ip AS label, first_seen_epoch AS start, last_seen_epoch AS end \
     FROM ng_ip_history WHERE device_id = $1::bigint ORDER BY first_seen DESC LIMIT $2::bigint";

/// A person's name, from the mirror. `$1` is their Item id.
pub const SELECT_OWNER_NAME: &str = "SELECT name FROM ng_people WHERE item_id = $1::uuid LIMIT 1";

/// The database's clock, in unix seconds.
pub const SELECT_CLOCK: &str = "SELECT EXTRACT(EPOCH FROM NOW())::bigint AS ts";

/// The earliest observation the database holds, in unix seconds, or null when it
/// holds none.
///
/// The oldest of the two tables that record *when*: a presence session's start
/// and an event's timestamp. `LEAST` ignores nulls and answers null only when
/// both do, so a database with events and no presence still answers, and an
/// empty one answers "nothing yet" rather than zero.
///
/// It aggregates the `timestamptz` and extracts the epoch from the **one**
/// resulting value, rather than aggregating the generated epoch twin. Both give
/// the same number and only this one can use the daemon's indexes: the twins are
/// `GENERATED … STORED` columns the daemon does not index, so `MIN` over one is
/// a sequential scan of the whole table, and `ng_events` is the largest table
/// here and the one with a 5 s statement timeout in front of it. Extracting
/// after the aggregate is what keeps the `null` decode problem to a value that
/// has already become a `bigint`.
pub const SELECT_MONITORING_START: &str = "SELECT LEAST(\
     (SELECT EXTRACT(EPOCH FROM MIN(started_at))::bigint FROM ng_presence), \
     (SELECT EXTRACT(EPOCH FROM MIN(\"timestamp\"))::bigint FROM ng_events)) AS earliest";

// ===========================================================================
// The assistant scopes
// ===========================================================================
//
// Every one of these is a READ. The assistant's writes go through the same two
// statements an admin's edit does — `writeback::build_update` for a device and
// `build_person_upsert` for a person — so there is one definition of what a
// user-owned write is, and adding a tool cannot widen it.
//
// The projection every device read shares. Named once so the row struct that
// decodes it (`assist_host::DeviceReadRow`) has exactly one shape to match, and
// so a column added to one listing cannot be missing from another.

/// The projection every assistant device read shares, as a macro.
///
/// A macro over `concat!` rather than a `const` spliced with `format!`: this is
/// nine statements that must project identically, because one row struct decodes
/// all nine and a column missing from one of them is a field that is silently
/// `None` on exactly one code path. `concat!` makes them one string at compile
/// time, so the daemon-schema test still runs the real statements rather than a
/// template of them, and nothing is built at run time from anything.
///
/// `owner_name` comes from a correlated subquery on the mirror rather than a
/// join, so the statement still addresses `ng_devices` directly: the view exists
/// for the record tier, and the plugin's own statements name the real table
/// (`DESIGN.md` Decision 8). A device whose owner Item was deleted keeps its
/// `owner_item_id` and resolves no name, which is the state the demo seed carries
/// on purpose.
macro_rules! device_read {
    ($tail:literal) => {
        concat!(
            "SELECT d.id, d.mac, d.display_name, d.resolved_name, d.hostname, d.mdns_name, ",
            "d.vendor, d.device_type, d.os_family, d.state, d.last_ip, d.last_ipv6, ",
            "d.current_ap, d.current_location, d.notes, d.hidden, d.notify, ",
            "d.owner_item_id::text AS owner_item_id, ",
            "d.trovato_item_id::text AS trovato_item_id, ",
            "d.first_seen_at_epoch AS first_seen, d.last_seen_at_epoch AS last_seen, ",
            "(SELECT p.name FROM ng_people p WHERE p.item_id = d.owner_item_id) AS owner_name ",
            "FROM ng_devices d ",
            $tail
        )
    };
}

/// One device by MAC. `$1` is the normalized address.
pub const SELECT_DEVICE_BY_MAC: &str = device_read!("WHERE lower(d.mac) = lower($1::text) LIMIT 1");

/// One device by its row id. `$1` is the bigint identity.
pub const SELECT_DEVICE_BY_ID: &str = device_read!("WHERE d.id = $1::bigint LIMIT 1");

/// One device by the Item that overlays it. `$1` is the Item id.
pub const SELECT_DEVICE_BY_ITEM: &str = device_read!("WHERE d.trovato_item_id = $1::uuid LIMIT 1");

/// Every device, newest sighting first. `$1` is the row limit.
pub const SELECT_DEVICES_ALL: &str =
    device_read!("ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $1::bigint");

/// Devices belonging to one person. `$1` is their Item id, `$2` the row limit.
pub const SELECT_DEVICES_BY_OWNER: &str = device_read!(
    "WHERE d.owner_item_id = $1::uuid ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $2::bigint"
);

/// Devices nobody owns. `$1` is the row limit.
pub const SELECT_DEVICES_UNOWNED: &str = device_read!(
    "WHERE d.owner_item_id IS NULL ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $1::bigint"
);

/// Devices in one state. `$1` is the state, `$2` the row limit.
pub const SELECT_DEVICES_BY_STATE: &str = device_read!(
    "WHERE d.state = $1::text ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $2::bigint"
);

/// Devices an admin hid from the listings. `$1` is the row limit.
pub const SELECT_DEVICES_HIDDEN: &str =
    device_read!("WHERE d.hidden = TRUE ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $1::bigint");

/// Devices whose name or address contains a fragment. `$1` is the fragment,
/// `$2` the row limit.
///
/// Five columns, because a person searching for a device does not know which of
/// them holds the name they remember: the one they typed, the one the daemon
/// resolved, the DHCP hostname, the mDNS name, or the address itself.
///
/// The fragment is **bound**, and the `%` wrapping happens in SQL rather than in
/// Rust — so this is not injection in any form. It is worth being precise about
/// what that does and does not buy: a `%` or a `_` inside the fragment is still
/// a `LIKE` wildcard once it is concatenated, so searching for `%` matches every
/// device. That is harmless (a read, bounded by the row limit) and occasionally
/// useful, and it is stated here rather than claimed away.
pub const SELECT_DEVICES_BY_NAME: &str = device_read!(
    "WHERE d.display_name ILIKE '%' || $1::text || '%' \
        OR d.resolved_name ILIKE '%' || $1::text || '%' \
        OR d.hostname ILIKE '%' || $1::text || '%' \
        OR d.mdns_name ILIKE '%' || $1::text || '%' \
        OR d.mac ILIKE '%' || $1::text || '%' \
     ORDER BY d.last_seen_at DESC NULLS LAST LIMIT $2::bigint"
);

/// Every person, with how many devices are theirs. `$1` is the row limit.
///
/// A left join and a count rather than a per-person query: "who exists and how
/// much do they own" is one question, and asking it per person would put a query
/// per person inside one tool call.
pub const SELECT_PEOPLE_WITH_COUNTS: &str = "SELECT p.item_id::text AS item_id, p.name, \
     p.notes, p.notify_arrive, p.notify_depart, p.state, p.current_location, \
     COUNT(d.id) AS device_count \
     FROM ng_people p LEFT JOIN ng_devices d ON d.owner_item_id = p.item_id \
     GROUP BY p.item_id, p.name, p.notes, p.notify_arrive, p.notify_depart, p.state, \
              p.current_location \
     ORDER BY p.name LIMIT $1::bigint";

/// One person, with their device count. `$1` is their Item id.
pub const SELECT_PERSON_WITH_COUNT: &str = "SELECT p.item_id::text AS item_id, p.name, \
     p.notes, p.notify_arrive, p.notify_depart, p.state, p.current_location, \
     COUNT(d.id) AS device_count \
     FROM ng_people p LEFT JOIN ng_devices d ON d.owner_item_id = p.item_id \
     WHERE p.item_id = $1::uuid \
     GROUP BY p.item_id, p.name, p.notes, p.notify_arrive, p.notify_depart, p.state, \
              p.current_location";

/// How many devices name this person as their owner. `$1` is their Item id.
///
/// Its own statement rather than a filter on the listing, because it is asked at
/// the moment of a delete and the answer decides whether the delete happens.
pub const SELECT_OWNED_DEVICE_COUNT: &str =
    "SELECT COUNT(*) AS device_count FROM ng_devices WHERE owner_item_id = $1::uuid";

/// Recent events for one device. `$1` is the device id, `$2` the cutoff in unix
/// seconds, `$3` the row limit.
pub const SELECT_DEVICE_EVENTS: &str = "SELECT event_type, timestamp_epoch AS ts, \
     details::text AS details \
     FROM ng_events WHERE device_id = $1::bigint AND timestamp_epoch >= $2::bigint \
     ORDER BY timestamp_epoch DESC LIMIT $3::bigint";

/// Recent security events across the network. `$1` is the cutoff in unix
/// seconds, `$2` the row limit.
///
/// The type list is inline rather than bound, and is the one place in this file
/// that is: it is a compile-time constant with no value from outside in it, and
/// binding an array through the `db` host's JSON parameter encoding would be a
/// type negotiation for no gain. It must stay in step with
/// [`crate::model::SECURITY_EVENT_TYPES`], and a test asserts it does.
pub const SELECT_SECURITY_EVENTS: &str = "SELECT e.event_type, e.timestamp_epoch AS ts, \
     e.details::text AS details, d.mac \
     FROM ng_events e LEFT JOIN ng_devices d ON d.id = e.device_id \
     WHERE e.event_type IN \
       ('arp_scan', 'arp_spoof', 'gratuitous_arp', 'identity_change', 'ip_conflict', 'rogue_dhcp') \
       AND e.timestamp_epoch >= $1::bigint \
     ORDER BY e.timestamp_epoch DESC LIMIT $2::bigint";

/// How many security events since a cutoff. `$1` is the cutoff in unix seconds.
pub const SELECT_SECURITY_EVENT_COUNT: &str = "SELECT COUNT(*) AS event_count FROM ng_events \
     WHERE event_type IN \
       ('arp_scan', 'arp_spoof', 'gratuitous_arp', 'identity_change', 'ip_conflict', 'rogue_dhcp') \
       AND timestamp_epoch >= $1::bigint";

/// Presence spans overlapping a window, across every device. `$1` is the window
/// start, `$2` the end, `$3` the row limit.
///
/// **Overlapping**, not contained: a device that came online before the window
/// and was still online during it was online during it, and a containment test
/// would answer "nobody was home" for exactly the case somebody asks about. An
/// open span (`ended_at_epoch IS NULL`) overlaps any window that starts before
/// now, which is what makes "who is home right now" answerable.
///
/// Summary rows are excluded for the reason every other timeline excludes them:
/// a compacted day is not a session.
pub const SELECT_PRESENCE_WINDOW: &str = "SELECT d.mac, d.display_name, d.resolved_name, \
     d.hostname, d.mdns_name, d.device_type, d.state, \
     d.owner_item_id::text AS owner_item_id, \
     (SELECT p.name FROM ng_people p WHERE p.item_id = d.owner_item_id) AS owner_name, \
     s.started_at_epoch AS start, s.ended_at_epoch AS end \
     FROM ng_presence s JOIN ng_devices d ON d.id = s.device_id \
     WHERE s.is_summary = FALSE \
       AND s.started_at_epoch < $2::bigint \
       AND (s.ended_at_epoch IS NULL OR s.ended_at_epoch > $1::bigint) \
     ORDER BY s.started_at_epoch DESC LIMIT $3::bigint";

// ---------------------------------------------------------------------------
// The overview's questions, asked in conversation
// ---------------------------------------------------------------------------
//
// These three read the plugin's own views (007_netgrasp_overview_views.sql),
// not the daemon's tables, and that is the point of them: /overview and the
// network scope answer "who came home today" and "what is new this week" from
// the same definitions, so a page and a conversation cannot disagree about what
// "today" or "new" means. The views compute every time as a BIGINT, including
// the two ng_people arrival times the daemon stores with no epoch twin, so the
// rule above about reading twins holds here with nothing to alias.

/// The database's calendar day, which is what "today" means on the overview.
/// No parameter.
pub const SELECT_TODAY: &str = "SELECT to_char(now(), 'YYYY-MM-DD') AS today";

/// One day's arrivals and departures, oldest first. `$1` is the day as
/// `YYYY-MM-DD` text, `$2` the row limit.
///
/// `day` is text in the view, so the day binds as text: comparing it as a date
/// would need the view's column to be one, and the overview's include joins on
/// it as text.
pub const SELECT_MOVEMENTS_ON_DAY: &str = "SELECT event_type, timestamp_epoch AS ts, day, \
     person_item_id, person_name, location, via, device_mac, \
     device_display_name, device_resolved_name, device_hostname \
     FROM ng_person_movements WHERE day = $1::text \
     ORDER BY timestamp_epoch ASC LIMIT $2::bigint";

/// Everyone the daemon counts as home, earliest arrival first. No parameter:
/// a household has tens of people, and the overview lists all of them too.
pub const SELECT_PEOPLE_HOME: &str = "SELECT item_id::text AS item_id, name, current_location, \
     last_arrived_at_epoch AS arrived, devices_online \
     FROM ng_people_presence WHERE state = 'home' \
     ORDER BY last_arrived_at_epoch ASC NULLS LAST, name ASC";

/// Devices first seen in the last seven days, newest first, hidden ones left
/// out as the overview leaves them out. `$1` is the row limit.
pub const SELECT_NEW_DEVICES: &str = "SELECT id, mac, display_name, resolved_name, hostname, \
     mdns_name, vendor, device_type, device_type_confidence, os_family, \
     identity_source, state, last_ip, owner_item_id::text AS owner_item_id, owner_name, \
     first_seen_at_epoch AS first_seen, last_seen_at_epoch AS last_seen \
     FROM ng_devices_new WHERE NOT hidden \
     ORDER BY first_seen_at_epoch DESC LIMIT $1::bigint";

// ---------------------------------------------------------------------------
// People: the person page, the bulk form, merge (U2)
// ---------------------------------------------------------------------------

/// The devices the bulk form offers: nobody named them and nobody owns them,
/// hidden ones left out, online ones first. `$1` is the row limit.
///
/// The same definition as the To sort tab (`ng_devices_todo`, 010): no
/// `display_name`, no owner, not hidden. Online first because the device a
/// person is holding is the one they can identify.
pub const SELECT_DEVICES_TO_SORT: &str = device_read!(
    "WHERE d.display_name IS NULL AND d.owner_item_id IS NULL AND d.hidden = FALSE \
     ORDER BY (d.state = 'online') DESC, d.last_seen_at DESC NULLS LAST LIMIT $1::bigint"
);

/// One person's presence, for their page. `$1` is their Item id.
///
/// Read from the overview's view rather than from `ng_people`, because the view
/// carries the epoch twins of the two arrival times the mirror stores only as
/// `timestamptz`, which the `db` host decodes as null.
pub const SELECT_PERSON_PRESENCE: &str = "SELECT state, current_location, \
     last_arrived_at_epoch AS arrived, last_departed_at_epoch AS departed, devices_online \
     FROM ng_people_presence WHERE item_id = $1::uuid";

/// One person's comings and goings, newest first. `$1` is their Item id, `$2`
/// the row limit.
///
/// Through `ng_person_movements`, which since 014 attributes a merged person's
/// old rows to the person they were merged into, so the page of the person who
/// was kept shows both histories.
pub const SELECT_PERSON_MOVEMENTS: &str = "SELECT event_type, timestamp_epoch AS ts, day, \
     person_item_id, person_name, location, via, device_mac, \
     device_display_name, device_resolved_name, device_hostname \
     FROM ng_person_movements WHERE $1::uuid::text = person_item_id \
     ORDER BY timestamp_epoch DESC LIMIT $2::bigint";

/// How many comings and goings are one person's. `$1` is their Item id.
///
/// For the merge page, which says how much history moves with the merge.
pub const SELECT_PERSON_MOVEMENT_COUNT: &str = "SELECT count(*) AS movements \
     FROM ng_person_movements WHERE $1::uuid::text = person_item_id";

/// Record a merge, in one statement. `$1` is the person merged away, `$2` the
/// person kept, `$3` the merged person's name.
///
/// Three changes that must happen together or not at all, which is why they are
/// one statement: the `db` host runs each call in a transaction of its own, so
/// this is the only way three writes share one. Every data-modifying `WITH`
/// clause of a statement commits or rolls back with it.
///
/// - Earlier merges into the person going away are re-pointed at the person
///   kept, so the record stays one hop deep and the movements view needs one
///   join, however many merges came before.
/// - The merge itself is recorded, with the name the person had: the daemon's
///   events carry the old id, and this row is what attributes them.
/// - The merged person's mirror row is dropped, so the daemon stops counting
///   them on its next reconcile.
///
/// It returns the people whose earlier merges were re-pointed, as JSON text, so
/// a merge that fails later can put them back ([`UNDO_PERSON_MERGE`]).
///
/// `ng_events` is not in it. The daemon owns that table and its `details`
/// (`docs/UI.md`, People model), so a past arrival keeps the id and name it was
/// recorded with, and the view does the attributing.
pub const MERGE_PERSON_RECORD: &str = "WITH repointed AS (\
       UPDATE ng_person_merge SET into_item_id = $2::uuid \
       WHERE into_item_id = $1::uuid RETURNING from_item_id), \
     recorded AS (\
       INSERT INTO ng_person_merge (from_item_id, into_item_id, from_name, merged_at) \
       VALUES ($1::uuid, $2::uuid, $3::text, now()) \
       ON CONFLICT (from_item_id) DO UPDATE SET into_item_id = EXCLUDED.into_item_id, \
         from_name = EXCLUDED.from_name, merged_at = EXCLUDED.merged_at \
       RETURNING from_item_id), \
     retired AS (DELETE FROM ng_people WHERE item_id = $1::uuid RETURNING item_id) \
     SELECT COALESCE((SELECT json_agg(from_item_id::text) FROM repointed), '[]'::json)::text \
       AS repointed, \
       (SELECT count(*) FROM recorded) AS recorded, \
       (SELECT count(*) FROM retired) AS retired";

/// Take a merge record back, in one statement. `$1` is the person who was
/// merged away, `$2` the person kept, `$3` the JSON list
/// [`MERGE_PERSON_RECORD`] returned.
///
/// Only for a merge whose last step failed: the earlier merges it re-pointed go
/// back to the person they named, and its own row goes. The mirror row it
/// dropped is written again by the person mirror, from the Item that still
/// exists, because that is the one statement that writes a mirror row.
pub const UNDO_PERSON_MERGE: &str = "WITH restored AS (\
       UPDATE ng_person_merge SET into_item_id = $1::uuid \
       WHERE from_item_id::text IN (SELECT jsonb_array_elements_text($3::text::jsonb)) \
       RETURNING from_item_id) \
     DELETE FROM ng_person_merge WHERE from_item_id = $1::uuid AND into_item_id = $2::uuid";

// ===========================================================================
// Alerts (U3)
// ===========================================================================
//
// What the Alerts page reads, and the one row it writes. `ng_notify_status` and
// `ng_notify_requests` are the daemon's (netgraspd schema V4): the daemon is
// the only thing that sends anything, and these two tables are how the web side
// learns whether it can, and asks it to prove it. Neither holds the ntfy topic,
// the server or the token.
//
// Times of day are formatted by Postgres, in the session's time zone, because
// the plugin has no clock of its own beyond an epoch and no time zone database.
// `to_char` returns text, which the `db` host decodes; the timestamptz itself
// would arrive as null.

/// The daemon's one status row about delivery. No parameter.
///
/// No row at all means netgraspd has never reported: it is older than V4, or
/// it has not started since it was upgraded.
pub const SELECT_NOTIFY_STATUS: &str = "SELECT notify_configured, notify_backend, \
     last_delivery_ok, last_delivery_at_epoch AS last_delivery_at, \
     to_char(last_delivery_at, 'YYYY-MM-DD') AS last_delivery_day, \
     to_char(last_delivery_at, 'HH24:MI') AS last_delivery_clock, \
     updated_at_epoch AS updated_at \
     FROM ng_notify_status WHERE id = 1";

/// The most recent test request and what became of it. No parameter.
pub const SELECT_LATEST_TEST_REQUEST: &str = "SELECT id, requested_at_epoch AS requested_at, \
     handled_at_epoch AS handled_at, outcome, \
     to_char(requested_at, 'HH24:MI') AS requested_clock, \
     to_char(handled_at, 'YYYY-MM-DD') AS handled_day, \
     to_char(handled_at, 'HH24:MI') AS handled_clock \
     FROM ng_notify_requests ORDER BY id DESC LIMIT 1";

/// Ask the daemon for a test alert. `$1` is who asked (a user id, as text).
///
/// The plugin's only write to a daemon table here, and it is an insert into the
/// one table whose rows the plugin owns: `handled_at` and `outcome` are left for
/// the daemon, which answers on its reconcile tick.
///
/// No `RETURNING`: it runs through the `db` host's execute call, which counts
/// rows, and the host's query call refuses a write.
pub const INSERT_TEST_REQUEST: &str = "INSERT INTO ng_notify_requests (requested_by) \
     VALUES ($1::text)";

/// The alerts the daemon delivered, newest first: `ng_events.notified`, which
/// the daemon sets only once a notifier accepted the message. `$1` is the row
/// limit.
///
/// `person_name` prefers the person's current name, the way the movements view
/// does, and falls back to the one recorded on the event. `last_seen_clock` is
/// the departure's "last seen" time, from the daemon's `details.last_seen_at`,
/// read only when it looks like a timestamp so one malformed row cannot fail
/// the page.
pub const SELECT_RECENTLY_SENT: &str = "SELECT e.id, e.event_type, e.timestamp_epoch AS ts, \
     to_char(e.\"timestamp\", 'YYYY-MM-DD') AS day, \
     to_char(e.\"timestamp\", 'HH24:MI') AS clock, \
     COALESCE(p.name, e.details ->> 'person') AS person_name, \
     e.details ->> 'device_name' AS device_name, \
     CASE WHEN e.details ->> 'last_seen_at' ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T' \
          THEN to_char((e.details ->> 'last_seen_at')::timestamptz, 'HH24:MI') END AS last_seen_clock, \
     d.mac AS device_mac, d.display_name AS device_display_name, \
     d.resolved_name AS device_resolved_name, d.hostname AS device_hostname \
     FROM ng_events e \
     LEFT JOIN ng_devices d ON d.id = e.device_id \
     LEFT JOIN ng_people p ON p.item_id::text = e.details ->> 'person_item_id' \
     WHERE e.notified \
     ORDER BY e.\"timestamp\" DESC LIMIT $1::bigint";

/// How many devices there are, and how many of them alert when they come back
/// online. Hidden devices are counted in neither. No parameter.
pub const SELECT_ALERTING_COUNTS: &str = "SELECT \
     count(*) FILTER (WHERE notify) AS alerting, count(*) AS devices \
     FROM ng_devices WHERE NOT hidden";

/// The devices that alert, grouped by owner (people first, by name) and then by
/// name. `$1` is the row limit.
pub const SELECT_DEVICES_ALERTING: &str = device_read!(
    "WHERE d.notify AND NOT d.hidden \
     ORDER BY owner_name NULLS LAST, \
              lower(COALESCE(d.display_name, d.resolved_name, d.hostname, d.mac)) \
     LIMIT $1::bigint"
);

/// Every device the bulk alerts form lists: all but the hidden ones, the same
/// order. `$1` is the row limit.
pub const SELECT_DEVICES_FOR_ALERTS: &str = device_read!(
    "WHERE NOT d.hidden \
     ORDER BY owner_name NULLS LAST, \
              lower(COALESCE(d.display_name, d.resolved_name, d.hostname, d.mac)) \
     LIMIT $1::bigint"
);

/// Every statement above, for the tests that check them as a set.
pub const ALL: &[(&str, &str)] = &[
    ("SELECT_DIRTY_DEVICES", SELECT_DIRTY_DEVICES),
    ("UPDATE_LINK_ITEM", UPDATE_LINK_ITEM),
    ("UPDATE_MARK_CLEAN", UPDATE_MARK_CLEAN),
    ("SELECT_DAEMON_TITLE_FIELDS", SELECT_DAEMON_TITLE_FIELDS),
    ("UPDATE_CLEAR_OWNER", UPDATE_CLEAR_OWNER),
    ("DELETE_PERSON_MIRROR", DELETE_PERSON_MIRROR),
    ("UPDATE_UNLINK_DEVICE", UPDATE_UNLINK_DEVICE),
    ("DELETE_EXPIRED_EVENTS", DELETE_EXPIRED_EVENTS),
    ("SELECT_DEVICE_STATE", SELECT_DEVICE_STATE),
    ("SELECT_PRESENCE_SPANS", SELECT_PRESENCE_SPANS),
    ("SELECT_LOCATION_SPANS", SELECT_LOCATION_SPANS),
    ("SELECT_ADDRESS_SPANS", SELECT_ADDRESS_SPANS),
    ("SELECT_OWNER_NAME", SELECT_OWNER_NAME),
    ("SELECT_CLOCK", SELECT_CLOCK),
    ("SELECT_MONITORING_START", SELECT_MONITORING_START),
    // The assistant scopes.
    ("SELECT_DEVICE_BY_MAC", SELECT_DEVICE_BY_MAC),
    ("SELECT_DEVICE_BY_ID", SELECT_DEVICE_BY_ID),
    ("SELECT_DEVICE_BY_ITEM", SELECT_DEVICE_BY_ITEM),
    ("SELECT_DEVICES_ALL", SELECT_DEVICES_ALL),
    ("SELECT_DEVICES_BY_OWNER", SELECT_DEVICES_BY_OWNER),
    ("SELECT_DEVICES_UNOWNED", SELECT_DEVICES_UNOWNED),
    ("SELECT_DEVICES_BY_STATE", SELECT_DEVICES_BY_STATE),
    ("SELECT_DEVICES_HIDDEN", SELECT_DEVICES_HIDDEN),
    ("SELECT_DEVICES_BY_NAME", SELECT_DEVICES_BY_NAME),
    ("SELECT_PEOPLE_WITH_COUNTS", SELECT_PEOPLE_WITH_COUNTS),
    ("SELECT_PERSON_WITH_COUNT", SELECT_PERSON_WITH_COUNT),
    ("SELECT_OWNED_DEVICE_COUNT", SELECT_OWNED_DEVICE_COUNT),
    ("SELECT_DEVICE_EVENTS", SELECT_DEVICE_EVENTS),
    ("SELECT_SECURITY_EVENTS", SELECT_SECURITY_EVENTS),
    ("SELECT_SECURITY_EVENT_COUNT", SELECT_SECURITY_EVENT_COUNT),
    ("SELECT_PRESENCE_WINDOW", SELECT_PRESENCE_WINDOW),
    ("SELECT_TODAY", SELECT_TODAY),
    ("SELECT_MOVEMENTS_ON_DAY", SELECT_MOVEMENTS_ON_DAY),
    ("SELECT_PEOPLE_HOME", SELECT_PEOPLE_HOME),
    ("SELECT_NEW_DEVICES", SELECT_NEW_DEVICES),
    // People (U2).
    ("SELECT_DEVICES_TO_SORT", SELECT_DEVICES_TO_SORT),
    ("SELECT_PERSON_PRESENCE", SELECT_PERSON_PRESENCE),
    ("SELECT_PERSON_MOVEMENTS", SELECT_PERSON_MOVEMENTS),
    ("SELECT_PERSON_MOVEMENT_COUNT", SELECT_PERSON_MOVEMENT_COUNT),
    ("MERGE_PERSON_RECORD", MERGE_PERSON_RECORD),
    ("UNDO_PERSON_MERGE", UNDO_PERSON_MERGE),
    // Alerts (U3).
    ("SELECT_NOTIFY_STATUS", SELECT_NOTIFY_STATUS),
    ("SELECT_LATEST_TEST_REQUEST", SELECT_LATEST_TEST_REQUEST),
    ("INSERT_TEST_REQUEST", INSERT_TEST_REQUEST),
    ("SELECT_RECENTLY_SENT", SELECT_RECENTLY_SENT),
    ("SELECT_ALERTING_COUNTS", SELECT_ALERTING_COUNTS),
    ("SELECT_DEVICES_ALERTING", SELECT_DEVICES_ALERTING),
    ("SELECT_DEVICES_FOR_ALERTS", SELECT_DEVICES_FOR_ALERTS),
];

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The columns the plugin was written against before it was reconciled with
    /// the daemon's landed schema. None of them exists; a query naming one fails
    /// at runtime against a real daemon database and nowhere else.
    const COLUMNS_THAT_DO_NOT_EXIST: &[&str] = &["start_time", "end_time", "ip_address"];

    #[test]
    fn no_statement_names_a_column_the_daemon_does_not_have() {
        for (name, sql) in ALL {
            for absent in COLUMNS_THAT_DO_NOT_EXIST {
                assert!(
                    !sql.contains(absent),
                    "{name} names '{absent}', which is not a column of the daemon's schema"
                );
            }
        }
    }

    /// Every `timestamptz` column of the daemon's schema. Selecting one decodes
    /// as `null` through the `db` host, so the plugin reads its `_epoch` twin
    /// and aliases the twin back to the name the row struct expects — which is
    /// why this compares the *expression* of each select item, not the alias.
    const TIMESTAMPTZ_COLUMNS: &[&str] = &[
        "first_seen_at",
        "last_seen_at",
        "started_at",
        "ended_at",
        "first_seen",
        "last_seen",
        "\"timestamp\"",
        "last_arrived_at",
        "last_departed_at",
        // netgraspd V4: the notify status and test request tables.
        "last_delivery_at",
        "updated_at",
        "requested_at",
        "handled_at",
    ];

    /// The select items of a `SELECT`, as `(expression, alias)` pairs. A bare
    /// timestamp may still be **ordered** by, so only the projection is checked.
    fn select_items(sql: &str) -> Vec<&str> {
        let Some(rest) = sql.strip_prefix("SELECT ") else {
            return Vec::new();
        };
        let projection = rest.split(" FROM ").next().unwrap_or(rest);
        projection
            .split(',')
            .map(|item| item.split(" AS ").next().unwrap_or(item).trim())
            .collect()
    }

    #[test]
    fn no_statement_selects_a_bare_timestamp_column() {
        for (name, sql) in ALL {
            for expr in select_items(sql) {
                assert!(
                    !TIMESTAMPTZ_COLUMNS.contains(&expr),
                    "{name} selects the timestamptz column '{expr}' rather than its epoch twin"
                );
            }
        }
    }

    /// The other half of the same property: the statements that read a time do
    /// read one, rather than having quietly dropped the column.
    #[test]
    fn the_span_statements_select_an_epoch_twin_for_both_ends() {
        for (name, sql) in [
            ("presence", SELECT_PRESENCE_SPANS),
            ("location", SELECT_LOCATION_SPANS),
            ("address", SELECT_ADDRESS_SPANS),
        ] {
            let items = select_items(sql);
            let epochs = items.iter().filter(|e| e.ends_with("_epoch")).count();
            assert_eq!(
                epochs, 2,
                "the {name} timeline selects {epochs} epoch twins"
            );
        }
    }

    /// The other side of the same coin: an Item link is a uuid wherever it is
    /// bound. `owner_item_id`, `trovato_item_id` and `ng_people.item_id` all
    /// hold a kernel `item.id`, and binding one as text raises
    /// `operator does not exist: uuid = text` at runtime.
    #[test]
    fn every_bound_item_link_is_cast_to_uuid() {
        for (name, sql) in ALL {
            for column in ["owner_item_id", "trovato_item_id", "item_id"] {
                let mut from = 0;
                while let Some(at) = sql[from..].find(&format!("{column} = $")) {
                    let tail = &sql[from + at..];
                    // `= NULL` and `= EXCLUDED.…` are not bindings; the search
                    // pattern already excludes them by requiring a `$`.
                    assert!(
                        tail.split_whitespace()
                            .nth(2)
                            .is_some_and(|p| p.starts_with("$") && p.contains("::uuid")),
                        "{name} binds {column} without a ::uuid cast: {tail}"
                    );
                    from += at + column.len();
                }
            }
        }
    }

    /// A device id is a bigint. Casting one `::uuid` is the error this whole
    /// reconciliation existed to remove, and it is invisible until a real
    /// daemon row is in front of it.
    #[test]
    fn no_statement_casts_a_device_id_to_uuid() {
        for (name, sql) in ALL {
            assert!(
                !sql.contains("device_id = $1::uuid"),
                "{name} binds a device id as a uuid"
            );
            assert!(
                !sql.contains("WHERE id = $1::uuid") && !sql.contains("WHERE id = $2::uuid"),
                "{name} binds a device primary key as a uuid"
            );
        }
    }

    /// Everything a value could reach is a placeholder: no statement here is
    /// built by interpolation, `raw_sql` capability notwithstanding.
    #[test]
    fn every_statement_is_parameterized_or_takes_no_parameter() {
        for (name, sql) in ALL {
            assert!(!sql.contains(';'), "{name} contains a statement separator");
            assert!(
                !sql.contains("{}") && !sql.contains("{ }"),
                "{name} looks like a format template"
            );
        }
    }

    /// The security statements spell their type list inline, and it is the one
    /// list in this file that is not bound. It has to be the same set the rest
    /// of the plugin calls a security event, and nothing else would say so: a
    /// drifted list is a page that quietly under-reports, which is exactly the
    /// defect migration 004 exists to correct.
    #[test]
    fn the_inline_security_type_list_is_the_one_the_model_declares() {
        for (name, sql) in [
            ("SELECT_SECURITY_EVENTS", SELECT_SECURITY_EVENTS),
            ("SELECT_SECURITY_EVENT_COUNT", SELECT_SECURITY_EVENT_COUNT),
        ] {
            for event_type in crate::model::SECURITY_EVENT_TYPES {
                assert!(
                    sql.contains(&format!("'{event_type}'")),
                    "{name} omits the security event type {event_type}"
                );
            }
            // And nothing beyond it: an `IN` list with a type the daemon never
            // writes is the defect in the other direction. Located from
            // `event_type IN` rather than from `IN`, which also occurs inside
            // `JOIN`.
            let list = sql
                .split("event_type IN")
                .nth(1)
                .and_then(|rest| rest.split(')').next())
                .unwrap_or_default();
            let quoted = list.matches('\'').count() / 2;
            assert_eq!(
                quoted,
                crate::model::SECURITY_EVENT_TYPES.len(),
                "{name} lists {quoted} event types, the model names {}",
                crate::model::SECURITY_EVENT_TYPES.len()
            );
        }
    }

    /// Every assistant device read projects the same columns, because one row
    /// struct decodes all of them. The macro makes that true; this says so, so
    /// that a statement written by hand later cannot quietly opt out.
    #[test]
    fn every_assistant_device_read_projects_the_same_columns() {
        let statements = [
            ("SELECT_DEVICE_BY_MAC", SELECT_DEVICE_BY_MAC),
            ("SELECT_DEVICE_BY_ID", SELECT_DEVICE_BY_ID),
            ("SELECT_DEVICE_BY_ITEM", SELECT_DEVICE_BY_ITEM),
            ("SELECT_DEVICES_ALL", SELECT_DEVICES_ALL),
            ("SELECT_DEVICES_BY_OWNER", SELECT_DEVICES_BY_OWNER),
            ("SELECT_DEVICES_UNOWNED", SELECT_DEVICES_UNOWNED),
            ("SELECT_DEVICES_BY_STATE", SELECT_DEVICES_BY_STATE),
            ("SELECT_DEVICES_HIDDEN", SELECT_DEVICES_HIDDEN),
            ("SELECT_DEVICES_BY_NAME", SELECT_DEVICES_BY_NAME),
            ("SELECT_DEVICES_TO_SORT", SELECT_DEVICES_TO_SORT),
        ];
        let projection_of = |sql: &str| -> String {
            sql.split(" FROM ng_devices d")
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let first = projection_of(statements[0].1);
        for (name, sql) in &statements[1..] {
            assert_eq!(projection_of(sql), first, "{name} projects something else");
        }
        for column in [
            "owner_item_id",
            "trovato_item_id",
            "owner_name",
            "first_seen",
            "last_seen",
            "hidden",
            "notify",
        ] {
            assert!(
                first.contains(column),
                "the shared projection omits {column}"
            );
        }
    }

    #[test]
    fn the_timeline_statements_read_only_observed_rows() {
        for sql in [SELECT_PRESENCE_SPANS, SELECT_LOCATION_SPANS] {
            assert!(
                sql.contains("is_summary = FALSE"),
                "a timeline statement admits compacted summary rows: {sql}"
            );
        }
    }
}
