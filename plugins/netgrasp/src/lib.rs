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

mod assist_host;
mod db;
mod device_view;
mod forms;
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
/// `MenuRoute`, not `MenuDefinition`, for one reason: **weight**.
///
/// The kernel sorts the navigation by weight and holds the entries in a
/// `HashMap` (`crates/kernel/src/menu/registry.rs`), so six entries that all
/// weigh the same come out in whatever order the map iterates — the navigation
/// rendered in a different order on different requests to the same running
/// server. `MenuDefinition` has no weight field to set: it is a frozen type and
/// predates the weight, which is exactly why `MenuRoute` exists. The two
/// serialize to the same shape the registry reads, so this is a change of SDK
/// type and not of contract.
///
/// The order below is the order a person reads them in: the overview, then what
/// is here now, then everything, then who, then what happened.
///
/// Navigation is all the seven below are. `callback` is left empty on every one:
/// the kernel routes an entry only when `handler_type` is `"api"` and a callback
/// is set, and `MenuRoute::page` is a plain link (`G-NO-PLUGIN-HTTP`). The paths
/// work because `002_netgrasp_gathers.sql` (and, for `/overview`,
/// `008_netgrasp_overview.sql`) aliases each one onto a
/// `/gather/<query_id>` route — the menu makes them findable, the URL aliases
/// make them exist.
///
/// The row menu's forms follow them, and are the other kind of entry: invisible,
/// each naming a callback, each routed to [`tap_api`]. `tap_menu` is where a
/// plugin declares a served path at all, which is why two unrelated things come
/// out of one tap.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    let mut menu = vec![
        MenuRoute::page("/overview", "Overview")
            .permission(PERM_VIEW_DEVICES)
            .weight(0),
        MenuRoute::page("/devices/online", "Online now")
            .permission(PERM_VIEW_DEVICES)
            .weight(1),
        MenuRoute::page("/devices", "All devices")
            .permission(PERM_VIEW_DEVICES)
            .weight(2),
        MenuRoute::page("/who-is-home", "Who is home")
            .permission(PERM_VIEW_DEVICES)
            .weight(3),
        MenuRoute::page("/people", "People")
            .permission(PERM_VIEW_DEVICES)
            .weight(4),
        MenuRoute::page("/events", "Events")
            .permission(PERM_VIEW_DEVICES)
            .weight(5),
        MenuRoute::page("/events/security", "Security events")
            .permission(PERM_VIEW_DEVICES)
            .weight(6),
    ];
    // The row menu's forms. Invisible, gated on `administer netgrasp`, and each
    // one naming a callback — which is the only combination the kernel routes
    // (`routes/plugin_api.rs`). They ride in the same tap because `tap_menu` is
    // where the kernel learns about a plugin-served path at all; nothing about
    // them is navigation.
    menu.extend(forms::routes());
    menu
}

/// Serve one of the row menu's forms.
///
/// The kernel dispatches here for any menu entry whose `handler_type` is `api`
/// and whose `callback` is set, having already checked that entry's permission
/// and, for a state-changing method, the `_token`. What is left for the plugin
/// is the permission check the host does literally, the form, and the write —
/// see `forms.rs` for why the write cannot start in the row itself.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    forms::serve(&request)
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
// Configuring Netgrasp by conversation
// ===========================================================================

/// Declare the three things a person can configure by talking to them.
///
/// Dispatched once at boot, without services, so this must be a constant: no
/// database, no host calls beyond what building a value takes. Everything that
/// varies — what devices exist, who owns them — arrives later through
/// [`tap_assistant_context`] and the read tools.
#[plugin_tap]
pub fn tap_assistant_scopes() -> Vec<AssistantScope> {
    assist_host::scopes()
}

/// Describe whatever a conversation was opened on.
#[plugin_tap]
pub fn tap_assistant_context(request: AssistantContextRequest) -> AssistantContext {
    assist_host::context(&request)
}

/// Answer one tool call: a read, a description of a write, or a write.
///
/// A write reaches `Execute` only after a person applied the proposal that
/// `Describe` produced. Netgrasp does not have to trust that: every write tool
/// checks `administer netgrasp` again here, because a conversation outlives the
/// request that opened it and the kernel checked the permission then.
#[plugin_tap]
pub fn tap_assistant_tool(call: AssistantToolCall) -> AssistantToolResult {
    assist_host::tool(&call)
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
        for m in navigation() {
            assert!(
                m.callback.is_empty(),
                "navigation entry {} sets a callback, which the kernel drops",
                m.path
            );
        }
    }

    /// The other half of the same rule, from the other side. A callback is
    /// dispatched only when `handler_type` is `"api"`, so an api entry with no
    /// callback is a path that 404s, and a page entry WITH one is a handler the
    /// kernel logs as unreachable at startup and never calls
    /// (`routes/plugin_api.rs`, `unreachable_callbacks`). Two shipped plugins
    /// were dead that way.
    #[test]
    fn every_served_route_names_a_handler_the_kernel_will_dispatch() {
        let served = routes_only();
        assert!(
            !served.is_empty(),
            "the row menu's forms are not registered"
        );
        for m in served {
            assert!(
                !m.callback.is_empty(),
                "api route {} names no callback and will 404",
                m.path
            );
            assert!(
                !m.permission.is_empty(),
                "api route {} is public, and every one of them writes or discloses",
                m.path
            );
        }
    }

    /// The navigation entries, which are what the six assertions about ordering
    /// and aliases are about.
    fn navigation() -> Vec<MenuRoute> {
        __inner_tap_menu()
            .into_iter()
            .filter(|m| m.handler_type == "page")
            .collect()
    }

    /// The plugin-served routes, which are not navigation and are not aliased.
    fn routes_only() -> Vec<MenuRoute> {
        __inner_tap_menu()
            .into_iter()
            .filter(|m| m.handler_type == "api")
            .collect()
    }

    /// The kernel sorts the navigation by weight out of a `HashMap`, so equal
    /// weights mean an order that varies between requests. Distinct weights are
    /// the only thing that makes the navigation stable, and the assertion is on
    /// the rendered order rather than on "they differ" so that reordering the
    /// vec without reordering the weights is caught too.
    #[test]
    fn the_navigation_has_one_stable_order() {
        let mut menus = navigation();
        menus.sort_by_key(|m| m.weight);
        let ordered: Vec<&str> = menus.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            ordered,
            [
                "/overview",
                "/devices/online",
                "/devices",
                "/who-is-home",
                "/people",
                "/events",
                "/events/security",
            ]
        );

        let mut weights: Vec<i32> = navigation().iter().map(|m| m.weight).collect();
        weights.sort_unstable();
        weights.dedup();
        assert_eq!(weights.len(), 7, "two menu entries share a weight");
    }

    /// A navigation entry must be visible, or it is filtered out by
    /// `root_menus()` before any permission is considered. A served route must
    /// NOT be: the forms are reached from a row menu, and an invisible entry is
    /// how a plugin says "route this, do not list it".
    #[test]
    fn navigation_is_visible_and_served_routes_are_not() {
        for m in navigation() {
            assert!(m.visible, "menu {} is not visible", m.path);
        }
        for m in routes_only() {
            assert!(
                !m.visible,
                "route {} would appear in the site navigation",
                m.path
            );
        }
    }

    /// Every menu path must be a route a gather migration actually aliases,
    /// or the navigation links to a 404.
    #[test]
    fn every_menu_path_is_an_alias_the_migration_seeds() {
        let migrations = [
            include_str!("../migrations/002_netgrasp_gathers.sql"),
            include_str!("../migrations/008_netgrasp_overview.sql"),
        ];
        for m in navigation() {
            assert!(
                migrations
                    .iter()
                    .any(|migration| migration.contains(&format!("'{}'", m.path))),
                "menu path {} has no url_alias in 002 or 008",
                m.path
            );
        }
    }

    /// The front page is the overview, on a fresh install and on one that
    /// still has the old default, and on nothing else.
    ///
    /// 005 claimed the setting only if nobody had, and 008 moves it only where
    /// it is still what 005 wrote. A migration that set it unconditionally would
    /// overwrite an operator's own front page on every install, which is the
    /// promise 005 made and this is the test that keeps it.
    #[test]
    fn the_overview_becomes_the_front_page_only_where_the_old_default_stands() {
        let migration = include_str!("../migrations/008_netgrasp_overview.sql");
        assert!(
            migration.contains("AND value = '\"/devices/online\"'::jsonb"),
            "008 must move the front page only when it is still /devices/online"
        );
        assert!(
            migration.contains("ON CONFLICT (key) DO NOTHING"),
            "008 must claim an unset front page without overwriting a set one"
        );
        assert!(
            !migration.contains("ON CONFLICT (key) DO UPDATE"),
            "008 would overwrite an operator's front page"
        );
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

    /// The overview counts security events itself, in SQL, so the list is
    /// written there too and the count on the overview must be the pager total
    /// of the page it links to.
    #[test]
    fn the_overview_counts_exactly_the_declared_security_event_types() {
        let views = include_str!("../migrations/007_netgrasp_overview_views.sql");
        let (_, overview) = views
            .split_once("CREATE OR REPLACE VIEW ng_overview")
            .unwrap_or_default();
        for t in netgrasp_core::model::SECURITY_EVENT_TYPES {
            // Twice: once for the total and once for the last 24 hours.
            assert_eq!(
                overview.matches(&format!("'{t}'")).count(),
                2,
                "security event type {t} is not in both of the overview's counts"
            );
        }
    }

    /// The security-event list is written a third time, in the shared event
    /// table template, which is what flags a spoof visually in the *middle* of
    /// the ordinary event log rather than only on /events/security. A type the
    /// gather selects but the template does not flag renders as an ordinary
    /// row on the security page — correct data, invisible warning.
    #[test]
    fn the_event_template_flags_exactly_the_declared_security_event_types() {
        let template = include_str!("../../../templates/gather/netgrasp/event-table.html");
        for t in netgrasp_core::model::SECURITY_EVENT_TYPES {
            assert!(
                template.contains(&format!("\"{t}\"")),
                "security event type {t} is not flagged by event-table.html"
            );
        }
    }

    /// The row menu is one partial included by three templates, which is the
    /// only reason the device table, the event table and the people cards offer
    /// the same entries. A listing that stops including it loses its menu
    /// silently: the rows still render, and nothing anywhere is an error.
    #[test]
    fn every_listing_offers_the_row_menu() {
        for (what, template) in [
            (
                "device-table.html",
                include_str!("../../../templates/gather/netgrasp/device-table.html"),
            ),
            (
                "event-table.html",
                include_str!("../../../templates/gather/netgrasp/event-table.html"),
            ),
            (
                "query--ng_person_list.html",
                include_str!("../../../templates/gather/query--ng_person_list.html"),
            ),
        ] {
            assert!(
                template.contains("gather/netgrasp/row-actions.html"),
                "{what} no longer includes the row actions menu"
            );
            assert!(
                template.contains("ng_row_ref"),
                "{what} includes the menu without telling it what the row is"
            );
        }
    }

    /// The menu's links and the plugin's routes are two lists that have to be
    /// the same list.
    ///
    /// They are written in different languages in different files — hrefs in a
    /// Tera partial, `MenuRoute`s in Rust — and nothing connects them at
    /// compile time. A path renamed on one side gives a menu entry that 404s,
    /// which looks exactly like a permission problem and is the first thing
    /// anybody would go and check.
    #[test]
    fn every_menu_entry_links_to_a_route_the_plugin_serves() {
        let partial = include_str!("../../../templates/gather/netgrasp/row-actions.html");
        let served: Vec<String> = __inner_tap_menu()
            .into_iter()
            .filter(|m| m.handler_type == "api")
            .map(|m| m.path)
            .collect();

        for path in &served {
            assert!(
                partial.contains(&format!("href=\"{path}?")),
                "{path} is served and nothing in the menu links to it"
            );
        }

        // And the other direction: every netgrasp path the menu links to is one
        // of them. An entry pointing at a path nobody registered is the 404.
        for line in partial.lines() {
            let Some(rest) = line.split_once("href=\"/netgrasp") else {
                continue;
            };
            let linked = format!(
                "/netgrasp{}",
                rest.1.split(['?', '"']).next().unwrap_or_default()
            );
            assert!(
                served.contains(&linked),
                "the menu links to {linked}, which the plugin does not serve"
            );
        }
    }

    /// **The menu may not write from a gather row, and this is why.**
    ///
    /// A gather content template is rendered in its own Tera context and the
    /// site context that carries `csrf_token` is built afterwards, for the page
    /// around it (`G-GATHER-TEMPLATE-NO-CSRF`). A `<form method="post">` here
    /// would post with no token, and the kernel refuses a state-changing plugin
    /// request without one *before* dispatching — so the form would 403 and the
    /// plugin would never be called.
    ///
    /// The failure is silent in the worst way: it looks exactly like a broken
    /// handler. This test is what stops somebody restoring the "obvious" inline
    /// form in a year's time and spending a day on the 403.
    #[test]
    fn the_row_menu_writes_nothing_from_a_gather_row() {
        let partial = include_str!("../../../templates/gather/netgrasp/row-actions.html");
        let markup = strip_tera_comments(partial);
        assert!(
            !markup.contains("<form"),
            "a gather template cannot carry a form token, so the menu must not post from a row"
        );
        assert!(
            !markup.contains("method=\"post\""),
            "a gather template cannot carry a form token, so the menu must not post from a row"
        );
    }

    /// The right-click handler and the menu are two files agreeing on one
    /// selector, the same way the chrome and the script agree on the refresh
    /// attribute. Rename the class in one of them and right-click silently
    /// stops working while the button keeps working — the hardest kind of half
    /// to notice.
    #[test]
    fn the_menu_and_the_script_agree_on_the_menu_element() {
        let partial = include_str!("../../../templates/gather/netgrasp/row-actions.html");
        let script = include_str!("../../../static/js/netgrasp.js");

        assert!(
            partial.contains("<details class=\"ng-menu\""),
            "the menu is no longer a details element with the class the script looks for"
        );
        assert!(
            script.contains("details.ng-menu"),
            "the script no longer finds the menu"
        );
        assert!(
            script.contains("contextmenu"),
            "the script no longer opens the menu on right-click"
        );
        // The no-JavaScript guarantee, asserted rather than trusted: the menu
        // opens because it is a <details>, so the script must never be what
        // makes an entry reachable.
        assert!(
            partial.contains("<summary"),
            "the menu no longer opens without JavaScript"
        );
    }

    /// The reload must not close a menu somebody just opened. The script and
    /// the menu agree on that through the same selector as above; what this
    /// pins is that the deferral exists at all, because removing it leaves a
    /// menu that closes itself ten seconds after it opens.
    #[test]
    fn the_reload_defers_while_the_page_is_in_use() {
        let script = include_str!("../../../static/js/netgrasp.js");
        assert!(
            script.contains("inUse"),
            "the reload no longer checks whether the page is in use"
        );
        assert!(
            script.contains("details.ng-menu[open]"),
            "the reload no longer notices an open menu"
        );
    }

    /// The menu is styled in both colour schemes. A dark-mode block that loses
    /// the menu's rules leaves an unreadable control rather than an ugly one.
    #[test]
    fn the_stylesheet_covers_the_menu_in_both_schemes() {
        let css = include_str!("../../../static/css/netgrasp.css");
        for class in [
            ".ng-menu",
            ".ng-menu__button",
            ".ng-menu__panel",
            ".ng-actions",
            ".ng-visually-hidden",
        ] {
            assert!(css.contains(class), "{class} is not styled");
        }
        assert!(
            css.contains("@media (prefers-color-scheme: dark)"),
            "the stylesheet has no dark mode"
        );
        let (_, dark) = css
            .split_once("@media (prefers-color-scheme: dark)")
            .unwrap_or_default();
        assert!(
            dark.contains(".ng-menu__button"),
            "the menu button is not restyled for dark mode"
        );
    }

    /// **The listing templates are rendered, not grepped.**
    ///
    /// Every other template assertion in this file is a string search, which
    /// cannot tell a working template from one that aborts on the first row. It
    /// matters more here than it sounds: when a gather template raises, the
    /// kernel falls back to its built-in dump of every column of the base
    /// table, daemon internals included — the page still renders, so nothing
    /// looks broken, and seven of these nine pages were in exactly that state
    /// before `gather/netgrasp/page.html` existed.
    ///
    /// The rows below are the shape a *record* gather really yields: flat, keyed
    /// by physical column name, with JSON nulls where a column is unset — which
    /// is the case that breaks templates, because `default` does not fire for a
    /// null and an undefined variable aborts the render.
    #[test]
    fn every_listing_template_renders_with_the_rows_it_will_really_get() {
        let tera = match tera::Tera::new("../../templates/**/*.html") {
            Ok(t) => t,
            Err(e) => panic!("the templates do not parse: {e}"),
        };

        // A device with everything, and a device with nothing but the two
        // columns the schema guarantees: a MAC and an id. The second is the
        // interesting one — no Item, no name, no owner, no flags.
        let devices = serde_json::json!([
            {
                "id": 1,
                "mac": "02:00:5e:00:00:04",
                "display_name": "Office printer",
                "resolved_name": "printer.local",
                "hostname": "printer",
                "device_type": "printer",
                "state": "online",
                "last_ip": "10.0.2.18",
                "owner_item_id": "0193a5a0-0000-7000-8000-00000000000a",
                "owner_name": "Jamie",
                "hidden": false,
                "notify": true,
                "trovato_item_id": "0193a5a0-0000-7000-8000-00000000000b",
                "last_seen_at_epoch": 1_757_000_000
            },
            {
                "id": 2,
                "mac": "02:00:5e:00:00:09",
                "display_name": null,
                "resolved_name": null,
                "hostname": null,
                "device_type": null,
                "state": null,
                "last_ip": null,
                "owner_item_id": null,
                "owner_name": null,
                "hidden": null,
                "notify": null,
                "trovato_item_id": null,
                "last_seen_at_epoch": null
            }
        ]);

        let events = serde_json::json!([
            {
                "id": 11,
                "device_id": 2,
                "event_type": "arp_spoof",
                "timestamp_epoch": 1_757_000_100,
                "details": {"claimed": "10.0.2.1"}
            },
            // An event with no device: the row that must render a menu-less
            // cell rather than a menu about nothing.
            {"id": 12, "device_id": null, "event_type": null, "timestamp_epoch": null, "details": null}
        ]);

        let people = serde_json::json!([
            {
                "id": "0193a5a0-0000-7000-8000-00000000000a",
                "title": "Jamie",
                "fields": {"field_notes": "", "field_notify_arrive": true}
            },
            // A person with no fields at all, which is what an Item the plugin
            // minted carrying only a title looks like.
            {"id": "0193a5a0-0000-7000-8000-00000000000c", "title": null, "fields": {}}
        ]);

        for (template, rows) in [
            ("gather/netgrasp/device-table.html", &devices),
            ("gather/netgrasp/event-table.html", &events),
            ("gather/query--ng_person_list.html", &people),
        ] {
            let mut context = tera::Context::new();
            context.insert("rows", rows);
            context.insert("total", &2);
            context.insert("page", &1);
            context.insert("total_pages", &1);
            context.insert("base_path", "/devices");
            context.insert(
                "query",
                &serde_json::json!({"query_id": "ng_device_list", "label": "Devices"}),
            );
            context.insert("filter_values", &serde_json::json!({}));

            let html = tera
                .render(template, &context)
                .unwrap_or_else(|e| panic!("{template} failed to render: {e:#?}"));

            // The menu reached every row, including the row with nothing on it.
            assert!(
                html.contains("ng-menu__button"),
                "{template} rendered no row menu"
            );
            // The degradation rule: a device with no Item still gets actions,
            // and its assistant entry falls back to the network scope carrying
            // the device's own reference.
            if template == "gather/netgrasp/device-table.html" {
                assert!(
                    html.contains("/ai/assistant/netgrasp_device/"),
                    "a device with an Item must open its own scope"
                );
                assert!(
                    html.contains("/ai/assistant/netgrasp_network?device="),
                    "a device with no Item must fall back to the network scope"
                );
                assert!(
                    html.contains("02%3A00%3A5e%3A00%3A00%3A09"),
                    "the fallback must carry the MAC: {html}"
                );
            }
        }
    }

    /// **The overview and its three listings are rendered, not grepped**, for
    /// the reason the test above gives, and with the rows that break templates:
    /// nulls in every column that can hold one, an include that came back empty,
    /// and a departure with neither a place nor a way out.
    ///
    /// The overview's row is the shape the kernel builds: the view's columns,
    /// with each include's child rows attached under the include's name.
    #[test]
    fn the_overview_and_its_listings_render_with_the_rows_they_will_really_get() {
        let tera = match tera::Tera::new("../../templates/**/*.html") {
            Ok(t) => t,
            Err(e) => panic!("the templates do not parse: {e}"),
        };

        let home = serde_json::json!([
            {
                "item_id": "0193a5a0-0000-7000-8000-00000000000a",
                "name": "Jamie",
                "state": "home",
                "current_location": "Studio",
                "last_arrived_at_epoch": 1_757_000_000,
                "last_departed_at_epoch": null,
                "devices_online": 2
            },
            // Home, with no arrival time and no location: a mirror row the
            // daemon has set a state on but never an arrival.
            {
                "item_id": "0193a5a0-0000-7000-8000-00000000000c",
                "name": "Arlo",
                "state": "home",
                "current_location": null,
                "last_arrived_at_epoch": null,
                "last_departed_at_epoch": null,
                "devices_online": 0
            }
        ]);
        let movements = serde_json::json!([
            {
                "id": 21,
                "event_type": "person_arrived",
                "timestamp_epoch": 1_757_000_000,
                "day": "2025-09-04",
                "person_item_id": "0193a5a0-0000-7000-8000-00000000000a",
                "person_name": "Jamie",
                "location": "Studio",
                "via": "Driveway",
                "device_id": 1,
                "device_mac": "02:00:5e:00:00:01",
                "device_display_name": "Jamie's laptop",
                "device_resolved_name": null,
                "device_hostname": null,
                "device_item_id": "0193a5a0-0000-7000-8000-00000000000b"
            },
            // A departure from a device that has since been deleted, recorded
            // for a person with no details: every nullable column null.
            {
                "id": 22,
                "event_type": "person_departed",
                "timestamp_epoch": 1_757_000_100,
                "day": "2025-09-04",
                "person_item_id": null,
                "person_name": null,
                "location": null,
                "via": null,
                "device_id": null,
                "device_mac": null,
                "device_display_name": null,
                "device_resolved_name": null,
                "device_hostname": null,
                "device_item_id": null
            }
        ]);
        let new_devices = serde_json::json!([
            {
                "id": 12,
                "mac": "02:00:5e:00:00:0c",
                "display_name": null,
                "resolved_name": null,
                "hostname": "guest-phone",
                "mdns_name": null,
                "vendor": "Apple, Inc.",
                "device_type": "phone",
                "device_type_confidence": 0.92,
                "os_family": "iOS",
                "identity_source": "dhcp",
                "identity_confidence": 0.8,
                "state": "online",
                "last_ip": "10.0.1.201",
                "hidden": false,
                "notify": true,
                "owner_item_id": null,
                "owner_name": null,
                "trovato_item_id": null,
                "first_seen_at_epoch": 1_757_000_000,
                "last_seen_at_epoch": 1_757_000_500,
                "period": "this_week"
            },
            // Nothing identified at all: no type, so no confidence either.
            {
                "id": 13,
                "mac": "02:00:5e:00:00:08",
                "display_name": null,
                "resolved_name": null,
                "hostname": null,
                "mdns_name": null,
                "vendor": null,
                "device_type": null,
                "device_type_confidence": null,
                "os_family": null,
                "identity_source": null,
                "identity_confidence": null,
                "state": null,
                "last_ip": null,
                "hidden": false,
                "notify": true,
                "owner_item_id": null,
                "owner_name": null,
                "trovato_item_id": null,
                "first_seen_at_epoch": 1_757_000_000,
                "last_seen_at_epoch": null,
                "period": "this_week"
            }
        ]);

        let overview_row = |home: &serde_json::Value,
                            movements: &serde_json::Value,
                            new_devices: &serde_json::Value| {
            serde_json::json!([{
                "id": 1,
                "today": "2025-09-04",
                "home_state": "home",
                "new_period": "this_week",
                "people_home": 2,
                "people_total": 3,
                "devices_online": 7,
                "devices_new": 2,
                "movements_today": 2,
                "security_events": 5,
                "security_events_24h": 1,
                "generated_at_epoch": 1_757_000_600,
                "home": home,
                "movements": movements,
                "new_devices": new_devices
            }])
        };
        let empty = serde_json::json!([]);

        let render = |template: &str, query_id: &str, rows: &serde_json::Value| {
            let mut context = tera::Context::new();
            context.insert("rows", rows);
            context.insert("total", &1);
            context.insert("page", &1);
            context.insert("total_pages", &1);
            context.insert("base_path", "/overview");
            context.insert(
                "query",
                &serde_json::json!({"query_id": query_id, "label": "Overview"}),
            );
            context.insert("filter_values", &serde_json::json!({}));
            tera.render(template, &context)
                .unwrap_or_else(|e| panic!("{template} failed to render: {e:#?}"))
        };

        let full = render(
            "gather/query--ng_overview.html",
            "ng_overview",
            &overview_row(&home, &movements, &new_devices),
        );
        for expected in [
            "Home since",
            "Jamie",
            "Arlo",
            "Arrived",
            "Left",
            "via Driveway",
            "Somebody",
            "92%",
            "Not yet identified",
            "ng-menu__button",
            "href=\"/events/security\"",
            "ng-stat--alert",
            "/people/movements?day=2025-09-04",
        ] {
            assert!(
                full.contains(expected),
                "the overview did not render {expected:?}: {full}"
            );
        }
        // The row menu comes back to the overview, not to a gather path.
        // Autoescaping writes the slash as an entity, which an href decodes.
        assert!(full.contains("back=&#x2F;overview&amp;"), "{full}");

        // Every include empty, and a quiet day: three empty states, no table.
        let quiet = render(
            "gather/query--ng_overview.html",
            "ng_overview",
            &overview_row(&empty, &empty, &empty),
        );
        assert!(quiet.contains("Nobody is home."), "{quiet}");
        assert!(
            quiet.contains("Nobody has arrived or left today."),
            "{quiet}"
        );
        assert!(quiet.contains("Nothing new has appeared"), "{quiet}");
        assert!(!quiet.contains("<table"), "{quiet}");

        for (template, query_id, rows, expected) in [
            (
                "gather/query--ng_people_home.html",
                "ng_people_home",
                &home,
                "Home since",
            ),
            (
                "gather/query--ng_person_movements.html",
                "ng_person_movements",
                &movements,
                "Arrived",
            ),
            (
                "gather/query--ng_devices_new.html",
                "ng_devices_new",
                &new_devices,
                "92%",
            ),
        ] {
            let html = render(template, query_id, rows);
            assert!(
                html.contains(expected),
                "{template} lost {expected:?}: {html}"
            );
        }
    }

    /// **Location reads cleanly with UniFi enrichment on and off.** Off, every
    /// row has a null place and access point, and the device table must not
    /// grow a column of dashes; on, the column appears, links each place to its
    /// section of /devices/location, and shows the access point under it.
    #[test]
    fn the_where_column_appears_only_when_something_on_the_page_is_somewhere() {
        let tera = match tera::Tera::new("../../templates/**/*.html") {
            Ok(t) => t,
            Err(e) => panic!("the templates do not parse: {e}"),
        };
        let device = |id: i64, location: Option<&str>, ap: Option<&str>| {
            serde_json::json!({
                "id": id,
                "mac": format!("02:00:5e:00:00:{id:02x}"),
                "display_name": null,
                "resolved_name": null,
                "hostname": format!("host-{id}"),
                "device_type": null,
                "state": "online",
                "last_ip": null,
                "current_location": location,
                "current_ap": ap,
                "owner_item_id": null,
                "owner_name": null,
                "hidden": false,
                "notify": true,
                "trovato_item_id": null,
                "last_seen_at_epoch": 1_757_000_000
            })
        };
        let render = |template: &str, query_id: &str, rows: serde_json::Value| {
            let mut context = tera::Context::new();
            context.insert("rows", &rows);
            context.insert("total", &rows.as_array().map_or(0, Vec::len));
            context.insert("page", &1);
            context.insert("total_pages", &1);
            context.insert("base_path", "/devices");
            context.insert(
                "query",
                &serde_json::json!({"query_id": query_id, "label": "Devices"}),
            );
            context.insert("filter_values", &serde_json::json!({}));
            tera.render(template, &context)
                .unwrap_or_else(|e| panic!("{template} failed to render: {e:#?}"))
        };

        // Enrichment off: nothing anywhere.
        let off = render(
            "gather/netgrasp/device-table.html",
            "ng_device_list",
            serde_json::json!([device(1, None, None), device(2, None, None)]),
        );
        assert!(!off.contains("<th>Where</th>"), "{off}");
        assert!(!off.contains("/devices/location"), "{off}");
        assert!(!off.contains("ng-ap"), "{off}");

        // Enrichment on: a placed device, one with only an access point, and a
        // wired one with neither.
        let on = render(
            "gather/netgrasp/device-table.html",
            "ng_device_list",
            serde_json::json!([
                device(1, Some("Living room"), Some("Living room AP")),
                device(2, None, Some("Garage AP")),
                device(3, None, None)
            ]),
        );
        assert!(on.contains("<th>Where</th>"), "{on}");
        assert!(
            on.contains("href=\"/devices/location#loc-living-room\">Living room</a>"),
            "{on}"
        );
        assert!(on.contains("Living room AP"), "{on}");
        assert!(on.contains("ng-ap--alone\">Garage AP"), "{on}");

        // The location page: one section per place, in the gather's order, each
        // with the id the Where cells link to.
        let page = render(
            "gather/query--ng_devices_by_location.html",
            "ng_devices_by_location",
            serde_json::json!([
                device(1, Some("Living room"), Some("Living room AP")),
                device(4, Some("Living room"), Some("Living room AP")),
                device(5, Some("Studio"), Some("Studio AP"))
            ]),
        );
        let living = page.find("id=\"loc-living-room\"").unwrap_or(usize::MAX);
        let studio = page.find("id=\"loc-studio\"").unwrap_or(usize::MAX);
        assert!(living < studio && studio < usize::MAX, "{page}");
        assert_eq!(page.matches("class=\"ng-section\"").count(), 2, "{page}");
        // Each section holds exactly its own devices.
        let studio_section = &page[studio..];
        assert!(studio_section.contains("host-5") && !studio_section.contains("host-4"));

        // And with enrichment off the page is its empty state, which says why.
        let empty = render(
            "gather/query--ng_devices_by_location.html",
            "ng_devices_by_location",
            serde_json::json!([]),
        );
        assert!(empty.contains("UniFi enrichment"), "{empty}");
    }

    /// Tera comments are not markup, and this file's comments discuss the very
    /// markup the test above forbids.
    fn strip_tera_comments(template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{#") {
            out.push_str(&rest[..start]);
            match rest[start..].find("#}") {
                Some(end) => rest = &rest[start + end + 2..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// Every menu path must also be a path the *web* interface can serve. The
    /// alias test above proves the route exists; this one proves the migration
    /// that makes the menu visible at all is still shipped, since a menu nobody
    /// can see is the state this pass started from.
    #[test]
    fn the_manifest_ships_the_migration_that_reveals_the_menu() {
        let manifest = include_str!("../netgrasp.info.toml");
        let migration = include_str!("../migrations/005_netgrasp_web_interface.sql");
        assert!(
            manifest.contains("005_netgrasp_web_interface.sql"),
            "005_netgrasp_web_interface.sql is not in the manifest's migration list"
        );
        assert!(
            migration.contains(PERM_VIEW_DEVICES),
            "the migration no longer grants {PERM_VIEW_DEVICES}"
        );
    }

    /// The manifest's `api_version` must match the kernel this repo is pinned
    /// to, or the module is refused at load with a version mismatch and no page
    /// exists to debug.
    ///
    /// Checked against the Trovato version the workspace records in
    /// `[workspace.metadata.trovato]` rather than against a constant imported
    /// from the kernel, because the SDK does not export one: Trovato's version
    /// and its `KERNEL_API_VERSION` move in lock-step by its own versioning
    /// protocol. The recorded `rev` must also be the one the dependencies pin.
    /// So bumping the pinned `rev` without the recorded version, or the
    /// recorded version without the manifest, fails here.
    ///
    /// Before 1.0.0 this read `CARGO_PKG_VERSION`, because the workspace
    /// version was the kernel's. The plugin now has a version of its own, and
    /// the manifest's `version` must equal it.
    #[test]
    fn the_manifest_declares_the_pinned_kernels_api_version() {
        let manifest = include_str!("../netgrasp.info.toml");
        let workspace = include_str!("../../../Cargo.toml");

        // The table header on a line of its own: the comments above the
        // dependencies name the table in prose too.
        let field = |key: &str| {
            workspace
                .lines()
                .map(str::trim)
                .skip_while(|line| *line != "[workspace.metadata.trovato]")
                .skip(1)
                .take_while(|line| !line.starts_with('['))
                .find_map(|line| line.strip_prefix(key)?.trim().strip_prefix('='))
                .map(|value| value.trim().trim_matches('"').to_string())
                .unwrap_or_default()
        };
        let kernel = field("version");
        let rev = field("rev");
        assert!(
            !kernel.is_empty() && !rev.is_empty(),
            "Cargo.toml has no [workspace.metadata.trovato] version and rev"
        );

        let pin = format!("rev = \"{rev}\"");
        assert_eq!(
            workspace.matches(&pin).count(),
            3,
            "trovato-sdk, trovato-kernel and [workspace.metadata.trovato] must all name {rev}"
        );

        let mut parts = kernel.split('.');
        let major = parts.next().unwrap_or_default();
        let minor = parts.next().unwrap_or_default();
        let expected = format!("api_version = \"{major}.{minor}\"");
        assert!(
            manifest.contains(&expected),
            "manifest does not declare {expected} for Trovato {kernel}"
        );

        let own = format!("version = \"{}\"", env!("CARGO_PKG_VERSION"));
        assert!(
            manifest.lines().any(|line| line.trim() == own),
            "manifest does not declare the plugin's own {own}"
        );
    }

    /// The auto-reload is two files agreeing on one attribute name: the chrome
    /// writes `data-ng-refresh` onto the page element, and the static script
    /// reads it. Rename it in one of them and the pages stop reloading with no
    /// error anywhere — a wall display that quietly freezes is the worst failure
    /// this feature has.
    #[test]
    fn the_chrome_and_the_script_agree_on_the_refresh_attribute() {
        let chrome = include_str!("../../../templates/gather/netgrasp/page.html");
        let script = include_str!("../../../static/js/netgrasp.js");

        assert!(
            chrome.contains("data-ng-refresh=\"{{ ng_refresh }}\""),
            "the chrome no longer publishes the interval as data-ng-refresh"
        );
        assert!(
            chrome.contains("{% set ng_refresh = 10 %}"),
            "the chrome's default interval is no longer 10 seconds"
        );
        assert!(
            script.contains("getAttribute(\"data-ng-refresh\")"),
            "the script no longer reads data-ng-refresh"
        );
        assert!(
            script.contains("\"refresh\""),
            "the script no longer honours the ?refresh= override"
        );
        assert!(
            chrome.contains("data-ng-refresh-label"),
            "the chrome no longer carries the label the script fills in"
        );
        assert!(
            script.contains("data-ng-refresh-label"),
            "the script no longer fills in the interval label"
        );
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

    /// The SDK the module is built against and the kernel the integration test
    /// drives must be the same revision of Trovato.
    ///
    /// They are two entries in the workspace manifest, and nothing else would
    /// notice them drifting apart. A plugin compiled against one contract and
    /// exercised against another is a test that proves nothing about what ships:
    /// the module would be built against SDK types the running kernel no longer
    /// has, and the test would pass anyway because it never sees the mismatch.
    #[test]
    fn the_sdk_and_the_test_kernel_pin_the_same_trovato() {
        let manifest = include_str!("../../../Cargo.toml");
        let revs: Vec<&str> = manifest
            .lines()
            .filter(|line| {
                line.starts_with("trovato-sdk =") || line.starts_with("trovato-kernel =")
            })
            .filter_map(|line| line.split("rev = \"").nth(1))
            .filter_map(|rest| rest.split('"').next())
            .collect();

        assert_eq!(
            revs.len(),
            2,
            "expected a pinned rev on both trovato-sdk and trovato-kernel, found {revs:?}"
        );
        assert_eq!(
            revs[0], revs[1],
            "trovato-sdk and trovato-kernel pin different Trovato revisions"
        );
    }

    /// The `ng_device_state` record type is declared over a VIEW, and the view
    /// lists its columns explicitly — `SELECT d.*` would freeze today's column
    /// list in at CREATE VIEW time and silently omit anything added later. So the
    /// two lists have to agree, and nothing in the running system says so: a
    /// field mapped to a column the view does not select renders as a blank cell
    /// with no error anywhere.
    ///
    /// Read out of the manifest rather than restated, so adding a field is what
    /// this notices, not editing the test.
    #[test]
    fn the_owner_view_carries_every_column_the_record_type_maps() {
        let manifest = include_str!("../netgrasp.info.toml");
        let view = include_str!("../migrations/006_netgrasp_owner_names.sql");

        // The record type's backing relation is the view, not the daemon's table.
        assert!(
            manifest.contains("table = \"ng_devices_with_owner\""),
            "ng_device_state is no longer declared over the owner view"
        );
        assert!(
            manifest.contains("\"ng_devices_with_owner\""),
            "the view is not in db_tables, so the kernel will refuse the record type"
        );

        // Every physical column on the right-hand side of the ng_device_state
        // field map: the block after its [record_types.fields] header, up to the
        // next TOML section. Sections are the boundary, not blank lines and not
        // comments — the map has both inside it.
        let fields: String = manifest
            .split("[record_types.fields]")
            .nth(1)
            .unwrap_or_default()
            .lines()
            .take_while(|line| !line.trim_start().starts_with('['))
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        let mut checked = 0;
        for line in fields.lines() {
            let Some((_, column)) = line.split_once('=') else {
                continue;
            };
            let column = column.trim().trim_matches('"');
            if column.is_empty() {
                continue;
            }
            assert!(
                view.contains(&format!("d.{column},")) || column == "owner_name",
                "the view does not select {column}, which ng_device_state maps"
            );
            checked += 1;
        }
        assert!(
            checked >= 19,
            "only {checked} mapped columns were checked; the field map failed to parse"
        );
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

    // --- the self-standing demo -------------------------------------------
    //
    // `docker-compose.demo.yml` is the only path into this repository that needs
    // nothing but Docker: no Rust on the host, no Trovato checkout, no daemon and
    // no LAN. Nothing in the compose file is checked by a compiler, so a typo in a
    // search path is a demo that silently serves Trovato's own pages instead of
    // netgrasp's. These tests are what notices.

    /// The compose file, read as text.
    ///
    /// Scanned rather than parsed: a YAML parser would be a dependency added to
    /// a plugin whose whole point is a small wasm artifact, and every claim below
    /// is about a literal string an operator can also see by eye.
    fn demo_compose() -> &'static str {
        include_str!("../../../docker-compose.demo.yml")
    }

    fn demo_overlay_dockerfile() -> &'static str {
        include_str!("../../../docker/overlay.Dockerfile")
    }

    /// The value of a `KEY: value` line in the compose file, first occurrence,
    /// with a YAML anchor declaration (`&name `) stripped off the front.
    fn compose_value(key: &str) -> String {
        let raw = demo_compose()
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .unwrap_or_else(|| panic!("docker-compose.demo.yml declares no {key}"))
            .trim();
        let raw = match raw.strip_prefix('&') {
            Some(rest) => rest.split_once(' ').map(|(_, v)| v).unwrap_or(""),
            None => raw,
        };
        raw.trim().trim_matches('"').to_string()
    }

    /// One service's block, from its `  name:` key to the next key at that
    /// indent. Indentation is the whole structure of a compose file, so the
    /// block ends at the first line that starts a sibling key rather than at the
    /// first blank line or comment.
    fn compose_service(name: &str) -> String {
        let mut lines = demo_compose()
            .lines()
            .skip_while(|line| line.trim_end() != format!("  {name}:"));
        let key = lines
            .next()
            .unwrap_or_else(|| panic!("docker-compose.demo.yml has no {name} service"));
        std::iter::once(key)
            .chain(lines.take_while(|line| {
                line.trim().is_empty() || line.starts_with("   ") || line.starts_with("  #")
            }))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every volume mount in the compose file, as written.
    fn compose_mounts() -> Vec<&'static str> {
        demo_compose()
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix("- "))
            .filter(|value| value.contains(":/netgrasp/"))
            .collect()
    }

    /// The three search paths are the whole integration seam, and the demo is
    /// the one place they are written down as a deployment rather than as prose.
    /// Each must EXTEND the image's own directory rather than replace it (a
    /// lone `/netgrasp/templates` hides every Trovato base template and every
    /// page 500s), and netgrasp's directory must come LAST, because a later
    /// entry is the one that wins a name collision.
    #[test]
    fn the_demo_extends_each_of_the_three_search_paths_and_wins_the_collision() {
        for (var, dir) in [
            ("PLUGINS_DIR", "plugins"),
            ("TEMPLATES_DIR", "templates"),
            ("STATIC_DIR", "static"),
        ] {
            let value = compose_value(var);
            assert_eq!(
                value,
                format!("/app/{dir}:/netgrasp/{dir}"),
                "{var} must extend the image's /app/{dir} and put netgrasp's directory last"
            );
        }
    }

    /// The kernel the demo runs is a PINNED published release, and the manifest
    /// must be loadable by it.
    ///
    /// The kernel's rule (`PluginInfo::check_api_compatibility`) is plugin major
    /// == kernel major and plugin minor <= kernel minor, so this repository's
    /// `0.99` manifest runs unchanged on a `0.101` kernel. That is a fact worth
    /// pinning rather than rediscovering: the sibling test above ties
    /// `api_version` to the Trovato version the workspace pins, and without this
    /// one nothing says the released kernel in the demo can still load it.
    ///
    /// `latest` is rejected on purpose. A demo whose kernel changes underneath
    /// it is a demo that breaks with no commit to blame.
    #[test]
    fn the_demo_kernel_is_a_pinned_release_that_can_load_this_manifest() {
        let image = demo_compose()
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("image: ghcr.io/jeremyandrews/trovato:"))
            .expect("docker-compose.demo.yml runs no ghcr.io/jeremyandrews/trovato image")
            .trim();

        assert!(
            image != "latest" && image != "nightly",
            "the demo kernel must be a pinned version, not '{image}'"
        );

        let mut kernel = image.split('.');
        let kernel_major: u32 = kernel.next().unwrap().parse().expect("kernel major");
        let kernel_minor: u32 = kernel.next().unwrap().parse().expect("kernel minor");

        let manifest = include_str!("../netgrasp.info.toml");
        let declared = manifest
            .lines()
            .find_map(|line| line.trim().strip_prefix("api_version = "))
            .expect("the manifest declares no api_version")
            .trim()
            .trim_matches('"');
        let mut plugin = declared.split('.');
        let plugin_major: u32 = plugin.next().unwrap().parse().expect("plugin major");
        let plugin_minor: u32 = plugin.next().unwrap().parse().expect("plugin minor");

        assert_eq!(
            plugin_major, kernel_major,
            "api_version {declared} cannot load on kernel {image}: major mismatch"
        );
        assert!(
            plugin_minor <= kernel_minor,
            "api_version {declared} needs a newer kernel than {image}"
        );
    }

    /// Nothing in this repository is copied into Trovato, and the demo is where
    /// that rule is easiest to break: a writable mount is one `plugin install`
    /// away from the kernel copying a build artifact into it. All three of
    /// netgrasp's contributions are mounted read-only, and the assertion is
    /// per-mount rather than a count so a fourth mount cannot arrive unnoticed.
    #[test]
    fn the_demo_mounts_every_netgrasp_directory_read_only() {
        for mount in [
            "netgrasp-overlay:/netgrasp/plugins:ro",
            "./templates:/netgrasp/templates:ro",
            "./static:/netgrasp/static:ro",
        ] {
            assert!(
                demo_compose().contains(mount),
                "the demo does not mount {mount}"
            );
        }
        for mount in compose_mounts() {
            assert!(
                mount.ends_with(":ro"),
                "{mount} mounts a netgrasp directory writable"
            );
        }
    }

    /// The demo's overlay is built by the repository's own script, not by a
    /// second copy of its logic inside a Dockerfile. `build-overlay.sh` is where
    /// the layout `trovato plugin install` expects is decided, and it is also
    /// what runs `check-host-imports.sh` on the artifact it just assembled.
    /// A Dockerfile that ran `cargo build` and `cp` itself would drop that check
    /// and drift the layout.
    #[test]
    fn the_demo_builds_its_overlay_with_the_repositorys_own_script() {
        assert!(
            demo_overlay_dockerfile().contains("scripts/build-overlay.sh"),
            "docker/overlay.Dockerfile does not call scripts/build-overlay.sh"
        );
        assert!(
            !demo_overlay_dockerfile().contains("cargo build"),
            "docker/overlay.Dockerfile reimplements the build instead of calling the script"
        );
    }

    /// A brand new database has to be walked through three states in order:
    /// the kernel's own migrations, then the plugin's, then the demo rows. The
    /// kernel migrates from `plugin install` and registers the plugin's Item
    /// types when it next boots, and `item.type` is a foreign key onto
    /// `item_type`, so a seed that runs before that boot fails on its first
    /// `ng_person` INSERT.
    ///
    /// Install-then-serve in one command, and a seed that waits for the health
    /// check, is what orders those three states. Both halves are asserted
    /// because losing either one is a demo that comes up empty or not at all.
    #[test]
    fn the_demo_installs_the_plugin_before_it_serves_and_seeds_after_it_is_healthy() {
        assert!(
            demo_compose().contains("./trovato plugin install netgrasp && exec ./trovato serve"),
            "the demo kernel does not install the plugin before serving"
        );
        assert!(
            demo_compose().contains("scripts/seed-demo.sql")
                || demo_compose().contains("seed-demo.sql"),
            "the demo never loads scripts/seed-demo.sql"
        );
        let seed = compose_service("seed");
        assert!(
            seed.contains("trovato:") && seed.contains("condition: service_healthy"),
            "the seed does not wait for the kernel to be healthy"
        );
    }

    /// The kernel runs no scheduler: `tap_cron` fires only when something POSTs
    /// `/cron/<CRON_KEY>`. Without a poker the demo still serves every page,
    /// the seed writes `sync_state = 'clean'`, but nothing ever mints a device
    /// Item, prunes an expired event or clears a dirty row again, which is half
    /// of what the plugin does. The poker must use the same key the kernel is
    /// given, or every poke is a 404 nobody looks at.
    #[test]
    fn the_demo_pokes_the_cron_route_with_the_key_the_kernel_was_given() {
        assert!(
            demo_compose().contains("/cron/$$CRON_KEY"),
            "the demo has no cron poker, so tap_cron never fires"
        );
        assert!(
            compose_value("CRON_KEY").len() > 3,
            "the demo sets no CRON_KEY for the poker to use"
        );
    }

    /// The kernel gates the whole site behind a first-run wizard. Until
    /// `site_config` carries `installed`, `check_installation` answers every path
    /// except `/health`, `/static` and `/install` with a 303 to the installer, so
    /// a demo can install the plugin, seed it, serve it, come up healthy, and
    /// still show a stranger a form instead of a single netgrasp page. Observed,
    /// not hypothetical: that is exactly what the first version of this compose
    /// file did.
    ///
    /// The wizard is two form POSTs. A demo that promises one command has to make
    /// them, and then has to check that a real page serves afterwards, because a
    /// 303 to /install is a 200 as far as any health check is concerned.
    #[test]
    fn the_demo_walks_through_the_kernels_first_run_wizard() {
        let first_run = include_str!("../../../scripts/first-run.sh");
        for step in ["/install/admin", "/install/site"] {
            assert!(
                first_run.contains(step),
                "scripts/first-run.sh does not post {step}"
            );
        }
        // The overview specifically: it is the front page, and its gather is
        // the last one the migrations add, so a 200 from it means all of them ran.
        assert!(
            first_run.contains("\"$BASE/overview\""),
            "scripts/first-run.sh never checks that the front page serves"
        );
        assert!(
            demo_compose().contains("scripts/first-run.sh"),
            "the demo never completes the kernel's first-run wizard"
        );
    }

    /// Two ways to serve this repository, one first run. `serve-demo.sh` drives a
    /// Trovato checkout and the compose file drives the published image, and both
    /// meet the same wizard for the same reason. Sharing the script is what keeps
    /// the developer path from being the one nobody notices has broken.
    #[test]
    fn both_ways_of_serving_this_repository_share_one_first_run_script() {
        let serve_demo = include_str!("../../../scripts/serve-demo.sh");
        assert!(
            serve_demo.contains("first-run.sh"),
            "scripts/serve-demo.sh does not run scripts/first-run.sh"
        );
    }
}
