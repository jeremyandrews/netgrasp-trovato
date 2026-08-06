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
pub const SELECT_DIRTY_DEVICES: &str = "SELECT id, mac, hostname, vendor, device_type, os_family, \
     state, last_ip, current_location, first_seen_at_epoch AS first_seen, \
     last_seen_at_epoch AS last_seen, display_name, trovato_item_id \
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
pub const SELECT_DAEMON_TITLE_FIELDS: &str =
    "SELECT mac, hostname, vendor FROM ng_devices WHERE trovato_item_id = $1::uuid LIMIT 1";

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
     state, last_ip, current_location, first_seen_at_epoch AS first_seen, \
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
