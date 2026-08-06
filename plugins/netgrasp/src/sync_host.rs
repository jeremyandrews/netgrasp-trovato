//! The two sync directions, executed against the kernel hosts.
//!
//! The *decisions* live in [`netgrasp_core::sync`] and
//! [`netgrasp_core::writeback`]; this module is the part that cannot be tested
//! without a host, kept as thin as it can be for exactly that reason.
//!
//! The **statements** live in [`netgrasp_core::queries`] for the same reason,
//! and it is a stronger one than tidiness: the SQL here is written against a
//! schema the netgrasp daemon owns, nothing on this side type-checks it, and a
//! wrong column name is invisible until a real daemon database is in front of
//! it. Hoisting the statements into the core is what lets
//! `crates/netgrasp-core/tests/daemon_schema_test.rs` run *these* statements —
//! not restatements of them — against the daemon's own DDL.
//!
//! # Chunking
//!
//! Two ceilings bound a sync pass and neither is the one the scope names.
//!
//! The **64 KB tap I/O buffer** applies to a tap's *return value*, and no device
//! ever crosses it: rows are read through the `db` host and Items are written
//! through `item-api`, so the tap boundary only ever carries
//! [`SyncReport`]'s handful of integers.
//!
//! The ceiling that does bind is the SDK's **256 KB output buffer** on
//! `query_raw` (`MAX_OUTPUT_BUFFER` in `crates/plugin-sdk/src/host.rs`), which a
//! large dirty set would overflow. [`MAX_DEVICES_PER_TICK`] is set so a page of
//! device rows cannot approach it, and the remainder is left `dirty` for the
//! next tick — the same round-robin drain Argus uses for feeds, and the reason
//! the pass needs no queue.
//!
//! The **150 s background epoch** is the third bound, and the same page size
//! serves it: one `get-item` plus at most one `save-item` per row.

use netgrasp_core::model::{DeviceRow, Span, SpanRow};
use netgrasp_core::queries;
use netgrasp_core::sync::{SyncAction, daemon_title, plan};
use netgrasp_core::writeback::{Statement, build_person_upsert, build_update, overlay_from_item};
use netgrasp_core::{CoreError, CoreResult, DEVICE_TYPE, retention};
use serde::Deserialize;
use serde_json::{Value, json};
use trovato_sdk::host;

use crate::db::{exec, query_rows};
use crate::item_host;

/// Device rows processed per cron tick.
///
/// Sized against the SDK's 256 KB `query_raw` output buffer: a device row
/// serializes to a few hundred bytes, so 200 rows is well inside it with room for
/// a pathological hostname. A larger dirty set drains over successive ticks.
pub const MAX_DEVICES_PER_TICK: i64 = 200;

/// Site variable naming the event retention window, in days.
pub const VAR_RETENTION_DAYS: &str = "netgrasp_event_retention_days";

/// What one sync pass did. Small by construction: this is the value that crosses
/// the 64 KB tap boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SyncReport {
    /// Dirty rows examined.
    pub examined: usize,
    /// Device Items created.
    pub created: usize,
    /// Device Items re-created because the row named a deleted one.
    pub relinked: usize,
    /// Device Items whose derived title was refreshed.
    pub refreshed: usize,
    /// Rows already correct — cleared without a write.
    pub skipped: usize,
    /// Rows left `dirty` because their own handling failed.
    pub failed: usize,
    /// Whether more dirty rows remain for the next tick.
    pub more_pending: bool,
}

/// Run the daemon→kernel pass.
///
/// Per-row failures are counted and skipped rather than propagated: one device
/// with a malformed row must not stop the other 199, and leaving it `dirty` means
/// the next tick retries it for free.
///
/// # Errors
///
/// [`CoreError::Store`] only when the dirty set itself cannot be read — the one
/// failure that makes the whole pass meaningless.
pub fn sync_devices() -> CoreResult<SyncReport> {
    let rows: Vec<DeviceRow> = query_rows(
        queries::SELECT_DIRTY_DEVICES,
        &[json!(MAX_DEVICES_PER_TICK)],
    )?;

    let mut report = SyncReport {
        examined: rows.len(),
        more_pending: rows.len() as i64 == MAX_DEVICES_PER_TICK,
        ..SyncReport::default()
    };

    for row in &rows {
        match sync_one(row) {
            Ok(SyncAction::Create { .. }) => report.created += 1,
            Ok(SyncAction::Relink { .. }) => report.relinked += 1,
            Ok(SyncAction::Refresh { .. }) => report.refreshed += 1,
            Ok(SyncAction::Skip { .. }) => report.skipped += 1,
            Err(e) => {
                report.failed += 1;
                host::log(
                    "warning",
                    "netgrasp",
                    &format!("sync: device {} left dirty: {e}", row.mac),
                );
            }
        }
    }

    Ok(report)
}

/// Handle one dirty row, returning the action taken.
///
/// Ordering matters and is the idempotency argument: the Item is written (or
/// confirmed) **before** `sync_state` is cleared, so a pass that dies between the
/// two leaves the row `dirty` and the next pass redoes it. Redoing it is free
/// because every step is an upsert or a no-op — a row whose Item already carries
/// the derived title plans as [`SyncAction::Skip`].
fn sync_one(row: &DeviceRow) -> CoreResult<SyncAction> {
    let linked = row.trovato_item_id.as_deref().unwrap_or_default();
    let existing = if linked.is_empty() {
        None
    } else {
        load_item(linked)?
    };
    let current_title = existing
        .as_ref()
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str);

    let action = plan(row, existing.is_some(), current_title);

    match &action {
        SyncAction::Create { title } | SyncAction::Relink { title, .. } => {
            let item_id = create_device_item(&row.mac, title)?;
            link_item(row.id, &item_id)?;
        }
        SyncAction::Refresh { item_id, title } => refresh_title(item_id, title)?,
        SyncAction::Skip { .. } => {}
    }

    mark_clean(row.id)?;
    Ok(action)
}

/// Load an Item, mapping "no such Item" to `None`.
fn load_item(id: &str) -> CoreResult<Option<Value>> {
    match item_host::get_item(id) {
        Ok(Value::Null) => Ok(None),
        Ok(v) => Ok(Some(v)),
        Err(code) => Err(CoreError::Item(format!("get-item {id}: host error {code}"))),
    }
}

/// Create a device Item and return its id.
///
/// The Item carries the MAC and nothing else the daemon owns: every other field
/// is the admin's to fill in (`DESIGN.md` Decision 1). Created **unpublished**
/// (`status: 0`) is deliberately *not* done — a device the daemon just found
/// should appear in the lists immediately, and an admin hides it with the
/// `hidden` field rather than by unpublishing.
fn create_device_item(mac: &str, title: &str) -> CoreResult<String> {
    let payload = json!({
        "type": DEVICE_TYPE,
        "title": title,
        "status": 1,
        "fields": { "field_mac": mac },
    });
    let saved = item_host::save_item(&payload)
        .map_err(|code| CoreError::Item(format!("save-item create: host error {code}")))?;
    saved
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::Item("save-item returned no id".into()))
}

/// Refresh a device Item's derived title.
///
/// The payload carries **no `fields` key**, which `Item::update` reads as "leave
/// the fields alone" (`input.fields.unwrap_or(current.fields)`). That is what
/// makes this safe to run against an Item an admin is editing: the sync cannot
/// clobber a user-owned value because it never sends one.
fn refresh_title(item_id: &str, title: &str) -> CoreResult<()> {
    let payload = json!({ "id": item_id, "title": title });
    item_host::save_item(&payload)
        .map(|_| ())
        .map_err(|code| CoreError::Item(format!("save-item refresh: host error {code}")))
}

/// Point a device row at its Item.
///
/// Writes `trovato_item_id` only — a link-owned column, so this touches neither
/// the daemon's nor the user's set. The two ids are different types and both
/// casts matter: the Item id is a uuid, the device id is a bigint.
fn link_item(device_id: i64, item_id: &str) -> CoreResult<()> {
    exec(
        queries::UPDATE_LINK_ITEM,
        &[json!(item_id), json!(device_id)],
    )?;
    Ok(())
}

/// Lower the daemon's dirty flag for one row.
///
/// The one statement in the plugin that writes a daemon-owned column, and it is
/// deliberately its own function so that the write-back cannot reach it: the
/// write-back builds its statement from
/// [`netgrasp_core::columns::USER_OWNED`] and `sync_state` is not in that set.
fn mark_clean(device_id: i64) -> CoreResult<()> {
    exec(queries::UPDATE_MARK_CLEAN, &[json!(device_id)])?;
    Ok(())
}

// ===========================================================================
// Kernel → daemon
// ===========================================================================

/// Write an admin's device edit back to the daemon's table.
///
/// The statement is built by [`build_update`] from the user-owned column list, so
/// this function has no opportunity to name a daemon column even by accident. It
/// returns the rows it touched: zero is normal and not an error — a device Item
/// whose row the daemon has since deleted, or one created by hand before any row
/// existed, simply matches nothing.
///
/// # Errors
///
/// [`CoreError::Invalid`] when the Item payload has no title;
/// [`CoreError::Store`] when the update fails.
pub fn write_back_device(item: &Value) -> CoreResult<u64> {
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Invalid("device item has no id".into()))?;
    let overlay = overlay_from_item(item)?;

    // What the daemon alone would call this device. Read here rather than
    // computed in SQL so there is one definition of it (`daemon_title`) rather
    // than a Rust one and a `CASE` expression that can drift apart. A failed
    // read degrades to `None`, which stores the title unconditionally — losing
    // hostname tracking rather than losing a name a human typed.
    let fallback = daemon_fallback_title(item_id).unwrap_or_else(|e| {
        host::log(
            "warning",
            "netgrasp",
            &format!("write-back: daemon title for {item_id}: {e}"),
        );
        None
    });

    let Statement { sql, params, .. } = build_update(item_id, &overlay, fallback.as_deref())?;
    exec(&sql, &params)
}

/// The title the daemon's own observations imply for the device behind an Item.
///
/// `None` when no daemon row is linked — an admin-created device Item with
/// nothing on the network behind it yet.
fn daemon_fallback_title(item_id: &str) -> CoreResult<Option<String>> {
    #[derive(Deserialize)]
    struct FallbackRow {
        mac: String,
        hostname: Option<String>,
        vendor: Option<String>,
    }
    let rows: Vec<FallbackRow> =
        query_rows(queries::SELECT_DAEMON_TITLE_FIELDS, &[json!(item_id)])?;
    Ok(rows.into_iter().next().map(|r| {
        // Id 0 is never a real identity value; nothing derived from this probe
        // reads it, and `daemon_title` looks only at mac, hostname and vendor.
        let mut probe = DeviceRow::new(0, r.mac);
        probe.hostname = r.hostname;
        probe.vendor = r.vendor;
        daemon_title(&probe)
    }))
}

/// Mirror a person Item into `ng_people` for the daemon to read.
///
/// # Errors
///
/// [`CoreError::Invalid`] when the payload has no id or title;
/// [`CoreError::Store`] when the upsert fails.
pub fn mirror_person(item: &Value) -> CoreResult<u64> {
    let Statement { sql, params, .. } = build_person_upsert(item)?;
    exec(&sql, &params)
}

/// Retire a person: drop the mirror row and unassign their devices.
///
/// Devices are unassigned rather than deleted — the device is still on the
/// network, it just has no owner any more. This is the one place the plugin
/// writes a user-owned column outside the write-back, and it writes exactly one
/// (`owner_item_id`), because a dangling `owner_item_id` would make the
/// by-owner gather return rows for a person who no longer exists.
///
/// # Errors
///
/// [`CoreError::Store`] when either statement fails.
pub fn retire_person(item_id: &str) -> CoreResult<()> {
    exec(queries::UPDATE_CLEAR_OWNER, &[json!(item_id)])?;
    exec(queries::DELETE_PERSON_MIRROR, &[json!(item_id)])?;
    Ok(())
}

/// Unlink a device row whose Item an admin deleted.
///
/// The row is left alone otherwise: the device is still on the network and the
/// daemon still owns the row. Clearing the link is what lets the next sync pass
/// notice it and mint a fresh Item — so deleting a device Item is a "forget my
/// edits and start over", not a "stop tracking this device".
///
/// The row is also marked `dirty` so that pass happens on the next tick rather
/// than whenever the daemon next touches the device. This is the one place
/// anything but the daemon raises the flag, and it is safe for the reason the
/// write-back is not allowed to do it: it happens on a *delete*, which cannot
/// recur, so it cannot produce a cycle.
///
/// # Errors
///
/// [`CoreError::Store`] when the update fails.
pub fn unlink_device(item_id: &str) -> CoreResult<u64> {
    exec(queries::UPDATE_UNLINK_DEVICE, &[json!(item_id)])
}

// ===========================================================================
// Retention
// ===========================================================================

/// Delete events past their retention window.
///
/// Bounded per tick ([`retention::PRUNE_BATCH`]) so a long-neglected install
/// drains over successive ticks instead of timing out forever against the `db`
/// host's 5 s `statement_timeout`.
///
/// # Errors
///
/// [`CoreError::Store`] when the delete fails.
pub fn prune_events(now: i64) -> CoreResult<u64> {
    let days = configured_retention_days();
    let cutoff = retention::cutoff(now, days);
    exec(
        queries::DELETE_EXPIRED_EVENTS,
        &[json!(cutoff), json!(retention::PRUNE_BATCH)],
    )
}

/// The retention window, from site config, clamped.
///
/// An unparseable or absent value falls back to the default rather than
/// disabling pruning: a site whose variable is a typo should keep 90 days of
/// events, not an unbounded log.
fn configured_retention_days() -> i64 {
    let raw = host::variables_get(
        VAR_RETENTION_DAYS,
        &retention::DEFAULT_RETENTION_DAYS.to_string(),
    )
    .unwrap_or_else(|_| retention::DEFAULT_RETENTION_DAYS.to_string());
    retention::clamp_days(
        raw.trim()
            .parse::<i64>()
            .unwrap_or(retention::DEFAULT_RETENTION_DAYS),
    )
}

// ===========================================================================
// Device page reads
// ===========================================================================

/// The daemon-owned state a device page shows above its timelines.
///
/// Defined in [`netgrasp_core::model`] with the query that fills it, and
/// re-exported here because this is where the plugin reads it.
pub use netgrasp_core::model::DeviceState;

/// Load the daemon's row for a device Item, if the sync has linked one.
///
/// # Errors
///
/// [`CoreError::Store`] when the query fails.
pub fn load_device_state(item_id: &str) -> CoreResult<Option<DeviceState>> {
    let rows: Vec<DeviceState> = query_rows(queries::SELECT_DEVICE_STATE, &[json!(item_id)])?;
    Ok(rows.into_iter().next())
}

/// The rows a device page's three timelines are built from.
pub struct DeviceHistory {
    /// Presence sessions.
    pub presence: Vec<Span>,
    /// Location stays, labelled by access point.
    pub locations: Vec<Span>,
    /// Address holdings, labelled by IP.
    pub addresses: Vec<Span>,
}

/// Rows fetched per timeline.
///
/// More than [`netgrasp_core::timeline::MAX_SPANS`] shows, so the page can say
/// truthfully that it truncated, and bounded well below the 5 s statement
/// timeout and the 256 KB output buffer.
const HISTORY_LIMIT: i64 = 100;

/// Load a device's presence, location and address history.
///
/// Three queries rather than one union: they are three tables with three
/// indexes, and a union would defeat all three. Each reads the epoch twins of
/// its interval columns, because a `timestamptz` decodes as `null` through the
/// `db` host — the reason every one of these timelines was empty against a real
/// daemon database before the schemas were reconciled.
///
/// # Errors
///
/// [`CoreError::Store`] when any of the three queries fails.
pub fn load_device_history(device_id: i64) -> CoreResult<DeviceHistory> {
    let presence: Vec<SpanRow> = query_rows(
        queries::SELECT_PRESENCE_SPANS,
        &[json!(device_id), json!(HISTORY_LIMIT)],
    )?;
    let locations: Vec<SpanRow> = query_rows(
        queries::SELECT_LOCATION_SPANS,
        &[json!(device_id), json!(HISTORY_LIMIT)],
    )?;
    let addresses: Vec<SpanRow> = query_rows(
        queries::SELECT_ADDRESS_SPANS,
        &[json!(device_id), json!(HISTORY_LIMIT)],
    )?;

    Ok(DeviceHistory {
        presence: presence.into_iter().map(Span::from).collect(),
        locations: locations.into_iter().map(Span::from).collect(),
        addresses: addresses.into_iter().map(Span::from).collect(),
    })
}

/// The owner's name for a device Item, if it has one.
///
/// Read from the `ng_people` mirror rather than through `get-item`, because the
/// mirror is a plain indexed table and the device page is a read path.
///
/// # Errors
///
/// [`CoreError::Store`] when the query fails.
pub fn load_owner_name(owner_item_id: &str) -> CoreResult<Option<String>> {
    #[derive(Deserialize)]
    struct NameRow {
        name: String,
    }
    let rows: Vec<NameRow> = query_rows(queries::SELECT_OWNER_NAME, &[json!(owner_item_id)])?;
    Ok(rows.into_iter().next().map(|r| r.name))
}
