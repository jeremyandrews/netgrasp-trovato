//! The assistant scopes' host side: reads, writes, and the tool dispatch.
//!
//! What can be decided without a database is in [`netgrasp_core::assist`] and is
//! tested there. This is the part that cannot: resolving a reference against the
//! rows that exist, and carrying a write out through the same two statements an
//! admin's edit goes through.
//!
//! # Every write goes the long way round
//!
//! A device write is four steps and none of them is optional:
//!
//! 1. **Resolve the row**, and mint its Item if there is none. Only rows the
//!    daemon marked `dirty` ever get an Item from the cron sync, so a device that
//!    has been `clean` since before the plugin existed — or that the demo seed
//!    created — has `trovato_item_id NULL`, and the write-back addresses the row
//!    *by that link*. Writing without minting would update zero rows and report
//!    success.
//! 2. **Build the whole Item**, all five fields. `Item::update` reads `fields` as
//!    `input.fields.unwrap_or(current.fields)` and replaces it wholesale, so a
//!    partial `fields` object silently deletes every field it omits. A field the
//!    call did not name is carried forward from the Item, and from the device row
//!    where the Item has no such key — the sync used to mint Items carrying only
//!    `field_mac`, and reading those absent fields as their zero values is what
//!    made a rename turn a device's alerts off.
//! 3. **Apply the coercions `tap_item_presave` would have applied.** The plugin's
//!    own `save-item` bypasses `ItemService`, so no presave and no
//!    `tap_item_update` fire — which is what makes the sync loop terminate
//!    (`DESIGN.md` Drift 3) and what makes this function responsible for
//!    everything those taps would have done.
//! 4. **Write back the columns the call named, and no others**, through
//!    `netgrasp_core::writeback::build_partial_update`, which builds its `SET`
//!    list from the intersection of `columns::USER_OWNED` and the edit — so it
//!    can name neither a daemon column nor a user column nobody asked about.
//!    The card is built from that same list, so what somebody clicks Apply on is
//!    what happens.
//!
//! A person write is the same shape with `mirror_person` in place of the
//! write-back. Nothing here writes `sync_state`: the two statements that do are
//! in `sync_host.rs` and neither is reachable from a tool.
//!
//! # The permission is checked twice
//!
//! The kernel checks the scope's permission before it opens a conversation. Every
//! tool checks it again, at the moment of the call, because that is where the
//! change happens and a conversation outlives the request that opened it.

use netgrasp_core::assist::{
    self, DeviceFacts, DeviceFilter, DeviceRef, EventFacts, PersonCandidate, PersonFacts,
    PresenceWindowFacts, TimelineRow,
};
use netgrasp_core::model::{DeviceEdit, Span, SpanRow};
use netgrasp_core::{DEVICE_TYPE, PERSON_TYPE, queries};
use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::types::{AssistantContext, AssistantToolResult};

use crate::db::{now, query_rows};
use crate::{PERM_ADMINISTER, item_host, sync_host};

/// Scope name: one device.
pub const SCOPE_DEVICE: &str = "netgrasp_device";
/// Scope name: one person.
pub const SCOPE_PERSON: &str = "netgrasp_person";
/// Scope name: the network as a whole.
pub const SCOPE_NETWORK: &str = "netgrasp_network";

/// Rows any listing tool returns at most.
const LIST_LIMIT: i64 = 200;

/// What a tool says when the caller does not hold the permission.
const NO_PERMISSION: &str = "You do not have permission to change Netgrasp.";

/// Whether the caller may change anything.
///
/// The belt over the kernel's braces. Note it checks the permission
/// **literally**: the host's `current-user-has-permission` has no
/// `administer site` bypass, unlike every kernel route, so a site that wants an
/// administrator to use this has to grant `administer netgrasp` for real.
pub(crate) fn may_administer() -> bool {
    host::current_user_has_permission(PERM_ADMINISTER)
}

/// Refuse a call in one line.
fn refuse(message: impl Into<String>) -> AssistantToolResult {
    AssistantToolResult::failed(message)
}

// ===========================================================================
// Reads
// ===========================================================================

/// One device by however the instruction named it.
pub(crate) fn load_device(reference: &DeviceRef) -> Result<DeviceFacts, String> {
    let rows: Vec<DeviceFacts> = match reference {
        DeviceRef::Mac(mac) => query_rows(queries::SELECT_DEVICE_BY_MAC, &[json!(mac)]),
        DeviceRef::Id(id) => query_rows(queries::SELECT_DEVICE_BY_ID, &[json!(id)]),
    }
    .map_err(|e| format!("could not read the device: {e}"))?;

    rows.into_iter().next().ok_or_else(|| match reference {
        DeviceRef::Mac(mac) => format!("no device has the address {mac}"),
        DeviceRef::Id(id) => format!("no device has the id {id}"),
    })
}

/// One device by the Item that overlays it. This is how a device scope opens.
fn load_device_by_item(item_id: &str) -> Result<DeviceFacts, String> {
    let rows: Vec<DeviceFacts> = query_rows(queries::SELECT_DEVICE_BY_ITEM, &[json!(item_id)])
        .map_err(|e| format!("could not read the device: {e}"))?;
    rows.into_iter()
        .next()
        .ok_or_else(|| format!("no device row is linked to the item {item_id}"))
}

/// Everybody, with their device counts.
pub(crate) fn load_people() -> Result<Vec<PersonFacts>, String> {
    query_rows(queries::SELECT_PEOPLE_WITH_COUNTS, &[json!(LIST_LIMIT)])
        .map_err(|e| format!("could not read the people: {e}"))
}

/// One person by their Item id.
pub(crate) fn load_person(item_id: &str) -> Result<PersonFacts, String> {
    let rows: Vec<PersonFacts> = query_rows(queries::SELECT_PERSON_WITH_COUNT, &[json!(item_id)])
        .map_err(|e| format!("could not read the person: {e}"))?;
    rows.into_iter()
        .next()
        .ok_or_else(|| format!("no person has the id {item_id}"))
}

/// Resolve however the instruction named a person against everyone who exists.
fn resolve_person(raw: &str) -> Result<PersonCandidate, String> {
    let reference = assist::parse_person_ref(raw)?;
    let people = load_people()?;
    let candidates: Vec<PersonCandidate> = people
        .iter()
        .map(|p| PersonCandidate {
            item_id: p.item_id.clone(),
            name: p.name.clone(),
        })
        .collect();
    assist::resolve_person(&reference, &candidates)
}

/// Devices belonging to one person.
fn load_owned_devices(item_id: &str) -> Result<Vec<DeviceFacts>, String> {
    query_rows(
        queries::SELECT_DEVICES_BY_OWNER,
        &[json!(item_id), json!(LIST_LIMIT)],
    )
    .map_err(|e| format!("could not read the devices: {e}"))
}

/// Devices matching a listing filter.
fn load_devices(filter: &DeviceFilter) -> Result<Vec<DeviceFacts>, String> {
    match filter {
        DeviceFilter::All => query_rows(queries::SELECT_DEVICES_ALL, &[json!(LIST_LIMIT)]),
        DeviceFilter::Unowned => query_rows(queries::SELECT_DEVICES_UNOWNED, &[json!(LIST_LIMIT)]),
        DeviceFilter::Online => query_rows(
            queries::SELECT_DEVICES_BY_STATE,
            &[json!("online"), json!(LIST_LIMIT)],
        ),
        DeviceFilter::Hidden => query_rows(queries::SELECT_DEVICES_HIDDEN, &[json!(LIST_LIMIT)]),
        DeviceFilter::Owner(who) => {
            let person = resolve_person(who)?;
            return load_owned_devices(&person.item_id);
        }
    }
    .map_err(|e| format!("could not read the devices: {e}"))
}

/// A device's three timelines: presence, locations and events.
type Timelines = (Vec<TimelineRow>, Vec<TimelineRow>, Vec<TimelineRow>);

/// A device's presence, locations and events over a window.
fn load_history(device_id: i64, days: i64) -> Result<Timelines, String> {
    let limit = assist::HISTORY_ROWS as i64;
    let cutoff = now().map_err(|e| format!("could not read the clock: {e}"))? - days * 86_400;

    let presence: Vec<SpanRow> = query_rows(
        queries::SELECT_PRESENCE_SPANS,
        &[json!(device_id), json!(limit)],
    )
    .map_err(|e| format!("could not read the presence history: {e}"))?;
    let locations: Vec<SpanRow> = query_rows(
        queries::SELECT_LOCATION_SPANS,
        &[json!(device_id), json!(limit)],
    )
    .map_err(|e| format!("could not read the location history: {e}"))?;
    let events: Vec<EventFacts> = query_rows(
        queries::SELECT_DEVICE_EVENTS,
        &[json!(device_id), json!(cutoff), json!(limit)],
    )
    .map_err(|e| format!("could not read the events: {e}"))?;

    let window_start = cutoff;
    Ok((
        spans_to_rows(presence, window_start),
        spans_to_rows(locations, window_start),
        events.iter().map(EventFacts::row).collect(),
    ))
}

/// Timeline rows inside the window, newest first.
fn spans_to_rows(rows: Vec<SpanRow>, window_start: i64) -> Vec<TimelineRow> {
    rows.into_iter()
        .map(Span::from)
        .filter(|span| span.end.is_none_or(|end| end >= window_start))
        .map(|span| TimelineRow {
            label: span.label,
            start: Some(span.start),
            end: span.end,
            detail: None,
        })
        .collect()
}

// ===========================================================================
// Writes
// ===========================================================================

/// The Item overlaying a device, minting one when the row has none.
///
/// Step 1 of every device write, and the step that is invisible until it is
/// missing: a device the daemon has never marked `dirty` has no Item, and the
/// write-back addresses the row by the Item link.
fn ensure_device_item(facts: &DeviceFacts) -> Result<String, String> {
    if let Some(item_id) = facts.trovato_item_id.as_deref().filter(|id| !id.is_empty())
        && let Ok(Some(_)) = sync_host::load_item(item_id)
    {
        return Ok(item_id.to_string());
    }

    // The same title the cron sync would have given it, from the same function,
    // so a device named by this path and a device named by that one agree — and
    // the row's own user-owned values, for the same reason.
    let item_id = sync_host::create_device_item(&facts.mac, &facts.title(), &facts.overlay())
        .map_err(|e| format!("could not create the device's item: {e}"))?;
    sync_host::link_item(facts.id, &item_id)
        .map_err(|e| format!("could not link the device to its item: {e}"))?;
    Ok(item_id)
}

/// Apply an edit to a device: mint the Item if needed, save it, write back the
/// columns the edit named.
///
/// Four steps, as the module header says, and the last two are where the
/// asymmetry lives. The **Item** has to be saved whole, because `Item::update`
/// replaces `fields` wholesale and an omitted key is a deleted value. The
/// **row** must not be: an `UPDATE` naming a column the tool call never
/// mentioned is a change nobody asked for and the card did not show.
///
/// So the Item's unnamed fields are filled in from the Item itself, and from the
/// device row where the Item carries no such key — which, on an Item the cron
/// sync minted before this build, is every field but the MAC — and the
/// write-back names only [`DeviceEdit::columns`].
pub(crate) fn apply_device_edit(facts: &DeviceFacts, edit: &DeviceEdit) -> Result<u64, String> {
    let item_id = ensure_device_item(facts)?;
    let existing = sync_host::load_item(&item_id)
        .map_err(|e| format!("could not read the device's item: {e}"))?
        .ok_or_else(|| format!("the device's item {item_id} has gone"))?;

    let title = edit
        .display_name
        .clone()
        .or_else(|| {
            existing
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| facts.mac.clone());

    // The coercions `tap_item_presave` would have applied, applied here because
    // no presave fires on this path.
    let mac = assist::normalize_mac(
        &field_str(&existing, "field_mac").unwrap_or_else(|| facts.mac.clone()),
    );
    let mut fields =
        netgrasp_core::writeback::merged_device_fields(&mac, &existing, &facts.overlay(), edit);
    if let Some(owner) = fields.get_mut("field_owner")
        && owner
            .as_str()
            .is_some_and(|id| !id.is_empty() && !assist::is_uuid_shaped(id))
    {
        // An owner that is not a uuid would reach `owner_item_id`, a uuid
        // column, and fail the write-back with a cast error nobody sees.
        *owner = json!("");
    }

    let payload = json!({
        "id": item_id,
        "type": DEVICE_TYPE,
        "title": title,
        "status": existing.get("status").and_then(Value::as_i64).unwrap_or(1),
        "fields": fields,
    });

    item_host::save_item(&payload)
        .map_err(|code| format!("could not save the device's item (host error {code})"))?;
    sync_host::write_back_device_edit(&item_id, edit)
        .map_err(|e| format!("could not write the change back to the device: {e}"))
}

/// Save a person Item and mirror it.
pub(crate) fn apply_person_save(payload: &Value) -> Result<Value, String> {
    let saved = item_host::save_item(payload)
        .map_err(|code| format!("could not save the person (host error {code})"))?;
    sync_host::mirror_person(&saved)
        .map_err(|e| format!("could not mirror the person for the daemon: {e}"))?;
    Ok(saved)
}

/// A field of an Item as a string.
fn field_str(item: &Value, name: &str) -> Option<String> {
    netgrasp_core::writeback::field_str(item, name)
}

/// A field of an Item as a boolean.
fn field_bool(item: &Value, name: &str) -> bool {
    netgrasp_core::writeback::field_bool(item, name)
}

/// The Item behind a person, whole, for an edit that has to preserve its fields.
pub(crate) fn load_person_item(item_id: &str) -> Result<Value, String> {
    sync_host::load_item(item_id)
        .map_err(|e| format!("could not read the person: {e}"))?
        .ok_or_else(|| format!("no person item with the id {item_id}"))
}

/// The person Item payload for an edit, with every field carried forward.
pub(crate) fn person_payload(
    item_id: &str,
    existing: &Value,
    name: Option<&str>,
    notes: Option<&str>,
    notify: Option<(bool, bool)>,
) -> Value {
    let title = name
        .map(str::to_string)
        .or_else(|| {
            existing
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let notes = notes
        .map(str::to_string)
        .or_else(|| field_str(existing, "field_notes"))
        .unwrap_or_default();
    let (arrive, depart) = notify.unwrap_or_else(|| {
        (
            field_bool(existing, "field_notify_arrive"),
            field_bool(existing, "field_notify_depart"),
        )
    });

    json!({
        "id": item_id,
        "type": PERSON_TYPE,
        "title": title,
        "status": existing.get("status").and_then(Value::as_i64).unwrap_or(1),
        "fields": {
            "field_notes": notes,
            "field_notify_arrive": arrive,
            "field_notify_depart": depart,
        }
    })
}

// ===========================================================================
// The declarations
// ===========================================================================

/// The prefix every scope's prompt starts with.
///
/// It says what Netgrasp is and, more usefully, what it **cannot** know: it
/// identifies devices from what it sees on the wire and has no idea who is
/// holding one. A model not told that will happily infer an owner from a device
/// type, which is the single most confident wrong answer available here.
pub const SCOPE_PROMPT_PREFIX: &str = "\
You are configuring Netgrasp, a passive network monitor. It watches the LAN and records devices (identified by MAC address), when each device was present, where it was (which access point or location), and events. People are named entries a person creates; a device belongs to at most one person. Netgrasp identifies devices from what it sees; it cannot know who is holding one. Names a person typed win over names the daemon guessed. Timestamps in the context are UTC.
Devices are referenced by MAC address or by their numeric id; people by name (must be exact and unique) or by uuid.
Hidden devices are kept out of listings; notify controls arrival and departure alerts for that device or person.";

/// One scope's prompt: the shared prefix plus its own paragraph.
fn prompt(specific: &str) -> String {
    format!("{SCOPE_PROMPT_PREFIX}\n{specific}")
}

/// A `{days}` parameter schema.
fn days_schema(required: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "days": {
                "type": "integer",
                "description": "How far back to look, in days (1 to 90). Defaults to 7."
            }
        }
    });
    if required {
        schema["required"] = json!(["days"]);
    }
    schema
}

/// The `{device, days}` parameter schema the person and network scopes use.
fn device_days_schema() -> Value {
    json!({
        "type": "object",
        "required": ["device"],
        "properties": {
            "device": {"type": "string", "description": "A MAC address or a numeric device id."},
            "days": {
                "type": "integer",
                "description": "How far back to look, in days (1 to 90). Defaults to 7."
            }
        }
    })
}

/// The `find_devices` tool, which three scopes share.
fn find_devices_tool() -> trovato_sdk::types::AssistantTool {
    trovato_sdk::types::AssistantTool::read(
        "find_devices",
        "Search devices by name, hostname or MAC address. Use this to turn a \
         description into an identifier before doing anything else with it.",
    )
    .parameters(json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": {"type": "string", "description": "Part of a name, hostname or MAC address."}
        }
    }))
}

/// The `list_people` tool, which two scopes share.
fn list_people_tool() -> trovato_sdk::types::AssistantTool {
    trovato_sdk::types::AssistantTool::read(
        "list_people",
        "List everyone, with their uuid, how many devices are theirs, and whether \
         they are home.",
    )
}

/// Every scope Netgrasp offers.
pub fn scopes() -> Vec<trovato_sdk::types::AssistantScope> {
    use trovato_sdk::types::{AssistantIdKind, AssistantRisk, AssistantScope, AssistantTool};

    let device = AssistantScope::new(
        SCOPE_DEVICE,
        "Netgrasp device",
        PERM_ADMINISTER,
        AssistantIdKind::Item,
    )
    .description("Name a device, give it an owner, and see where it has been.")
    .item_types([DEVICE_TYPE])
    .prompt(prompt(
        "You are configuring ONE device, described in the context. Its owner, its \
         name, its notes and its two flags are yours to propose changes to; \
         everything else in the context is what the daemon observed and cannot be \
         changed here.",
    ))
    .suggestions([
        "Who owns this device?",
        "Show me when this device was online this week",
        "Rename this to something sensible",
    ])
    .tool(
        AssistantTool::read(
            "device_history",
            "This device's presence, locations and events over the last few days.",
        )
        .parameters(days_schema(false)),
    )
    .tool(list_people_tool())
    .tool(find_devices_tool())
    .tool(
        AssistantTool::write(
            "set_owner",
            "Give this device to a person, or to nobody. Pass null to unassign it.",
            AssistantRisk::Normal,
        )
        .parameters(json!({
            "type": "object",
            "properties": {
                "person": {
                    "type": "string",
                    "description": "A person's exact name or uuid, or null for nobody."
                }
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "rename",
            "Set this device's display name. The name a person types wins over the \
             one the daemon guessed.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["display_name"],
            "properties": {"display_name": {"type": "string"}}
        })),
    )
    .tool(
        AssistantTool::write(
            "set_notes",
            "Replace this device's notes. An empty string clears them.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["text"],
            "properties": {"text": {"type": "string"}}
        })),
    )
    .tool(
        AssistantTool::write(
            "set_flags",
            "Hide this device from listings, or turn its arrival and departure \
             alerts on or off. Omit a flag to leave it alone.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "properties": {
                "hidden": {"type": "boolean"},
                "notify": {"type": "boolean"}
            }
        })),
    );

    let person = AssistantScope::new(
        SCOPE_PERSON,
        "Netgrasp person",
        PERM_ADMINISTER,
        AssistantIdKind::Item,
    )
    .description("Manage one person and the devices that belong to them.")
    .item_types([PERSON_TYPE])
    .prompt(prompt(
        "You are configuring ONE person, described in the context. You can change \
         their name, their notes and their notification settings, move devices to \
         and from them, and delete them. Deleting a person is refused while any \
         device is still theirs: unassign the devices first.",
    ))
    .suggestions([
        "Which devices are theirs?",
        "Does this person have a device at all?",
        "Turn on arrival notifications",
    ])
    .tool(AssistantTool::read(
        "person_devices",
        "Every device that belongs to this person.",
    ))
    .tool(
        AssistantTool::read(
            "device_history",
            "One device's presence, locations and events over the last few days.",
        )
        .parameters(device_days_schema()),
    )
    .tool(find_devices_tool())
    .tool(
        AssistantTool::write(
            "assign_device",
            "Give a device to a person, or to nobody. The person may be someone \
             else: this is how a device is moved. Pass null to unassign it.",
            AssistantRisk::Normal,
        )
        .parameters(json!({
            "type": "object",
            "required": ["device"],
            "properties": {
                "device": {"type": "string", "description": "A MAC address or a numeric device id."},
                "person": {
                    "type": "string",
                    "description": "A person's exact name or uuid, or null for nobody."
                }
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "set_notify",
            "Set whether to be told when this person arrives and when they leave.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["arrive", "depart"],
            "properties": {
                "arrive": {"type": "boolean"},
                "depart": {"type": "boolean"}
            }
        })),
    )
    .tool(
        AssistantTool::write("rename", "Change this person's name.", AssistantRisk::Low)
            .parameters(json!({
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string"}}
            })),
    )
    .tool(
        AssistantTool::write(
            "set_notes",
            "Replace this person's notes. An empty string clears them.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["text"],
            "properties": {"text": {"type": "string"}}
        })),
    )
    .tool(AssistantTool::write(
        "delete_person",
        "Delete this person. Refused while any device is still theirs.",
        AssistantRisk::High,
    ));

    let network = AssistantScope::new(
        SCOPE_NETWORK,
        "Netgrasp network",
        PERM_ADMINISTER,
        AssistantIdKind::None,
    )
    .description("Everything on the network: who owns what, and who is home.")
    .prompt(prompt(
        "You are looking at the WHOLE network. The context lists every person and \
         every device, grouped by owner. Use it to answer questions about ownership \
         and presence, and propose changes when asked to tidy something up.",
    ))
    .suggestions([
        "Which devices have no owner?",
        "Which device is this person using, and who should own it?",
        "Who is home right now?",
    ])
    .tool(list_people_tool())
    .tool(
        AssistantTool::read(
            "list_devices",
            "List devices: all of them, or only the unowned, online or hidden ones, \
             or only one person's.",
        )
        .parameters(json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "all, unowned, online, hidden, or owner:<name or uuid>."
                }
            }
        })),
    )
    .tool(
        AssistantTool::read(
            "device_history",
            "One device's presence, locations and events over the last few days.",
        )
        .parameters(device_days_schema()),
    )
    .tool(
        AssistantTool::read(
            "who_was_online",
            "Which devices were on the network during a window, grouped by owner. \
             At most seven days.",
        )
        .parameters(json!({
            "type": "object",
            "required": ["from", "to"],
            "properties": {
                "from": {"type": "string", "description": "ISO 8601, e.g. 2026-08-24T09:00:00Z."},
                "to": {"type": "string", "description": "ISO 8601, later than `from`."}
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "assign_device",
            "Give a device to a person, or to nobody. Pass null to unassign it.",
            AssistantRisk::Normal,
        )
        .parameters(json!({
            "type": "object",
            "required": ["device"],
            "properties": {
                "device": {"type": "string", "description": "A MAC address or a numeric device id."},
                "person": {
                    "type": "string",
                    "description": "A person's exact name or uuid, or null for nobody."
                }
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "create_person",
            "Create a person devices can be assigned to.",
            AssistantRisk::Normal,
        )
        .parameters(json!({
            "type": "object",
            "required": ["name"],
            "properties": {"name": {"type": "string"}}
        })),
    )
    .tool(
        AssistantTool::write(
            "rename_device",
            "Set a device's display name.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["device", "display_name"],
            "properties": {
                "device": {"type": "string"},
                "display_name": {"type": "string"}
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "set_device_flags",
            "Hide a device from listings, or turn its alerts on or off.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["device"],
            "properties": {
                "device": {"type": "string"},
                "hidden": {"type": "boolean"},
                "notify": {"type": "boolean"}
            }
        })),
    )
    .tool(
        AssistantTool::write(
            "set_person_notify",
            "Set whether to be told when a person arrives and when they leave.",
            AssistantRisk::Low,
        )
        .parameters(json!({
            "type": "object",
            "required": ["person", "arrive", "depart"],
            "properties": {
                "person": {"type": "string"},
                "arrive": {"type": "boolean"},
                "depart": {"type": "boolean"}
            }
        })),
    );

    vec![device, person, network]
}

// ===========================================================================
// The context tap
// ===========================================================================

/// Describe whatever a conversation was opened on.
///
/// A read that fails degrades to a snapshot saying so rather than to no
/// conversation at all: the tools still work, and a model told "the overview
/// could not be read" asks for what it needs instead of inventing it.
pub fn context(request: &trovato_sdk::types::AssistantContextRequest) -> AssistantContext {
    let clock = now().unwrap_or(0);
    let scope_id = request.scope_id.clone().unwrap_or_default();

    match request.scope.as_str() {
        SCOPE_DEVICE => device_context(&scope_id, clock),
        SCOPE_PERSON => person_context(&scope_id, clock),
        SCOPE_NETWORK => network_context(clock),
        other => {
            AssistantContext::new("Netgrasp", format!("Netgrasp has no scope called {other}."))
        }
    }
}

fn device_context(item_id: &str, clock: i64) -> AssistantContext {
    let facts = match load_device_by_item(item_id) {
        Ok(facts) => facts,
        Err(e) => {
            return AssistantContext::new(
                "Netgrasp device",
                format!("The daemon has no row for this device yet. {e}"),
            )
            .link("Devices", "/devices");
        }
    };

    let (presence, locations, events) = load_history(facts.id, 30).unwrap_or_default();
    let snapshot = assist::render_device_snapshot(&facts, &presence, &locations, &events, clock);

    AssistantContext::new(facts.phrase(), snapshot)
        .link("Device page", format!("/item/{item_id}"))
        .link("All devices", "/devices")
}

fn person_context(item_id: &str, clock: i64) -> AssistantContext {
    let person = match load_person(item_id) {
        Ok(person) => person,
        Err(e) => {
            return AssistantContext::new(
                "Netgrasp person",
                format!("This person has no mirror row yet. {e}"),
            )
            .link("People", "/people");
        }
    };
    let devices = load_owned_devices(&person.item_id).unwrap_or_default();
    let snapshot = assist::render_person_snapshot(&person, &devices, clock);

    AssistantContext::new(person.name.clone(), snapshot)
        .link("Person page", format!("/item/{item_id}"))
        .link("All people", "/people")
}

fn network_context(clock: i64) -> AssistantContext {
    let people = load_people().unwrap_or_default();
    let devices = load_devices(&DeviceFilter::All).unwrap_or_default();
    let security = security_event_count(clock).unwrap_or(0);
    // When the data starts. A failed read degrades to `None`, which says
    // "nothing has been observed yet" — wrong in the same direction the rest of
    // this function degrades, and still better than the silence that had the
    // model explaining an empty window as an outage.
    let monitoring_since = sync_host::monitoring_start().unwrap_or_else(|e| {
        host::log(
            "warning",
            "netgrasp",
            &format!("network context: earliest observation: {e}"),
        );
        None
    });
    let snapshot =
        assist::render_network_snapshot(&people, &devices, security, monitoring_since, clock);

    AssistantContext::new("Netgrasp network", snapshot)
        .link("Devices", "/devices")
        .link("People", "/people")
        .link("Security events", "/events/security")
}

/// How many security events in the last 24 hours.
fn security_event_count(clock: i64) -> Result<i64, String> {
    #[derive(serde::Deserialize)]
    struct CountRow {
        event_count: i64,
    }
    let rows: Vec<CountRow> = query_rows(
        queries::SELECT_SECURITY_EVENT_COUNT,
        &[json!(clock - 86_400)],
    )
    .map_err(|e| format!("could not count the security events: {e}"))?;
    Ok(rows.first().map_or(0, |r| r.event_count))
}

// ===========================================================================
// The tool tap
// ===========================================================================

/// Answer one tool call.
///
/// Every branch checks the permission first — the kernel checked it when the
/// conversation opened, and a conversation outlives the request that opened it.
pub fn tool(call: &trovato_sdk::types::AssistantToolCall) -> AssistantToolResult {
    if !may_administer() {
        return refuse(NO_PERMISSION);
    }
    let describing = call.mode == trovato_sdk::types::AssistantToolMode::Describe;
    let scope_id = call.scope_id.clone().unwrap_or_default();

    let outcome = match (call.scope.as_str(), call.tool.as_str()) {
        // Reads. Dispatched only with mode Execute by the kernel, so there is
        // no describe branch: a read has nothing to propose.
        (_, "list_people") => list_people(),
        (_, "find_devices") => find_devices(&call.arguments),
        (SCOPE_DEVICE, "device_history") => device_history_here(&scope_id, &call.arguments),
        (_, "device_history") => device_history_there(&call.arguments),
        (SCOPE_PERSON, "person_devices") => person_devices(&scope_id),
        (SCOPE_NETWORK, "list_devices") => list_devices(&call.arguments),
        (SCOPE_NETWORK, "who_was_online") => who_was_online(&call.arguments),

        // Writes.
        (SCOPE_DEVICE, "set_owner") => set_owner(&scope_id, &call.arguments, describing),
        (SCOPE_DEVICE, "rename") => rename_device_here(&scope_id, &call.arguments, describing),
        (SCOPE_DEVICE, "set_notes") => set_device_notes(&scope_id, &call.arguments, describing),
        (SCOPE_DEVICE, "set_flags") => {
            set_device_flags_here(&scope_id, &call.arguments, describing)
        }

        (SCOPE_PERSON, "assign_device") => {
            assign_device(&call.arguments, Some(&scope_id), describing)
        }
        (SCOPE_PERSON, "set_notify") => {
            set_person_notify_here(&scope_id, &call.arguments, describing)
        }
        (SCOPE_PERSON, "rename") => rename_person(&scope_id, &call.arguments, describing),
        (SCOPE_PERSON, "set_notes") => set_person_notes(&scope_id, &call.arguments, describing),
        (SCOPE_PERSON, "delete_person") => delete_person(&scope_id, describing),

        (SCOPE_NETWORK, "assign_device") => assign_device(&call.arguments, None, describing),
        (SCOPE_NETWORK, "create_person") => create_person(&call.arguments, describing),
        (SCOPE_NETWORK, "rename_device") => rename_device_there(&call.arguments, describing),
        (SCOPE_NETWORK, "set_device_flags") => set_device_flags_there(&call.arguments, describing),
        (SCOPE_NETWORK, "set_person_notify") => {
            set_person_notify_there(&call.arguments, describing)
        }

        (scope, tool) => Err(format!("{scope} has no tool called {tool}")),
    };

    match outcome {
        Ok(result) => result,
        Err(message) => refuse(message),
    }
}

// --- reads ----------------------------------------------------------------

fn list_people() -> Result<AssistantToolResult, String> {
    let people = load_people()?;
    if people.is_empty() {
        return Ok(AssistantToolResult::ok(
            "Nobody exists yet.",
            "No people are defined.",
        ));
    }
    let mut out = format!("{} people:\n", people.len());
    for person in &people {
        out.push_str(&person.render_line());
    }
    Ok(AssistantToolResult::ok(
        assist::cap(&out, assist::RESULT_MAX_BYTES),
        format!("Listed {} people", people.len()),
    ))
}

fn find_devices(arguments: &Value) -> Result<AssistantToolResult, String> {
    let query = assist::require_str(arguments, "query")?;
    let devices: Vec<DeviceFacts> = query_rows(
        queries::SELECT_DEVICES_BY_NAME,
        &[json!(query), json!(LIST_LIMIT)],
    )
    .map_err(|e| format!("could not search the devices: {e}"))?;
    Ok(render_device_list(
        &devices,
        &format!("matching '{query}'"),
        now().unwrap_or(0),
    ))
}

fn list_devices(arguments: &Value) -> Result<AssistantToolResult, String> {
    let raw = assist::optional_str(arguments, "filter")?.unwrap_or_else(|| "all".to_string());
    let filter = assist::parse_device_filter(&raw)?;
    let devices = load_devices(&filter)?;
    Ok(render_device_list(
        &devices,
        &format!("({raw})"),
        now().unwrap_or(0),
    ))
}

fn person_devices(item_id: &str) -> Result<AssistantToolResult, String> {
    let person = load_person(item_id)?;
    let devices = load_owned_devices(item_id)?;
    Ok(render_device_list(
        &devices,
        &format!("belonging to {}", person.name),
        now().unwrap_or(0),
    ))
}

fn render_device_list(devices: &[DeviceFacts], what: &str, clock: i64) -> AssistantToolResult {
    if devices.is_empty() {
        return AssistantToolResult::ok(
            format!("No devices {what}."),
            format!("No devices {what}"),
        );
    }
    let mut out = format!("{} devices {what}:\n", devices.len());
    for device in devices {
        out.push_str(&device.render_line(clock, true));
        if let Some(owner) = device.owner() {
            out.push_str(&format!("    owner: {}\n", person_line(&owner)));
        }
    }
    AssistantToolResult::ok(
        assist::cap(&out, assist::RESULT_MAX_BYTES),
        format!("Listed {} devices {what}", devices.len()),
    )
}

fn person_line(person: &PersonCandidate) -> String {
    format!("{} ({})", person.name, person.item_id)
}

fn device_history_here(item_id: &str, arguments: &Value) -> Result<AssistantToolResult, String> {
    let facts = load_device_by_item(item_id)?;
    device_history(&facts, arguments)
}

fn device_history_there(arguments: &Value) -> Result<AssistantToolResult, String> {
    let reference = assist::parse_device_ref(&assist::require_str(arguments, "device")?)?;
    let facts = load_device(&reference)?;
    device_history(&facts, arguments)
}

fn device_history(facts: &DeviceFacts, arguments: &Value) -> Result<AssistantToolResult, String> {
    let days = assist::parse_days(arguments, "days", 7)?;
    let clock = now().unwrap_or(0);
    let (presence, locations, events) = load_history(facts.id, days)?;

    let mut out = format!("{} over the last {days} days:\n\n", facts.phrase());
    out.push_str(&assist::render_timeline("Presence", &presence, clock));
    out.push('\n');
    out.push_str(&assist::render_timeline("Locations", &locations, clock));
    out.push('\n');
    out.push_str(&assist::render_timeline("Events", &events, clock));

    Ok(AssistantToolResult::ok(
        assist::cap(&out, assist::RESULT_MAX_BYTES),
        format!(
            "{} presence spans, {} location stays and {} events for {} over {days} days",
            presence.len(),
            locations.len(),
            events.len(),
            facts.label()
        ),
    ))
}

fn who_was_online(arguments: &Value) -> Result<AssistantToolResult, String> {
    let from = assist::require_str(arguments, "from")?;
    let to = assist::require_str(arguments, "to")?;
    let (start, end) = assist::parse_window(&from, &to)?;

    let spans: Vec<PresenceWindowFacts> = query_rows(
        queries::SELECT_PRESENCE_WINDOW,
        &[json!(start), json!(end), json!(assist::MAX_WINDOW_ROWS)],
    )
    .map_err(|e| format!("could not read the presence history: {e}"))?;

    let rendered = assist::render_who_was_online(&spans, now().unwrap_or(0));
    Ok(AssistantToolResult::ok(
        rendered,
        format!(
            "{} presence spans between {} and {}",
            spans.len(),
            assist::format_utc(start),
            assist::format_utc(end)
        ),
    ))
}

// --- writes ---------------------------------------------------------------
//
// Every one of these has the same shape: work out what would happen, build the
// edit, build the sentence, and then either stop (Describe) or do it (Execute).
// The sentence is built from the SAME `DeviceEdit` the write uses — before the
// Describe branch, not after it — so a card cannot describe one change and apply
// another, and `described` names the columns that edit will write.

/// Assign a device to somebody, or to nobody.
///
/// `default_person` is the person scope's own id: in that scope the tool exists
/// to move a device **to** the person being configured, so an omitted `person`
/// means them rather than nobody. In the network scope there is no such default
/// and an omitted `person` is an unassign, which is what `null` says too.
fn assign_device(
    arguments: &Value,
    default_person: Option<&str>,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let reference = assist::parse_device_ref(&assist::require_str(arguments, "device")?)?;
    let facts = load_device(&reference)?;

    let requested = assist::optional_str(arguments, "person")?;
    let has_key = arguments.get("person").is_some();
    let new_owner = match (requested, has_key, default_person) {
        (Some(raw), _, _) => Some(resolve_person(&raw)?),
        // An explicit null is an unassign in either scope.
        (None, true, _) => None,
        // Omitted, in the person scope: the person being configured.
        (None, false, Some(item_id)) => Some(person_candidate(item_id)?),
        (None, false, None) => None,
    };

    let current = facts.owner();
    let description = assist::describe_assign(
        &facts.descriptive(),
        &facts.mac,
        new_owner.as_ref().map(|p| p.name.as_str()),
        current.as_ref().map(|p| p.name.as_str()),
    );
    let edit = DeviceEdit {
        owner_item_id: Some(new_owner.as_ref().map(|p| p.item_id.clone())),
        ..DeviceEdit::default()
    };

    if describing {
        return Ok(described(&facts, &description, &edit));
    }

    apply_device_edit(&facts, &edit)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        match new_owner {
            Some(person) => format!("{} now belongs to {}", facts.phrase(), person.name),
            None => format!("{} now belongs to nobody", facts.phrase()),
        },
    ))
}

/// One person by their Item id, as a candidate.
fn person_candidate(item_id: &str) -> Result<PersonCandidate, String> {
    let person = load_person(item_id)?;
    Ok(PersonCandidate {
        item_id: person.item_id,
        name: person.name,
    })
}

/// What a Describe returns: the sentence, what will change, and a note when the
/// write will have to create the device's Item first.
///
/// **What will change** is [`DeviceEdit::columns`] — the same list the statement
/// builder builds its `SET` clause from — rendered in the words the tools use.
/// The card is the whole basis on which somebody clicks Apply, and a card that
/// said "Rename this device" while the write also turned its alerts off is
/// precisely what happened (`docs/JOINT-RUN.md`, plugin finding 1). Reading one
/// value twice is what makes the displayed change set and the executed change
/// set the same thing rather than two claims that can disagree.
///
/// Minting is said too: it is a visible side effect (the device appears in the
/// content listing and gets a page), and somebody applying a rename should not
/// discover it by accident.
fn described(facts: &DeviceFacts, description: &str, edit: &DeviceEdit) -> AssistantToolResult {
    let changes = assist::describe_change_set(&edit.columns());
    let mints = facts
        .trovato_item_id
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if mints {
        AssistantToolResult::ok(
            format!("{description}. {changes}. This also creates its Trovato item."),
            format!("{description}. {changes} (creates its Trovato item)"),
        )
    } else {
        AssistantToolResult::ok(
            format!("{description}. {changes}."),
            format!("{description}. {changes}"),
        )
    }
}

fn rename_device_here(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let facts = load_device_by_item(item_id)?;
    rename_device(&facts, arguments, describing)
}

fn rename_device_there(arguments: &Value, describing: bool) -> Result<AssistantToolResult, String> {
    let reference = assist::parse_device_ref(&assist::require_str(arguments, "device")?)?;
    let facts = load_device(&reference)?;
    rename_device(&facts, arguments, describing)
}

fn rename_device(
    facts: &DeviceFacts,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let new_name = assist::require_str(arguments, "display_name")?;
    let description = assist::describe_rename_device(&facts.mac, &facts.label(), &new_name);
    let edit = DeviceEdit {
        display_name: Some(new_name.clone()),
        ..DeviceEdit::default()
    };

    if describing {
        return Ok(described(facts, &description, &edit));
    }

    apply_device_edit(facts, &edit)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        format!("{} is now called '{new_name}'", facts.mac),
    ))
}

fn set_device_notes(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let facts = load_device_by_item(item_id)?;
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "'text' is required and must be a string".to_string())?
        .to_string();
    let description = assist::describe_set_notes(&facts.descriptive(), &facts.mac, &text);
    let edit = DeviceEdit {
        notes: Some(text.clone()),
        ..DeviceEdit::default()
    };

    if describing {
        return Ok(described(&facts, &description, &edit));
    }

    apply_device_edit(&facts, &edit)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        if text.trim().is_empty() {
            format!("Cleared the notes on {}", facts.phrase())
        } else {
            format!("Updated the notes on {}", facts.phrase())
        },
    ))
}

fn set_device_flags_here(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let facts = load_device_by_item(item_id)?;
    set_device_flags(&facts, arguments, describing)
}

fn set_device_flags_there(
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let reference = assist::parse_device_ref(&assist::require_str(arguments, "device")?)?;
    let facts = load_device(&reference)?;
    set_device_flags(&facts, arguments, describing)
}

fn set_device_flags(
    facts: &DeviceFacts,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let hidden = assist::optional_bool(arguments, "hidden")?;
    let notify = assist::optional_bool(arguments, "notify")?;
    if hidden.is_none() && notify.is_none() {
        return Err("set the `hidden` flag, the `notify` flag, or both".to_string());
    }
    let description = assist::describe_set_flags(&facts.descriptive(), &facts.mac, hidden, notify);
    let edit = DeviceEdit {
        hidden,
        notify,
        ..DeviceEdit::default()
    };

    if describing {
        return Ok(described(facts, &description, &edit));
    }

    apply_device_edit(facts, &edit)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        format!("Updated the flags on {}", facts.phrase()),
    ))
}

fn set_person_notify_here(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let person = load_person(item_id)?;
    set_person_notify(&person, arguments, describing)
}

fn set_person_notify_there(
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let who = assist::require_str(arguments, "person")?;
    let candidate = resolve_person(&who)?;
    let person = load_person(&candidate.item_id)?;
    set_person_notify(&person, arguments, describing)
}

fn set_person_notify(
    person: &PersonFacts,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let arrive = assist::require_bool(arguments, "arrive")?;
    let depart = assist::require_bool(arguments, "depart")?;
    let description = assist::describe_set_notify(&person.name, arrive, depart);

    if describing {
        return Ok(AssistantToolResult::ok(
            format!("{description}."),
            description,
        ));
    }

    let existing = load_person_item(&person.item_id)?;
    let payload = person_payload(
        &person.item_id,
        &existing,
        None,
        None,
        Some((arrive, depart)),
    );
    apply_person_save(&payload)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        description,
    ))
}

fn rename_person(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let person = load_person(item_id)?;
    let new_name = assist::require_str(arguments, "name")?;
    let description = assist::describe_rename_person(&person.name, &new_name);

    if describing {
        return Ok(AssistantToolResult::ok(
            format!("{description}."),
            description,
        ));
    }

    let existing = load_person_item(item_id)?;
    let payload = person_payload(item_id, &existing, Some(&new_name), None, None);
    apply_person_save(&payload)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        format!("{} is now called '{new_name}'", person.name),
    ))
}

fn set_person_notes(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let person = load_person(item_id)?;
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "'text' is required and must be a string".to_string())?
        .to_string();
    let description = assist::describe_set_person_notes(&person.name, &text);

    if describing {
        return Ok(AssistantToolResult::ok(
            format!("{description}."),
            description,
        ));
    }

    let existing = load_person_item(item_id)?;
    let payload = person_payload(item_id, &existing, None, Some(&text), None);
    apply_person_save(&payload)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        description,
    ))
}

fn create_person(arguments: &Value, describing: bool) -> Result<AssistantToolResult, String> {
    let name = assist::require_str(arguments, "name")?;
    let description = assist::describe_create_person(&name);

    if describing {
        // Refused at describe as well as at execute, because a card saying
        // "Create person 'Jamie'" when Jamie already exists is a card somebody
        // applies without thinking.
        let people = load_people()?;
        if let Some(existing) = people
            .iter()
            .find(|p| p.name.trim().eq_ignore_ascii_case(name.trim()))
        {
            return Err(format!(
                "'{}' already exists ({}). Use them instead of creating a second one.",
                existing.name, existing.item_id
            ));
        }
        return Ok(AssistantToolResult::ok(
            format!("{description}."),
            description,
        ));
    }

    let payload = json!({
        "type": PERSON_TYPE,
        "title": name,
        "status": 1,
        "fields": {
            "field_notes": "",
            "field_notify_arrive": false,
            "field_notify_depart": false,
        }
    });
    let saved = apply_person_save(&payload)?;
    let item_id = saved
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(AssistantToolResult::ok(
        format!("{description}. Their uuid is {item_id}."),
        format!("Created {name} ({item_id})"),
    ))
}

/// Delete a person, once nothing is theirs.
///
/// Refused while any device still names them, at Execute as well as at Describe:
/// time passes between the two, and the check that matters is the one made when
/// the delete happens. Deleting anyway would leave every one of those devices
/// pointing at an id with nothing behind it — a state the demo seed carries on
/// purpose, precisely because it is what this refuses to create more of.
fn delete_person(item_id: &str, describing: bool) -> Result<AssistantToolResult, String> {
    let person = load_person(item_id)?;
    let owned = owned_device_count(item_id)?;
    let description = assist::describe_delete_person(&person.phrase(), owned as usize);

    if describing {
        return Ok(AssistantToolResult::ok(
            if owned == 0 {
                format!("{description}.")
            } else {
                format!("{description}. Applying this will be refused until they are unassigned.")
            },
            description,
        ));
    }

    if owned > 0 {
        return Err(format!(
            "{} still has {owned} device{} assigned. Unassign them first.",
            person.name,
            if owned == 1 { "" } else { "s" }
        ));
    }

    // retire_person, then delete_item: `delete-item` calls `Item::delete`
    // directly and fires no `tap_item_delete`, so the mirror row and the owner
    // columns would otherwise be left behind.
    sync_host::retire_person(item_id).map_err(|e| format!("could not retire the person: {e}"))?;
    item_host::delete_item(item_id)
        .map_err(|code| format!("could not delete the person (host error {code})"))?;

    Ok(AssistantToolResult::ok(
        format!("Deleted {}.", person.name),
        format!("Deleted {}", person.name),
    ))
}

/// How many devices name this person as owner.
fn owned_device_count(item_id: &str) -> Result<i64, String> {
    #[derive(serde::Deserialize)]
    struct CountRow {
        device_count: i64,
    }
    let rows: Vec<CountRow> = query_rows(queries::SELECT_OWNED_DEVICE_COUNT, &[json!(item_id)])
        .map_err(|e| format!("could not count the person's devices: {e}"))?;
    Ok(rows.first().map_or(0, |r| r.device_count))
}

fn set_owner(
    item_id: &str,
    arguments: &Value,
    describing: bool,
) -> Result<AssistantToolResult, String> {
    let facts = load_device_by_item(item_id)?;
    let requested = assist::optional_str(arguments, "person")?;
    let new_owner = match requested {
        Some(raw) => Some(resolve_person(&raw)?),
        None => None,
    };
    let current = facts.owner();
    let description = assist::describe_assign(
        &facts.descriptive(),
        &facts.mac,
        new_owner.as_ref().map(|p| p.name.as_str()),
        current.as_ref().map(|p| p.name.as_str()),
    );

    let edit = DeviceEdit {
        owner_item_id: Some(new_owner.as_ref().map(|p| p.item_id.clone())),
        ..DeviceEdit::default()
    };

    if describing {
        return Ok(described(&facts, &description, &edit));
    }

    apply_device_edit(&facts, &edit)?;

    Ok(AssistantToolResult::ok(
        format!("{description}. Done."),
        match new_owner {
            Some(person) => format!("{} now belongs to {}", facts.phrase(), person.name),
            None => format!("{} now belongs to nobody", facts.phrase()),
        },
    ))
}
