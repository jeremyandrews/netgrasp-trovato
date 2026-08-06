//! Netgrasp plugin for Trovato: the UI and management surface over the native
//! netgrasp daemon's `ng_` tables, built as a pure WASM plugin.
//!
//! The daemon watches a LAN and writes what it sees. This plugin makes those
//! rows into things a person can look at and edit — a dashboard, gathers, roles,
//! and a device page with presence and location timelines — and writes the
//! person's edits back for the daemon to act on.
//!
//! # The shape, in one paragraph
//!
//! A device is **two tiers**: the daemon-owned row in `ng_devices` (a lightweight
//! record, for gathers and operational inspection) and a user-owned `ng_device`
//! Item (for editing, because a record has no write surface). Their columns are
//! disjoint sets named once in [`netgrasp_core::columns`]. Events are a record
//! type — high volume, never edited, pruned wholesale. Presence, location and
//! address history stay daemon tables and are rendered onto the device page.
//! People are Items, mirrored into `ng_people` so the daemon never reads the
//! kernel's `item` table. The reasoning for every one of those is in
//! `DESIGN.md`; the kernel behaviours that forced them are in `FRICTION.md`.
//!
//! # Direction of travel
//!
//! - **daemon → kernel** is [`tap_cron`]: dirty rows get device Items, derived
//!   titles are refreshed, expired events are pruned.
//! - **kernel → daemon** is [`tap_item_update`] (and [`tap_item_insert`] /
//!   [`tap_item_delete`]): an admin's edit writes the user-owned columns and
//!   nothing else.
//!
//! The loop between them terminates twice over — see
//! [`netgrasp_core::writeback`] and `DESIGN.md` Decision 4.

use netgrasp_core::{DEVICE_TYPE, PERSON_TYPE};
use trovato_sdk::host;
use trovato_sdk::prelude::*;

mod db;
mod device_view;
mod item_host;
mod sync_host;

/// Permission to manage devices, people and plugin configuration.
pub const PERM_ADMINISTER: &str = "administer netgrasp";

/// Permission to see the device lists and the device page.
pub const PERM_VIEW_DEVICES: &str = "view netgrasp devices";

// ===========================================================================
// Content types, permissions, menu
// ===========================================================================

/// The two Item content types Netgrasp declares.
///
/// Both hold **only** what a person edits. Everything the daemon observes is a
/// record type declared in `netgrasp.info.toml`, not a field here — a device's
/// `state` changes every time it is seen, and an Item field would mean an
/// `item_revision` row per sighting (`DESIGN.md` Decision 1).
///
/// `field_owner` is a plain `Text` uuid rather than a
/// [`FieldType::RecordReference`]: the kernel's reference widget writes a bare id
/// into the hidden input but re-reads `{"target_id": …}` on edit
/// (`static/js/record-ref.js` vs `crates/kernel/src/content/form.rs`), so a saved
/// reference does not survive an edit. Losing a device's owner every time an
/// admin fixes a typo in its notes is worse than making them paste an id.
/// `G-ITEM-FORM-MISMATCH`. This reverses the skeleton, which declared
/// `owner_id` as a `RecordReference`; nothing had ever been stored against it.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: DEVICE_TYPE.into(),
            label: "Device".into(),
            description: "A device on the network. Its name, owner and notes are yours to \
                          edit; everything else is what the netgrasp daemon observed."
                .into(),
            title_label: Some("Device name".into()),
            fields: vec![
                // The daemon's identity for the device, echoed onto the Item so
                // the Item is self-describing and gatherable on its own. Written
                // once at creation and never again — the sync's refresh sends no
                // `fields` key at all.
                FieldDefinition::new(
                    "field_mac",
                    FieldType::Text {
                        max_length: Some(64),
                    },
                )
                .required()
                .label("MAC Address"),
                FieldDefinition::new(
                    "field_owner",
                    FieldType::Text {
                        max_length: Some(64),
                    },
                )
                .label("Owner (person item id)"),
                FieldDefinition::new("field_notes", FieldType::TextLong).label("Notes"),
                FieldDefinition::new("field_hidden", FieldType::Boolean)
                    .label("Hide from device lists"),
                FieldDefinition::new("field_notify", FieldType::Boolean)
                    .label("Notify on arrival and departure"),
            ],
        },
        ContentTypeDefinition {
            machine_name: PERSON_TYPE.into(),
            label: "Person".into(),
            description: "Someone devices can belong to.".into(),
            title_label: Some("Name".into()),
            fields: vec![
                FieldDefinition::new("field_notes", FieldType::TextLong).label("Notes"),
                FieldDefinition::new("field_notify_arrive", FieldType::Boolean)
                    .label("Notify when they arrive"),
                FieldDefinition::new("field_notify_depart", FieldType::Boolean)
                    .label("Notify when they leave"),
            ],
        },
    ]
}

/// Permissions: the two plugin-level ones plus CRUD for each Item type.
///
/// Only the two Item types get CRUD. The record types (`ng_device_state`,
/// `ng_event`, the timelines) have no write surface to gate — the record admin is
/// list-and-view only — and their read access is governed by
/// `PERM_VIEW_DEVICES` and by the gathers' own access checks.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    let mut perms = vec![
        PermissionDefinition::new(
            PERM_ADMINISTER,
            "Manage netgrasp devices, people and configuration",
        ),
        PermissionDefinition::new(PERM_VIEW_DEVICES, "See the device lists and device pages"),
    ];
    perms.extend(PermissionDefinition::crud_for_type(DEVICE_TYPE));
    perms.extend(PermissionDefinition::crud_for_type(PERSON_TYPE));
    perms
}

/// Navigation entries for the plugin's routes.
///
/// Navigation is all these are. `MenuDefinition` carries a `callback` field in
/// the SDK that the kernel's own type does not have, so it is dropped on
/// deserialize and no handler is ever registered (`G-NO-PLUGIN-HTTP`). The paths
/// below work because `002_netgrasp_gathers.sql` aliases each one onto a
/// `/gather/<query_id>` route — the menu makes them findable, the URL aliases
/// make them exist. Nothing here sets a callback, because a field that silently
/// does nothing is worse than no field.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/devices/online", "Online now").permission(PERM_VIEW_DEVICES),
        MenuDefinition::new("/devices", "All devices").permission(PERM_VIEW_DEVICES),
        MenuDefinition::new("/who-is-home", "Who is home").permission(PERM_VIEW_DEVICES),
        MenuDefinition::new("/events", "Events").permission(PERM_VIEW_DEVICES),
        MenuDefinition::new("/events/security", "Security events").permission(PERM_VIEW_DEVICES),
        MenuDefinition::new("/people", "People").permission(PERM_VIEW_DEVICES),
    ]
}

// ===========================================================================
// daemon → kernel
// ===========================================================================

/// One sync pass plus one retention pass, per cron cycle.
///
/// Never panics and never propagates: `tap_cron` shares one dispatch budget
/// across every plugin, so a Netgrasp failure must not cost another plugin its
/// tick. Everything is logged and reported in the return value.
///
/// The return value is small by construction — a handful of integers — because
/// it crosses the 64 KB tap I/O buffer. The device rows themselves never do:
/// they are read through the `db` host and written through `item-api`.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    // The database's clock, not the tap's timestamp: the daemon writes its
    // timestamps against Postgres, and `input.timestamp` is the kernel host's
    // clock. On a single machine they agree; the retention cutoff should not
    // depend on that.
    let now = match db::now() {
        Ok(n) => n,
        Err(e) => {
            host::log(
                "warning",
                "netgrasp",
                &format!("tap_cron: clock read failed, falling back to the tap timestamp: {e}"),
            );
            input.timestamp
        }
    };

    let sync = match sync_host::sync_devices() {
        Ok(report) => {
            if report.created + report.relinked + report.refreshed > 0 {
                host::log(
                    "info",
                    "netgrasp",
                    &format!(
                        "sync: {} created, {} relinked, {} refreshed, {} skipped, {} failed",
                        report.created,
                        report.relinked,
                        report.refreshed,
                        report.skipped,
                        report.failed
                    ),
                );
            }
            serde_json::to_value(&report).unwrap_or(serde_json::Value::Null)
        }
        Err(e) => {
            host::log("error", "netgrasp", &format!("tap_cron: sync failed: {e}"));
            serde_json::json!({ "error": e.to_string() })
        }
    };

    let pruned = match sync_host::prune_events(now) {
        Ok(0) => serde_json::json!(0),
        Ok(n) => {
            host::log("info", "netgrasp", &format!("pruned {n} expired events"));
            serde_json::json!(n)
        }
        Err(e) => {
            host::log(
                "warning",
                "netgrasp",
                &format!("tap_cron: prune failed: {e}"),
            );
            serde_json::json!({ "error": e.to_string() })
        }
    };

    serde_json::json!({ "sync": sync, "pruned": pruned })
}

// ===========================================================================
// kernel → daemon
// ===========================================================================

/// Write an admin's edit back to the daemon's tables.
///
/// This is the write-back channel, and it is worth being precise about when it
/// fires, because the sync loop's termination depends on it: `tap_item_update` is
/// dispatched by `ItemService::update`, whose callers are the admin content
/// routes and the JSON item routes. The `save-item` host function the sync pass
/// uses calls `Item::update` **directly**, so a plugin's own write does not
/// arrive here (`DESIGN.md` Drift 3). The loop therefore has no edge to traverse.
///
/// It would still terminate if that changed, because
/// [`netgrasp_core::writeback::build_update`] cannot emit `sync_state` and so
/// cannot mark a row for re-sync.
#[plugin_tap]
pub fn tap_item_update(input: serde_json::Value) -> serde_json::Value {
    match item_type_of(&input) {
        t if t == DEVICE_TYPE => match sync_host::write_back_device(&input) {
            // Zero rows is normal, not an error: a device Item whose daemon row
            // has since been deleted, or one an admin created by hand before any
            // row existed, matches nothing.
            Ok(rows) => serde_json::json!({ "wrote_back": rows }),
            Err(e) => {
                host::log(
                    "warning",
                    "netgrasp",
                    &format!("tap_item_update: device write-back failed: {e}"),
                );
                serde_json::json!({ "error": e.to_string() })
            }
        },
        t if t == PERSON_TYPE => mirror_person_result(&input),
        _ => serde_json::json!({}),
    }
}

/// Mirror a newly created person into `ng_people`.
///
/// Devices are not handled here: a device Item is created by the sync pass
/// through `save-item`, which fires no taps, and an admin creating one by hand
/// has nothing to write back to (no daemon row carries its id yet). The next
/// daemon sighting of that MAC creates the real row, and the pairing is the
/// operator's to make.
#[plugin_tap]
pub fn tap_item_insert(input: serde_json::Value) -> serde_json::Value {
    if item_type_of(&input) == PERSON_TYPE {
        return mirror_person_result(&input);
    }
    serde_json::json!({})
}

/// Retire the daemon-side traces of a deleted Item.
///
/// A deleted **person** loses their mirror row and their devices lose their
/// owner. A deleted **device** Item unlinks its daemon row and marks it dirty, so
/// the next sync pass mints a replacement Item — deleting a device Item means
/// "forget my edits and start over", not "stop tracking this device", because the
/// device is still on the network either way.
#[plugin_tap]
pub fn tap_item_delete(input: serde_json::Value) -> serde_json::Value {
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return serde_json::json!({});
    };
    match item_type_of(&input) {
        t if t == PERSON_TYPE => match sync_host::retire_person(id) {
            Ok(()) => serde_json::json!({ "retired": id }),
            Err(e) => {
                host::log(
                    "warning",
                    "netgrasp",
                    &format!("tap_item_delete: retiring person {id} failed: {e}"),
                );
                serde_json::json!({ "error": e.to_string() })
            }
        },
        t if t == DEVICE_TYPE => match sync_host::unlink_device(id) {
            Ok(rows) => serde_json::json!({ "unlinked": rows }),
            Err(e) => {
                host::log(
                    "warning",
                    "netgrasp",
                    &format!("tap_item_delete: unlinking device {id} failed: {e}"),
                );
                serde_json::json!({ "error": e.to_string() })
            }
        },
        _ => serde_json::json!({}),
    }
}

/// Coerce an admin's edit into something usable.
///
/// `tap_item_presave` can **modify but not refuse**: the kernel merges the
/// `fields` object it gets back and then saves unconditionally, and a returned
/// `status` is ignored (`G-NO-PRESAVE-VETO`). So a MAC that is not a MAC cannot
/// be rejected. What this does instead is normalise the two things that would
/// otherwise be silently wrong:
///
/// - a MAC is lower-cased and colon-separated, so `AA-BB-CC-DD-EE-FF` and
///   `aa:bb:cc:dd:ee:ff` are the same device rather than two;
/// - an owner id that is not a uuid is blanked, because it would otherwise reach
///   `owner_item_id`, a uuid column, and fail the write-back with a cast error
///   the admin would never see.
///
/// Both are coercions the admin can observe by looking at the saved value. The
/// alternative — letting a bad value through to fail two layers away in a
/// background tap — is the failure mode `G-NO-PRESAVE-VETO` actually costs.
#[plugin_tap]
pub fn tap_item_presave(input: serde_json::Value) -> serde_json::Value {
    if item_type_of_presave(&input) != DEVICE_TYPE {
        return serde_json::json!({});
    }

    let mut fields = serde_json::Map::new();
    if let Some(mac) = netgrasp_core::writeback::field_str(&input, "field_mac") {
        fields.insert("field_mac".into(), serde_json::json!(normalize_mac(&mac)));
    }
    match netgrasp_core::writeback::field_str(&input, "field_owner") {
        Some(owner) if is_uuid_shaped(&owner) => {
            fields.insert("field_owner".into(), serde_json::json!(owner));
        }
        Some(_) => {
            // Blanked rather than kept: an unowned device is a correct state, a
            // device owned by a string that is not an id is not.
            fields.insert("field_owner".into(), serde_json::json!(""));
        }
        None => {}
    }

    if fields.is_empty() {
        return serde_json::json!({});
    }
    serde_json::json!({ "fields": fields })
}

/// Mirror a person Item and shape the tap's answer.
fn mirror_person_result(item: &serde_json::Value) -> serde_json::Value {
    match sync_host::mirror_person(item) {
        Ok(rows) => serde_json::json!({ "mirrored": rows }),
        Err(e) => {
            host::log("warning", "netgrasp", &format!("person mirror failed: {e}"));
            serde_json::json!({ "error": e.to_string() })
        }
    }
}

// ===========================================================================
// The device page
// ===========================================================================

/// Render the device page fragment.
///
/// Best-effort throughout: a failed history read must not cost an admin the page
/// they asked for, so each read is logged and degraded rather than propagated.
/// A device whose daemon row is unreadable still renders its identity block; a
/// device with no linked row renders an explanation.
#[plugin_tap]
pub fn tap_item_view(input: serde_json::Value) -> String {
    if item_type_of(&input) != DEVICE_TYPE {
        return String::new();
    }
    let Some(item_id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return String::new();
    };

    let now = db::now().unwrap_or_default();

    let state = match sync_host::load_device_state(item_id) {
        Ok(s) => s,
        Err(e) => {
            host::log(
                "warning",
                "netgrasp",
                &format!("tap_item_view: device state for {item_id}: {e}"),
            );
            None
        }
    };

    let history = match state.as_ref() {
        Some(s) => sync_host::load_device_history(s.id).unwrap_or_else(|e| {
            host::log(
                "warning",
                "netgrasp",
                &format!("tap_item_view: history for {item_id}: {e}"),
            );
            empty_history()
        }),
        None => empty_history(),
    };

    let owner_name = netgrasp_core::writeback::field_str(&input, "field_owner")
        .and_then(|id| sync_host::load_owner_name(&id).ok().flatten());

    device_view::render(state.as_ref(), &history, owner_name.as_deref(), now)
}

/// An empty history, for the degraded paths.
fn empty_history() -> sync_host::DeviceHistory {
    sync_host::DeviceHistory {
        presence: Vec::new(),
        locations: Vec::new(),
        addresses: Vec::new(),
    }
}

// ===========================================================================
// Shared helpers
// ===========================================================================

/// The content type named by a saved-Item tap payload.
///
/// The kernel serializes an `Item` with its type under `type`; some payloads use
/// `item_type`. Both are accepted rather than depending on which tap is calling.
fn item_type_of(input: &serde_json::Value) -> &str {
    input
        .get("type")
        .or_else(|| input.get("item_type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// The content type named by a presave payload, which uses `item_type`.
fn item_type_of_presave(input: &serde_json::Value) -> &str {
    input
        .get("item_type")
        .or_else(|| input.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// Normalise a MAC to lower-case colon-separated form.
///
/// Anything that is not twelve hex digits is returned trimmed and lower-cased
/// but otherwise untouched — presave cannot refuse a save, so a malformed value
/// has to be stored as something, and storing it recognisably wrong is better
/// than storing a mangled guess.
fn normalize_mac(raw: &str) -> String {
    let hex: String = raw
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect();
    if hex.len() != 12 {
        return raw.trim().to_lowercase();
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

/// Whether a string is shaped like a uuid.
///
/// Shape only — the database does the real parsing. This exists to keep a
/// non-uuid out of a uuid column, where it would fail the write-back two layers
/// from where the admin typed it.
fn is_uuid_shaped(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const PERSON_ID: &str = "33333333-3333-4333-8333-333333333333";

    // --- declarations -----------------------------------------------------

    #[test]
    fn only_the_two_editable_types_are_items() {
        let types = __inner_tap_item_info();
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        assert_eq!(names, [DEVICE_TYPE, PERSON_TYPE]);
    }

    /// The skeleton declared six content types, four of which were high-churn
    /// daemon data. Those are record types now (`DESIGN.md` Decision 2), and the
    /// names must not reappear as Items or the record registry would reject them
    /// for colliding with a content type.
    #[test]
    fn the_high_churn_daemon_types_are_no_longer_items() {
        let types = __inner_tap_item_info();
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        for record_only in ["ng_event", "ng_presence", "ng_ip_history", "ng_location"] {
            assert!(
                !names.contains(&record_only),
                "{record_only} is declared as an Item and as a record type — the record \
                 registry rejects a name that collides with a content type"
            );
        }
    }

    /// The device Item carries only what a person edits, plus the MAC echo.
    /// A daemon-owned field here would mean an item revision per sighting.
    #[test]
    fn the_device_item_carries_no_volatile_daemon_field() {
        let types = __inner_tap_item_info();
        let device = types
            .iter()
            .find(|t| t.machine_name == DEVICE_TYPE)
            .unwrap();
        let fields: Vec<&str> = device
            .fields
            .iter()
            .map(|f| f.field_name.as_str())
            .collect();
        assert_eq!(
            fields,
            [
                "field_mac",
                "field_owner",
                "field_notes",
                "field_hidden",
                "field_notify"
            ]
        );
        for volatile in [
            "state",
            "last_ip",
            "last_seen",
            "current_location",
            "hostname",
        ] {
            assert!(
                !fields.iter().any(|f| f.contains(volatile)),
                "device Item carries volatile daemon field {volatile}"
            );
        }
    }

    /// `RecordReference` does not survive an admin edit (`G-ITEM-FORM-MISMATCH`),
    /// so the owner is a plain text uuid. The skeleton had it the other way.
    #[test]
    fn no_field_is_a_record_reference() {
        for t in __inner_tap_item_info() {
            for f in &t.fields {
                assert!(
                    !matches!(f.field_type, FieldType::RecordReference(_)),
                    "{}.{} is a RecordReference and would lose its value on edit",
                    t.machine_name,
                    f.field_name
                );
            }
        }
    }

    #[test]
    fn permissions_cover_both_item_types_plus_the_two_plugin_permissions() {
        let perms = __inner_tap_perm();
        let names: Vec<&str> = perms.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&PERM_ADMINISTER));
        assert!(names.contains(&PERM_VIEW_DEVICES));
        for verb in ["view", "create", "edit", "delete"] {
            assert!(names.contains(&format!("{verb} {DEVICE_TYPE} content").as_str()));
            assert!(names.contains(&format!("{verb} {PERSON_TYPE} content").as_str()));
        }
        assert_eq!(perms.len(), 10);
    }

    /// The kernel's fallback format is "{op} {type} content" with no "any"
    /// qualifier; a permission the kernel never checks is a permission that
    /// silently grants nothing.
    #[test]
    fn permission_strings_match_the_kernel_fallback_format() {
        for perm in __inner_tap_perm() {
            assert!(!perm.name.contains(" any "), "'{}' has an 'any'", perm.name);
        }
    }

    /// `MenuDefinition.callback` is dropped on deserialize by the kernel
    /// (`G-NO-PLUGIN-HTTP`). Setting one would advertise a handler that does not
    /// exist; the skeleton set two.
    #[test]
    fn no_menu_entry_claims_a_callback_the_kernel_would_drop() {
        for m in __inner_tap_menu() {
            assert!(
                m.callback.is_empty(),
                "menu {} sets a callback, which the kernel drops",
                m.path
            );
        }
    }

    /// Every menu path must be a route the gather migration actually aliases,
    /// or the navigation links to a 404.
    #[test]
    fn every_menu_path_is_an_alias_the_migration_seeds() {
        let migration = include_str!("../migrations/002_netgrasp_gathers.sql");
        for m in __inner_tap_menu() {
            assert!(
                migration.contains(&format!("'{}'", m.path)),
                "menu path {} has no url_alias in 002_netgrasp_gathers.sql",
                m.path
            );
        }
    }

    /// The security-event list is written twice — once in Rust for the UI and
    /// once in the gather's `in` filter — and they must not drift.
    #[test]
    fn the_security_gather_lists_exactly_the_declared_security_event_types() {
        let migration = include_str!("../migrations/002_netgrasp_gathers.sql");
        for t in netgrasp_core::model::SECURITY_EVENT_TYPES {
            assert!(
                migration.contains(&format!("\"{t}\"")),
                "security event type {t} is missing from the ng_event_security gather"
            );
        }
    }

    /// The manifest's `db_tables` must name every table the plugin's SQL
    /// touches, or a structured call is denied at runtime with
    /// `table-not-declared`.
    #[test]
    fn every_table_the_plugin_writes_is_declared_in_the_manifest() {
        let manifest = include_str!("../netgrasp.info.toml");
        for table in [
            "ng_devices",
            "ng_people",
            "ng_events",
            "ng_presence",
            "ng_location_history",
            "ng_ip_history",
        ] {
            assert!(
                manifest.contains(&format!("\"{table}\"")),
                "{table} is not in db_tables"
            );
        }
    }

    // --- presave coercion -------------------------------------------------

    #[test]
    fn a_mac_is_normalized_to_lower_case_colon_form() {
        for raw in [
            "AA-BB-CC-DD-EE-FF",
            "aabb.ccdd.eeff",
            "AA:BB:CC:DD:EE:FF",
            " aabbccddeeff ",
        ] {
            assert_eq!(normalize_mac(raw), "aa:bb:cc:dd:ee:ff", "input {raw:?}");
        }
    }

    /// Presave cannot refuse, so a malformed MAC has to be stored as something.
    /// Storing it recognisably wrong beats storing a mangled guess.
    #[test]
    fn a_malformed_mac_is_left_recognisable_rather_than_mangled() {
        assert_eq!(normalize_mac("not a mac"), "not a mac");
        assert_eq!(normalize_mac("AA:BB"), "aa:bb");
    }

    #[test]
    fn presave_normalizes_the_mac_and_keeps_a_valid_owner() {
        let input = serde_json::json!({
            "item_type": DEVICE_TYPE,
            "fields": {"field_mac": "AA-BB-CC-DD-EE-FF", "field_owner": PERSON_ID}
        });
        let out = __inner_tap_item_presave(input);
        assert_eq!(out["fields"]["field_mac"], "aa:bb:cc:dd:ee:ff");
        assert_eq!(out["fields"]["field_owner"], PERSON_ID);
    }

    /// A non-uuid owner would reach `owner_item_id`, a uuid column, and fail the
    /// write-back with a cast error the admin never sees.
    #[test]
    fn presave_blanks_an_owner_that_is_not_a_uuid() {
        let input = serde_json::json!({
            "item_type": DEVICE_TYPE,
            "fields": {"field_owner": "Jeremy"}
        });
        let out = __inner_tap_item_presave(input);
        assert_eq!(out["fields"]["field_owner"], "");
    }

    #[test]
    fn presave_leaves_other_content_types_alone() {
        let input = serde_json::json!({
            "item_type": "blog_post",
            "fields": {"field_mac": "AA-BB-CC-DD-EE-FF"}
        });
        assert_eq!(__inner_tap_item_presave(input), serde_json::json!({}));
    }

    #[test]
    fn uuid_shape_accepts_a_uuid_and_rejects_near_misses() {
        assert!(is_uuid_shaped(PERSON_ID));
        assert!(!is_uuid_shaped("Jeremy"));
        assert!(!is_uuid_shaped(""));
        assert!(!is_uuid_shaped("33333333-3333-4333-8333-33333333333"));
        assert!(!is_uuid_shaped("33333333_3333-4333-8333-333333333333"));
        assert!(!is_uuid_shaped("gggggggg-3333-4333-8333-333333333333"));
    }

    // --- tap routing ------------------------------------------------------

    #[test]
    fn the_view_tap_renders_nothing_for_a_type_that_is_not_a_device() {
        for other in ["ng_person", "blog_post", ""] {
            let input = serde_json::json!({"type": other, "id": PERSON_ID});
            assert_eq!(__inner_tap_item_view(input), "");
        }
    }

    #[test]
    fn the_update_tap_ignores_a_type_it_does_not_own() {
        let input = serde_json::json!({"type": "blog_post", "id": PERSON_ID});
        assert_eq!(__inner_tap_item_update(input), serde_json::json!({}));
    }

    #[test]
    fn the_delete_tap_ignores_a_payload_with_no_id() {
        let input = serde_json::json!({"type": DEVICE_TYPE});
        assert_eq!(__inner_tap_item_delete(input), serde_json::json!({}));
    }

    #[test]
    fn the_insert_tap_only_mirrors_people() {
        let input = serde_json::json!({"type": DEVICE_TYPE, "id": PERSON_ID});
        assert_eq!(__inner_tap_item_insert(input), serde_json::json!({}));
    }

    #[test]
    fn the_type_helper_accepts_both_spellings_the_kernel_uses() {
        assert_eq!(item_type_of(&serde_json::json!({"type": "a"})), "a");
        assert_eq!(item_type_of(&serde_json::json!({"item_type": "b"})), "b");
        assert_eq!(item_type_of(&serde_json::json!({})), "");
        assert_eq!(
            item_type_of_presave(&serde_json::json!({"item_type": "c"})),
            "c"
        );
    }

    #[test]
    fn the_retention_default_the_plugin_uses_is_the_ninety_days_the_daemon_keeps() {
        assert_eq!(netgrasp_core::retention::DEFAULT_RETENTION_DAYS, 90);
    }
}
