//! Everything the assistant scopes decide **without a host**.
//!
//! The three scopes (`netgrasp_device`, `netgrasp_person`, `netgrasp_network`)
//! are declared and answered in the plugin, which is where the database and the
//! Item store are. What can be settled without either lives here: how a device
//! or a person is named in an instruction, whether a tool's arguments make
//! sense, what a snapshot reads like, and what a proposal card says.
//!
//! Two of those are worth arguing for.
//!
//! **Reference resolution.** A person says "the Amazon tablet" and a model has to
//! turn that into something the plugin can address. It cannot: the identifiers
//! are a MAC or a numeric row id for a device, and a uuid or an exactly-matching
//! name for a person, and nothing else. That narrowness is deliberate — a model
//! that can guess which device you meant will eventually guess wrong on a write.
//! So the reference rules are a small pure function with an exhaustive test, the
//! read tools exist to let the model *find* the identifier first, and an
//! ambiguous name is an error that names every candidate rather than a pick.
//!
//! **Describe strings.** Every write proposal's card is one sentence, and it is
//! the entire basis on which somebody decides to apply it. Each one names the
//! thing by its display name *and* its MAC, names the person, and states the
//! value being replaced — "Assign Amazon tablet (02:00:5e:00:00:04) to Jamie
//! (currently Arlo)". A card that said "Assign device to Jamie" would be a card
//! nobody could check.

use crate::model::DeviceRow;

/// Longest snapshot any scope renders, in bytes.
///
/// Well under the kernel's own default cap and far under the 64 KiB tap buffer,
/// because the tap's whole JSON return has to fit in that buffer and a snapshot
/// is only part of it.
pub const SNAPSHOT_MAX_BYTES: usize = 10_000;

/// Longest tool result any tool returns, in bytes.
pub const RESULT_MAX_BYTES: usize = 12_000;

/// Rows of history any one list renders.
pub const HISTORY_ROWS: usize = 10;

/// Largest window `who_was_online` will look at, in days.
pub const MAX_WINDOW_DAYS: i64 = 7;

/// Largest number of spans `who_was_online` will return.
pub const MAX_WINDOW_ROWS: i64 = 200;

/// Largest history window a device history tool will look at, in days.
pub const MAX_HISTORY_DAYS: i64 = 90;

// =============================================================================
// References
// =============================================================================

/// How an instruction named a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRef {
    /// A hardware address, normalized to lower-case colon-separated form.
    Mac(String),
    /// The `ng_devices.id` row identity.
    Id(i64),
}

/// Normalize a MAC to lower-case colon-separated form.
///
/// Accepts colons, dashes, dots or nothing between the octets, in any case, so
/// `AA-BB-CC-DD-EE-FF`, `aabb.ccdd.eeff` and `aa:bb:cc:dd:ee:ff` are one device
/// rather than three. Anything that is not twelve hex digits is returned
/// trimmed and lower-cased, so a caller can still compare it and report it.
#[must_use]
pub fn normalize_mac(raw: &str) -> String {
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

/// Whether a string is twelve hex digits' worth of MAC.
#[must_use]
pub fn looks_like_mac(raw: &str) -> bool {
    raw.chars().filter(char::is_ascii_hexdigit).count() == 12
        && raw
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '-' || c == '.')
}

/// Parse how an instruction named a device.
///
/// # Errors
///
/// A sentence naming what a device reference may be, for the model to read.
pub fn parse_device_ref(raw: &str) -> Result<DeviceRef, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("a device must be named by its MAC address or its numeric id".to_string());
    }
    if looks_like_mac(raw) {
        return Ok(DeviceRef::Mac(normalize_mac(raw)));
    }
    if let Ok(id) = raw.parse::<i64>()
        && id > 0
    {
        return Ok(DeviceRef::Id(id));
    }
    Err(format!(
        "'{raw}' is not a device reference: use the MAC address (aa:bb:cc:dd:ee:ff) \
         or the numeric id. Call find_devices to look one up."
    ))
}

/// How an instruction named a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonRef {
    /// An `ng_person` Item id.
    Uuid(String),
    /// A name, to be matched exactly and case-insensitively.
    Name(String),
}

/// Whether a string is shaped like a uuid. Shape only; the database parses.
#[must_use]
pub fn is_uuid_shaped(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Parse how an instruction named a person.
///
/// # Errors
///
/// A sentence for the model when the reference is empty.
pub fn parse_person_ref(raw: &str) -> Result<PersonRef, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("a person must be named by their name or their uuid".to_string());
    }
    if is_uuid_shaped(raw) {
        return Ok(PersonRef::Uuid(raw.to_lowercase()));
    }
    Ok(PersonRef::Name(raw.to_string()))
}

/// One person the plugin found, for resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonCandidate {
    /// The `ng_person` Item id.
    pub item_id: String,
    /// Their name.
    pub name: String,
}

/// Resolve a person reference against every person that exists.
///
/// A name must match exactly, ignoring case, and must match **one** person. An
/// ambiguous name names every candidate rather than picking: two people called
/// Sam is a situation a person can resolve in one sentence and a model cannot
/// resolve at all.
///
/// # Errors
///
/// A sentence for the model: no match, or the list of candidates.
pub fn resolve_person(
    reference: &PersonRef,
    candidates: &[PersonCandidate],
) -> Result<PersonCandidate, String> {
    match reference {
        PersonRef::Uuid(id) => candidates
            .iter()
            .find(|c| c.item_id.eq_ignore_ascii_case(id))
            .cloned()
            .ok_or_else(|| format!("no person has the id {id}")),
        PersonRef::Name(name) => {
            let matches: Vec<&PersonCandidate> = candidates
                .iter()
                .filter(|c| same_name(&c.name, name))
                .collect();
            match matches.len() {
                1 => Ok(matches[0].clone()),
                0 => Err(format!(
                    "no person is called '{name}'. Call list_people to see who exists."
                )),
                _ => Err(format!(
                    "'{name}' matches {} people: {}. Use the uuid instead.",
                    matches.len(),
                    matches
                        .iter()
                        .map(|c| format!("{} ({})", c.name, c.item_id))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
    }
}

/// A person's name as it is compared: surrounding space dropped, and case
/// folded.
///
/// One definition for every place that asks "is this somebody who already
/// exists": the owner form's new-person field, Add person, the bulk form, and
/// the assistant's `create_person`. Folding is Unicode lower-casing rather than
/// ASCII, so "ÉLODIE" and "élodie" are one person, as they are to a reader.
/// Inner spaces are left alone: "Mary Ann" and "Maryann" are two names.
#[must_use]
pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Whether two names name the same person, by [`name_key`].
#[must_use]
pub fn same_name(a: &str, b: &str) -> bool {
    name_key(a) == name_key(b)
}

/// The owner a possessive mDNS name announces: "Jeremy's Mac Studio" gives
/// "Jeremy".
///
/// Apple devices advertise their owner this way, and a good share of a
/// household's devices do it (22 of 166 on the live site, `docs/UI.md`). It is
/// a **suggestion** only, for the bulk form to offer: one of the live names is
/// "Me", so the plugin never assigns from it without a person choosing.
///
/// Accepts the straight and the typographic apostrophe, because macOS writes
/// the second. Returns `None` for a name with no possessive, for a possessive
/// with nothing before it, and for an owner longer than a name plausibly is.
#[must_use]
pub fn possessive_owner(mdns_name: &str) -> Option<String> {
    let name = mdns_name.trim();
    let at = name.find("'s ").or_else(|| name.find("\u{2019}s "))?;
    let owner = name[..at].trim();
    if owner.is_empty() || owner.chars().count() > 40 {
        return None;
    }
    Some(owner.to_string())
}

// =============================================================================
// Argument validation
// =============================================================================

/// A required string argument, trimmed and non-empty.
///
/// # Errors
///
/// A sentence for the model naming the argument.
pub fn require_str(arguments: &serde_json::Value, key: &str) -> Result<String, String> {
    match arguments.get(key).and_then(serde_json::Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(_) => Err(format!("'{key}' cannot be empty")),
        None => Err(format!("'{key}' is required and must be a string")),
    }
}

/// An optional string argument. `null` and an absent key are both `None`.
///
/// This is how "unassign" is expressed: `person: null` on `set_owner` and
/// `assign_device` means "nobody", which is a legitimate value rather than a
/// missing one.
///
/// # Errors
///
/// A sentence when the key is present and is not a string or null.
pub fn optional_str(arguments: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            Ok((!value.is_empty()).then(|| value.to_string()))
        }
        Some(_) => Err(format!("'{key}' must be a string or null")),
    }
}

/// An optional boolean argument.
///
/// # Errors
///
/// A sentence when the key is present and is not a boolean.
pub fn optional_bool(arguments: &serde_json::Value, key: &str) -> Result<Option<bool>, String> {
    match arguments.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

/// A required boolean argument.
///
/// # Errors
///
/// A sentence when the key is absent or is not a boolean.
pub fn require_bool(arguments: &serde_json::Value, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        _ => Err(format!("'{key}' is required and must be true or false")),
    }
}

/// A day count, defaulted and range-checked.
///
/// # Errors
///
/// A sentence naming the range.
pub fn parse_days(arguments: &serde_json::Value, key: &str, default: i64) -> Result<i64, String> {
    let raw = match arguments.get(key) {
        None | Some(serde_json::Value::Null) => return Ok(default),
        Some(value) => value,
    };
    let days = raw
        .as_i64()
        .ok_or_else(|| format!("'{key}' must be a whole number of days"))?;
    if !(1..=MAX_HISTORY_DAYS).contains(&days) {
        return Err(format!("'{key}' must be between 1 and {MAX_HISTORY_DAYS}"));
    }
    Ok(days)
}

/// A `who_was_online` window: two ISO 8601 instants, in order, at most
/// [`MAX_WINDOW_DAYS`] apart.
///
/// # Errors
///
/// A sentence for the model: an unparseable instant, a backwards window, or one
/// wider than the cap.
pub fn parse_window(from: &str, to: &str) -> Result<(i64, i64), String> {
    let start =
        parse_iso8601(from).ok_or_else(|| format!("'from' is not an ISO 8601 time: {from}"))?;
    let end = parse_iso8601(to).ok_or_else(|| format!("'to' is not an ISO 8601 time: {to}"))?;
    if end <= start {
        return Err("'to' must be after 'from'".to_string());
    }
    if end - start > MAX_WINDOW_DAYS * 86_400 {
        return Err(format!(
            "the window is wider than {MAX_WINDOW_DAYS} days; ask about a shorter one"
        ));
    }
    Ok((start, end))
}

/// The filter `list_devices` accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceFilter {
    /// Every device.
    All,
    /// Devices with no owner.
    Unowned,
    /// Devices the daemon last saw online.
    Online,
    /// Devices the daemon counts as idle: present recently, quiet now.
    Idle,
    /// Devices the daemon counts as gone.
    Offline,
    /// Devices an admin hid from the listings.
    Hidden,
    /// Devices belonging to one person.
    Owner(String),
    /// Devices whose OUI vendor contains this, ignoring case.
    Vendor(String),
    /// Devices whose type contains this, ignoring case.
    Type(String),
}

/// What [`parse_device_filter`] says when it does not recognise a filter, and
/// what a capped listing tells the model it can narrow with.
pub const DEVICE_FILTERS: &str = "all, online, idle, offline, unowned, hidden, \
     owner:<person>, vendor:<name>, type:<name>";

/// Parse a `list_devices` filter.
///
/// # Errors
///
/// A sentence naming the accepted values.
pub fn parse_device_filter(raw: &str) -> Result<DeviceFilter, String> {
    let raw = raw.trim();
    for (prefix, make) in [
        ("owner:", DeviceFilter::Owner as fn(String) -> DeviceFilter),
        ("vendor:", DeviceFilter::Vendor),
        ("type:", DeviceFilter::Type),
    ] {
        if let Some(value) = raw.strip_prefix(prefix) {
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("'{prefix}' needs a value after it"));
            }
            return Ok(make(value.to_string()));
        }
    }
    match raw.to_ascii_lowercase().as_str() {
        "" | "all" => Ok(DeviceFilter::All),
        "unowned" => Ok(DeviceFilter::Unowned),
        "online" => Ok(DeviceFilter::Online),
        "idle" => Ok(DeviceFilter::Idle),
        "offline" => Ok(DeviceFilter::Offline),
        "hidden" => Ok(DeviceFilter::Hidden),
        _ => Err(format!("'{raw}' is not a filter: use {DEVICE_FILTERS}")),
    }
}

impl DeviceFilter {
    /// The six parameters of [`crate::queries::SELECT_DEVICES_FILTERED`] and
    /// [`crate::queries::SELECT_DEVICE_COUNTS`], in order: state, vendor
    /// fragment, type fragment, unowned only, hidden only, owner Item id.
    ///
    /// `owner_item_id` is the person an [`DeviceFilter::Owner`] filter resolved
    /// to; resolving a name needs the people, which only the host has.
    #[must_use]
    pub fn sql_params(&self, owner_item_id: Option<&str>) -> Vec<serde_json::Value> {
        use serde_json::json;
        let (state, vendor, kind, unowned, hidden) = match self {
            Self::All | Self::Owner(_) => ("", "", "", false, false),
            Self::Unowned => ("", "", "", true, false),
            Self::Online => ("online", "", "", false, false),
            Self::Idle => ("idle", "", "", false, false),
            Self::Offline => ("offline", "", "", false, false),
            Self::Hidden => ("", "", "", false, true),
            Self::Vendor(v) => ("", v.as_str(), "", false, false),
            Self::Type(t) => ("", "", t.as_str(), false, false),
        };
        let owner = match self {
            Self::Owner(_) => owner_item_id.unwrap_or("00000000-0000-0000-0000-000000000000"),
            _ => "",
        };
        vec![
            json!(state),
            json!(vendor),
            json!(kind),
            json!(unowned),
            json!(hidden),
            json!(owner),
        ]
    }
}

// =============================================================================
// Naming
// =============================================================================

/// The naming inputs, as the four name columns plus the MAC.
///
/// A small adapter so the two labelling functions below ask
/// [`crate::sync::observed_name`] rather than re-listing the ladder. There is
/// one precedence in this plugin and it lives in `sync`; a second copy here is
/// exactly how the Item title and the proposal card came to disagree about a
/// printer's name.
fn inputs<'a>(
    display_name: Option<&'a str>,
    resolved_name: Option<&'a str>,
    hostname: Option<&'a str>,
    mdns_name: Option<&'a str>,
    mac: &'a str,
) -> crate::sync::TitleInputs<'a> {
    crate::sync::TitleInputs {
        display_name,
        resolved_name,
        hostname,
        mdns_name,
        vendor: None,
        mac,
    }
}

/// What to call a device, in a sentence a person reads.
///
/// The ladder is [`crate::sync::observed_name`] — the name a human typed, then
/// the daemon's resolved name, then its hostname, then its mDNS name — ending
/// at the MAC, which always exists, so this always answers.
#[must_use]
pub fn device_label(
    display_name: Option<&str>,
    resolved_name: Option<&str>,
    hostname: Option<&str>,
    mdns_name: Option<&str>,
    mac: &str,
) -> String {
    crate::sync::observed_name(&inputs(
        display_name,
        resolved_name,
        hostname,
        mdns_name,
        mac,
    ))
    .unwrap_or(mac)
    .to_string()
}

/// The label for a [`DeviceRow`], using the columns it carries.
#[must_use]
pub fn row_label(row: &DeviceRow) -> String {
    crate::sync::observed_name(&row.into())
        .unwrap_or(&row.mac)
        .to_string()
}

/// What to call a device on a **proposal card**, where a bare MAC is not enough.
///
/// The listing ladder above ends at the MAC, which is right for a list: the MAC
/// is beside every other column and the reader has context. A proposal card has
/// no context — it is one sentence, and "Assign 02:00:5e:00:00:04 to Jamie" asks
/// somebody to approve a change to a device they cannot picture.
///
/// So when nothing on the ladder produced a name, this falls back to what the
/// daemon *did* observe: the vendor's first word and the device type, which
/// turns that sentence into "Assign Amazon tablet (02:00:5e:00:00:04) to Jamie".
/// A vendor with no type gives "Amazon device"; a type with no vendor gives the
/// type; neither gives the MAC, and the MAC is in the phrase either way.
#[must_use]
pub fn descriptive_label(
    display_name: Option<&str>,
    resolved_name: Option<&str>,
    hostname: Option<&str>,
    mdns_name: Option<&str>,
    vendor: Option<&str>,
    device_type: Option<&str>,
    mac: &str,
) -> String {
    if let Some(name) = crate::sync::observed_name(&inputs(
        display_name,
        resolved_name,
        hostname,
        mdns_name,
        mac,
    )) {
        return name.to_string();
    }

    // The vendor's first word: "Amazon Technologies" and "Apple, Inc." are how
    // an OUI database says it, and neither is how a person does.
    let brand = vendor
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.split([' ', ',']).find(|word| !word.is_empty()))
        .map(str::to_string);
    let kind = device_type
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);

    match (brand, kind) {
        (Some(brand), Some(kind)) => format!("{brand} {kind}"),
        (Some(brand), None) => format!("{brand} device"),
        (None, Some(kind)) => kind,
        (None, None) => mac.to_string(),
    }
}

/// How a device is named in a proposal: its label and its MAC, always both.
///
/// Both, because the label is what a person recognizes and the MAC is what makes
/// it unambiguous — and when the label *is* the MAC, saying it twice would be
/// noise, so it is said once.
#[must_use]
pub fn device_phrase(label: &str, mac: &str) -> String {
    if label.eq_ignore_ascii_case(mac) {
        mac.to_string()
    } else {
        format!("{label} ({mac})")
    }
}

// =============================================================================
// Describe strings
// =============================================================================

/// "Assign X (mac) to Y" / "… (currently Z)" / "Unassign X (mac) from Z".
#[must_use]
pub fn describe_assign(
    device: &str,
    mac: &str,
    new_owner: Option<&str>,
    current_owner: Option<&str>,
) -> String {
    let phrase = device_phrase(device, mac);
    match (new_owner, current_owner) {
        (Some(new), Some(current)) if new == current => {
            format!("Leave {phrase} assigned to {current}")
        }
        (Some(new), Some(current)) => format!("Assign {phrase} to {new} (currently {current})"),
        (Some(new), None) => format!("Assign {phrase} to {new}"),
        (None, Some(current)) => format!("Unassign {phrase} from {current}"),
        (None, None) => format!("Leave {phrase} unassigned"),
    }
}

/// "Rename X from 'a' to 'b'".
#[must_use]
pub fn describe_rename_device(mac: &str, current: &str, new: &str) -> String {
    format!("Rename {mac} from '{current}' to '{new}'")
}

/// "Set the notes on X (mac)" / "Clear the notes on X (mac)".
#[must_use]
pub fn describe_set_notes(device: &str, mac: &str, notes: &str) -> String {
    let phrase = device_phrase(device, mac);
    if notes.trim().is_empty() {
        format!("Clear the notes on {phrase}")
    } else {
        format!("Set the notes on {phrase} to '{}'", ellipsize(notes, 60))
    }
}

/// "Hide X (mac) from listings", "Turn notifications on for X (mac)", or both.
#[must_use]
pub fn describe_set_flags(
    device: &str,
    mac: &str,
    hidden: Option<bool>,
    notify: Option<bool>,
) -> String {
    let phrase = device_phrase(device, mac);
    let mut parts: Vec<String> = Vec::new();
    if let Some(hidden) = hidden {
        parts.push(
            if hidden {
                "hide it from listings"
            } else {
                "show it in listings"
            }
            .to_string(),
        );
    }
    if let Some(notify) = notify {
        parts.push(
            if notify {
                "turn arrival and departure alerts on"
            } else {
                "turn arrival and departure alerts off"
            }
            .to_string(),
        );
    }
    if parts.is_empty() {
        return format!("Change nothing about {phrase}");
    }
    format!("For {phrase}: {}", parts.join(", and "))
}

/// "Create person 'X'".
#[must_use]
pub fn describe_create_person(name: &str) -> String {
    format!("Create person '{name}'")
}

/// "Rename person X to 'Y'".
#[must_use]
pub fn describe_rename_person(current: &str, new: &str) -> String {
    format!("Rename person '{current}' to '{new}'")
}

/// "Delete person X (no devices)" / "… (3 devices still assigned)".
#[must_use]
pub fn describe_delete_person(name: &str, device_count: usize) -> String {
    match device_count {
        0 => format!("Delete person {name} (no devices)"),
        1 => format!("Delete person {name} (1 device still assigned)"),
        n => format!("Delete person {name} ({n} devices still assigned)"),
    }
}

/// "Notify when X arrives and when they leave", and the other three cases.
#[must_use]
pub fn describe_set_notify(name: &str, arrive: bool, depart: bool) -> String {
    let what = match (arrive, depart) {
        (true, true) => "when they arrive and when they leave",
        (true, false) => "only when they arrive",
        (false, true) => "only when they leave",
        (false, false) => "never",
    };
    format!("Notify about {name} {what}")
}

/// What a proposal card says it will change, column by column.
///
/// The card is the whole basis on which somebody clicks Apply, so it has to
/// name the columns the write will touch and no others. It is built from
/// [`crate::model::DeviceEdit::columns`] — the same list
/// [`crate::writeback::build_partial_update`] builds its `SET` clause from — so
/// the displayed change set and the executed change set are one value read
/// twice rather than two descriptions that can disagree. They did disagree: a
/// rename's card said it would rename a device, and the write also turned its
/// alerts off (`docs/JOINT-RUN.md`, plugin finding 1).
///
/// Column names are given in the words the tools use, because the reader of a
/// card is a person and `owner_item_id` is not a thing they were offered.
#[must_use]
pub fn describe_change_set(columns: &[&str]) -> String {
    if columns.is_empty() {
        return "Changes nothing".to_string();
    }
    let named: Vec<&str> = columns
        .iter()
        .map(|column| match *column {
            "display_name" => "the name",
            "owner_item_id" => "the owner",
            "notes" => "the notes",
            "hidden" => "whether it is hidden",
            "notify" => "the arrival and departure alerts",
            "counts_for_presence" => "whether it counts for who is home",
            // A column outside the user-owned set cannot reach here from a
            // `DeviceEdit`, and if one ever does the card says so rather than
            // quietly leaving it off the list.
            other => other,
        })
        .collect();
    format!("Changes {}, and nothing else", join_clauses(&named))
}

/// "a", "a and b", "a, b and c".
fn join_clauses(parts: &[&str]) -> String {
    match parts {
        [] => String::new(),
        [only] => (*only).to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// "Set the notes on X" / "Clear the notes on X".
#[must_use]
pub fn describe_set_person_notes(name: &str, notes: &str) -> String {
    if notes.trim().is_empty() {
        format!("Clear the notes on {name}")
    } else {
        format!("Set the notes on {name} to '{}'", ellipsize(notes, 60))
    }
}

// =============================================================================
// Time
// =============================================================================

/// Parse an ISO 8601 instant into unix seconds.
///
/// Hand-rolled rather than pulled from a crate because this core is compiled to
/// `wasm32-wasip1` and every dependency it takes ships in the module. It accepts
/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH:MM:SS`, an optional
/// fractional part, and an optional `Z` or `+HH:MM` offset. Anything else is
/// `None`, which the caller turns into a sentence naming the format.
#[must_use]
pub fn parse_iso8601(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let (date, rest) = raw.split_once(['T', ' ']).unwrap_or((raw, ""));

    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next().unwrap_or("1").parse().ok()?;
    let day: i64 = parts.next().unwrap_or("1").parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Split the offset off the time before parsing it.
    let (time, offset_secs) = if let Some(stripped) = rest.strip_suffix('Z') {
        (stripped, 0)
    } else if let Some((time, offset)) = rest.rsplit_once('+') {
        (time, -parse_offset(offset)?)
    } else if rest.matches('-').count() == 1
        && let Some((time, offset)) = rest.rsplit_once('-')
    {
        (time, parse_offset(offset)?)
    } else {
        (rest, 0)
    };

    let mut clock = time.split(':');
    let hour: i64 = match clock.next() {
        Some("") | None => 0,
        Some(value) => value.parse().ok()?,
    };
    let minute: i64 = clock.next().unwrap_or("0").parse().ok()?;
    // Discard a fractional second rather than rejecting it: it is never material
    // to a presence window measured in minutes.
    let second: i64 = clock
        .next()
        .unwrap_or("0")
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    Some(
        days_from_civil(year, month, day) * 86_400
            + hour * 3600
            + minute * 60
            + second
            + offset_secs,
    )
}

/// `HH:MM` or `HHMM` as a count of seconds.
fn parse_offset(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let (hours, minutes) = match raw.split_once(':') {
        Some((h, m)) => (h, m),
        None if raw.len() == 4 => raw.split_at(2),
        None => (raw, "0"),
    };
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    Some(hours * 3600 + minutes * 60)
}

/// Days since the unix epoch for a civil date. Howard Hinnant's algorithm.
#[must_use]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A civil date from days since the unix epoch. The inverse of the above.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A unix second as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// The snapshot says UTC on every timestamp rather than converting: the daemon
/// writes UTC, the kernel has no timezone for the reader, and a model told
/// "09:00" with no zone will state a wrong local time with total confidence.
#[must_use]
pub fn format_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// "about 3 hours ago", "1 day 13 hours ago", "just now", "in 2 minutes".
///
/// **Rounded, never truncated.** The old phrase divided and dropped the
/// remainder, so a device first seen 37 hours ago read "1 day ago" and the model
/// answered "joined in the last 24 hours" from it (issue #32): it reasons from
/// the relative phrase, not the UTC time beside it. So:
///
/// - under an hour, minutes rounded to the nearest minute;
/// - under a day, hours rounded to the nearest hour, with "about" whenever the
///   rounding moved it, so 1 hour 56 minutes is "about 2 hours ago";
/// - from a day on, whole days **and** the remaining hours, rounded to the
///   nearest hour, so 37 hours is "1 day 13 hours ago" and never "1 day ago".
#[must_use]
pub fn relative(epoch: i64, now: i64) -> String {
    let delta = now - epoch;
    let magnitude = delta.abs();
    if magnitude < 60 {
        return if delta < 0 {
            "in under a minute".to_string()
        } else {
            "just now".to_string()
        };
    }
    let phrase = relative_magnitude(magnitude);
    if delta < 0 {
        format!("{phrase} from now")
    } else {
        format!("{phrase} ago")
    }
}

/// "1 day 13 hours", "about 2 hours", "5 minutes": a duration of at least a
/// minute, rounded as [`relative`] describes.
fn relative_magnitude(seconds: i64) -> String {
    fn unit(count: i64, name: &str) -> String {
        format!("{count} {name}{}", if count == 1 { "" } else { "s" })
    }
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        // 59.5 minutes and up has already rounded to an hour.
        return unit(minutes, "minute");
    }
    let hours = (seconds + 1_800) / 3_600;
    // Under a day stays in hours even when it rounds up to 24: 23 hours 40
    // minutes is "about 24 hours", which a model will not read as more than a
    // day, where "1 day" might be.
    if seconds < 86_400 {
        let about = if seconds % 3_600 == 0 { "" } else { "about " };
        return format!("{about}{}", unit(hours, "hour"));
    }
    let (days, rest) = (hours / 24, hours % 24);
    if rest == 0 {
        unit(days, "day")
    } else {
        format!("{} {}", unit(days, "day"), unit(rest, "hour"))
    }
}

/// A timestamp as the snapshots write one: absolute, then relative.
#[must_use]
pub fn stamp(epoch: Option<i64>, now: i64) -> String {
    match epoch {
        Some(epoch) => format!("{} ({})", format_utc(epoch), relative(epoch, now)),
        None => "never".to_string(),
    }
}

// =============================================================================
// Rendering helpers
// =============================================================================

/// Truncate to `max` bytes at a line boundary and say so.
///
/// A line boundary, because a snapshot is labelled lines and half a line is a
/// half-fact: "last seen: 2026-08-2" is worse than no line at all.
#[must_use]
pub fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let cut = head.rfind('\n').map(|i| i + 1).unwrap_or(head.len());
    format!("{}\n[truncated]", head[..cut].trim_end())
}

/// Shorten a phrase for a one-line description.
#[must_use]
pub fn ellipsize(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim().replace('\n', " ");
    if trimmed.chars().count() <= max_chars {
        return trimmed;
    }
    let kept: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A labelled line, or nothing at all when the value is absent.
///
/// Absent rather than "none" for the identity lines: a snapshot listing eight
/// fields as "none" reads as a device nobody knows anything about, and the model
/// spends a turn asking about it.
#[must_use]
pub fn line(label: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => format!("{label}: {value}\n"),
        None => String::new(),
    }
}

/// A labelled line that is written even when the value is absent.
#[must_use]
pub fn line_or(label: &str, value: Option<&str>, absent: &str) -> String {
    format!(
        "{label}: {}\n",
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(absent)
    )
}

/// A yes/no line.
#[must_use]
pub fn flag_line(label: &str, value: bool) -> String {
    format!("{label}: {}\n", if value { "yes" } else { "no" })
}

/// A person, as the network and person snapshots name one.
#[must_use]
pub fn person_phrase(name: &str, item_id: &str) -> String {
    format!("{name} ({item_id})")
}

/// When the data starts, and what time it is now.
///
/// Two sentences a model cannot work out for itself and will otherwise
/// invent an explanation for. Asked "who was online yesterday" against a
/// database an hour old, the model correctly found zero presence spans and then
/// speculated about "a gap in monitoring" — because nothing in its context said
/// the daemon's earliest observation was that morning
/// (`docs/JOINT-RUN.md`, plugin finding 3).
///
/// An empty window before the first observation and an empty window over a
/// monitored period are the same zero rows and different answers. This is what
/// tells them apart, so it says so in as many words rather than leaving the
/// inference to be made.
///
/// The current time is here for the same reason: "yesterday" is not a value the
/// daemon stores, and a model given presence data with no clock has to guess
/// which day it is in order to ask about the right one.
#[must_use]
pub fn render_monitoring_window(earliest: Option<i64>, now: i64) -> String {
    let mut out = String::new();
    match earliest {
        Some(earliest) => out.push_str(&format!(
            "Monitoring data begins at {}. There is no data before that, so a \
             question about an earlier time has no answer rather than a gap in \
             monitoring.\n",
            stamp(Some(earliest), now)
        )),
        None => out.push_str(
            "Monitoring data begins at: nothing has been observed yet, so every \
             window is empty for that reason.\n",
        ),
    }
    out.push_str(&format!("Current time: {}\n", format_utc(now)));
    out
}

// =============================================================================
// Snapshots
// =============================================================================

/// One device, as every assistant read decodes it.
///
/// One struct for all nine device statements, because they project identically
/// (`queries::device_read!`) and a second struct would be a second place for a
/// column to go missing. Every field is `Option` for the reason the daemon's own
/// row struct is: the daemon fills columns in as it learns them, and a device it
/// has only ever seen an ARP packet from has a MAC and nothing else.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DeviceFacts {
    /// `ng_devices.id`.
    pub id: i64,
    /// Hardware address.
    pub mac: String,
    /// The name a human typed.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The daemon's resolved name.
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Reverse-DNS hostname.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name.
    #[serde(default)]
    pub mdns_name: Option<String>,
    /// OUI vendor.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Daemon's classification.
    #[serde(default)]
    pub device_type: Option<String>,
    /// Daemon's OS guess.
    #[serde(default)]
    pub os_family: Option<String>,
    /// `online` / `offline` / `idle` / `new`.
    #[serde(default)]
    pub state: Option<String>,
    /// Most recent IPv4.
    #[serde(default)]
    pub last_ip: Option<String>,
    /// Most recent IPv6.
    #[serde(default)]
    pub last_ipv6: Option<String>,
    /// Access point last seen on.
    #[serde(default)]
    pub current_ap: Option<String>,
    /// Location last seen at.
    #[serde(default)]
    pub current_location: Option<String>,
    /// The admin's notes.
    #[serde(default)]
    pub notes: Option<String>,
    /// Hidden from listings.
    #[serde(default)]
    pub hidden: bool,
    /// Arrival and departure alerts on.
    #[serde(default)]
    pub notify: bool,
    /// The owning `ng_person` Item id.
    #[serde(default)]
    pub owner_item_id: Option<String>,
    /// The owner's name, resolved from the mirror. `None` when the owner id
    /// names a person whose Item was deleted, which is a real state.
    #[serde(default)]
    pub owner_name: Option<String>,
    /// The `ng_device` Item overlaying this row, once one exists.
    #[serde(default)]
    pub trovato_item_id: Option<String>,
    /// First observation, unix seconds.
    #[serde(default)]
    pub first_seen: Option<i64>,
    /// Most recent observation, unix seconds.
    #[serde(default)]
    pub last_seen: Option<i64>,
}

impl DeviceFacts {
    /// The naming inputs this row carries, vendor included.
    #[must_use]
    pub fn title_inputs(&self) -> crate::sync::TitleInputs<'_> {
        crate::sync::TitleInputs {
            display_name: self.display_name.as_deref(),
            resolved_name: self.resolved_name.as_deref(),
            hostname: self.hostname.as_deref(),
            mdns_name: self.mdns_name.as_deref(),
            vendor: self.vendor.as_deref(),
            mac: &self.mac,
        }
    }

    /// The title this device's Item should carry.
    ///
    /// [`crate::sync::device_title`], the same function the sync's mint and its
    /// title refresh use, so a device named by this path and a device named by
    /// that one agree.
    #[must_use]
    pub fn title(&self) -> String {
        crate::sync::device_title(&self.title_inputs())
    }

    /// The user-owned overlay this row carries, every column named.
    ///
    /// Fully named, unlike [`crate::model::DeviceRow::overlay`], because the
    /// assistant's device reads project the whole user-owned set: there is no
    /// column here that was not read, so "no notes" is a fact about the device
    /// rather than a gap in the projection. It is what an edit falls back to for
    /// a field the device's Item never carried.
    #[must_use]
    pub fn overlay(&self) -> crate::model::DeviceEdit {
        crate::model::DeviceEdit {
            display_name: self.display_name.clone(),
            owner_item_id: Some(self.owner_item_id.clone()),
            notes: Some(self.notes.clone().unwrap_or_default()),
            hidden: Some(self.hidden),
            notify: Some(self.notify),
            // Not read by the assistant's device projection; left unnamed so
            // nothing is inferred for it.
            counts_for_presence: None,
        }
    }

    /// What to call this device in a listing, where the MAC is beside it.
    #[must_use]
    pub fn label(&self) -> String {
        device_label(
            self.display_name.as_deref(),
            self.resolved_name.as_deref(),
            self.hostname.as_deref(),
            self.mdns_name.as_deref(),
            &self.mac,
        )
    }

    /// What to call this device on a proposal card, where it is all the reader
    /// has.
    #[must_use]
    pub fn descriptive(&self) -> String {
        descriptive_label(
            self.display_name.as_deref(),
            self.resolved_name.as_deref(),
            self.hostname.as_deref(),
            self.mdns_name.as_deref(),
            self.vendor.as_deref(),
            self.device_type.as_deref(),
            &self.mac,
        )
    }

    /// The device named the way a proposal names one.
    #[must_use]
    pub fn phrase(&self) -> String {
        device_phrase(&self.descriptive(), &self.mac)
    }

    /// The owner, when the mirror resolved a name for one.
    #[must_use]
    pub fn owner(&self) -> Option<PersonCandidate> {
        match (self.owner_item_id.as_ref(), self.owner_name.as_ref()) {
            (Some(item_id), Some(name)) => Some(PersonCandidate {
                item_id: item_id.clone(),
                name: name.clone(),
            }),
            _ => None,
        }
    }

    /// How the snapshot writes the owner line: a name and a uuid, an
    /// unresolvable id, or "none".
    #[must_use]
    pub fn owner_phrase(&self) -> String {
        match (self.owner_item_id.as_deref(), self.owner_name.as_deref()) {
            (Some(item_id), Some(name)) => person_phrase(name, item_id),
            // An id with no person behind it: the seed carries one on purpose,
            // and saying "none" here would be a lie the model would repeat.
            (Some(item_id), None) => format!("{item_id} (no person with that id)"),
            _ => "none".to_string(),
        }
    }

    /// One listing line: name, MAC, optionally type, state, last seen.
    #[must_use]
    pub fn render_line(&self, now: i64, with_type: bool) -> String {
        let mut line = format!("  {} — {}", self.label(), self.mac);
        if with_type && let Some(kind) = self.device_type.as_deref().filter(|k| !k.is_empty()) {
            line.push_str(&format!(", {kind}"));
        }
        line.push_str(&format!(
            ", {}",
            self.state.as_deref().unwrap_or("state unknown")
        ));
        line.push_str(&format!(", last seen {}", stamp(self.last_seen, now)));
        line.push('\n');
        line
    }

    /// One `list_devices` line: name, MAC, vendor, type, state, last seen, and
    /// the owner by name when there is one.
    ///
    /// The vendor is on the line because without it the model counted vendors
    /// from MAC prefixes (issue #33). The owner is a name, not a uuid: a
    /// listing is read, and `list_people` has the uuids for a write.
    #[must_use]
    pub fn listing_line(&self, now: i64) -> String {
        let mut line = format!("  {} — {}", self.label(), self.mac);
        for value in [self.vendor.as_deref(), self.device_type.as_deref()]
            .into_iter()
            .filter_map(present)
        {
            line.push_str(&format!(", {value}"));
        }
        line.push_str(&format!(
            ", {}, last seen {}",
            present(self.state.as_deref()).unwrap_or("state unknown"),
            stamp(self.last_seen, now)
        ));
        match (
            present(self.owner_name.as_deref()),
            present(self.owner_item_id.as_deref()),
        ) {
            (Some(name), _) => line.push_str(&format!(", owner {name}")),
            (None, Some(id)) => line.push_str(&format!(", owner id {id} (no such person)")),
            (None, None) => {}
        }
        line.push('\n');
        line
    }
}

/// One row of a rendered timeline.
#[derive(Debug, Clone, Default)]
pub struct TimelineRow {
    /// What the row is about: a location, an IP, an event type. Empty for a
    /// plain presence span.
    pub label: String,
    /// Start, unix seconds.
    pub start: Option<i64>,
    /// End, unix seconds. `None` is ongoing.
    pub end: Option<i64>,
    /// Extra detail appended in parentheses.
    pub detail: Option<String>,
}

/// Render a list of timeline rows, one line each, newest first.
#[must_use]
pub fn render_timeline(heading: &str, rows: &[TimelineRow], now: i64) -> String {
    if rows.is_empty() {
        return format!("{heading}: none recorded\n");
    }
    let mut out = format!("{heading}:\n");
    for row in rows.iter().take(HISTORY_ROWS) {
        out.push_str("  ");
        if !row.label.is_empty() {
            out.push_str(&row.label);
            out.push_str(" — ");
        }
        match (row.start, row.end) {
            (Some(start), Some(end)) => {
                out.push_str(&format!("{} to {}", format_utc(start), format_utc(end)));
            }
            (Some(start), None) => {
                out.push_str(&format!("{} (ongoing)", format_utc(start)));
            }
            (None, Some(end)) => out.push_str(&format!("until {}", format_utc(end))),
            (None, None) => out.push_str("time unknown"),
        }
        if let Some(detail) = row
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            out.push_str(&format!(" ({detail})"));
        }
        if let Some(start) = row.start {
            out.push_str(&format!(" [{}]", relative(start, now)));
        }
        out.push('\n');
    }
    if rows.len() > HISTORY_ROWS {
        out.push_str(&format!(
            "  … {} more not shown\n",
            rows.len() - HISTORY_ROWS
        ));
    }
    out
}

/// The `netgrasp_device` scope's snapshot.
#[must_use]
pub fn render_device_snapshot(
    facts: &DeviceFacts,
    presence: &[TimelineRow],
    locations: &[TimelineRow],
    events: &[TimelineRow],
    now: i64,
) -> String {
    let mut out = String::with_capacity(2_048);

    out.push_str(&format!("Device: {}\n", facts.phrase()));
    out.push_str(&format!("Numeric id: {}\n", facts.id));
    out.push_str(&format!("MAC: {}\n", facts.mac));
    out.push_str(&line("Display name", facts.display_name.as_deref()));
    out.push_str(&line("Resolved name", facts.resolved_name.as_deref()));
    out.push_str(&line("Hostname", facts.hostname.as_deref()));
    out.push_str(&line("mDNS name", facts.mdns_name.as_deref()));
    out.push_str(&line("Vendor", facts.vendor.as_deref()));
    out.push_str(&line("Type", facts.device_type.as_deref()));
    out.push_str(&line("OS", facts.os_family.as_deref()));
    out.push_str(&line_or("State", facts.state.as_deref(), "unknown"));
    out.push_str(&line("Last IPv4", facts.last_ip.as_deref()));
    out.push_str(&line("Last IPv6", facts.last_ipv6.as_deref()));
    out.push_str(&line("Access point", facts.current_ap.as_deref()));
    out.push_str(&line("Location", facts.current_location.as_deref()));
    out.push_str(&format!("First seen: {}\n", stamp(facts.first_seen, now)));
    out.push_str(&format!("Last seen: {}\n", stamp(facts.last_seen, now)));
    // The device scope's answer to "how far back can I ask": this device's own
    // first sighting, which is where its history starts whatever the rest of
    // the network has been recording.
    out.push_str(&render_monitoring_window(facts.first_seen, now));
    out.push_str(&format!("Owner: {}\n", facts.owner_phrase()));
    out.push_str(&line_or("Notes", facts.notes.as_deref(), "none"));
    out.push_str(&flag_line("Hidden", facts.hidden));
    out.push_str(&flag_line("Notify", facts.notify));

    out.push('\n');
    out.push_str(&render_timeline("Recent presence", presence, now));
    out.push('\n');
    out.push_str(&render_timeline("Recent locations", locations, now));
    out.push('\n');
    out.push_str(&render_timeline("Recent events", events, now));

    cap(&out, SNAPSHOT_MAX_BYTES)
}

/// One person, as every assistant read decodes them.
///
/// The mirror's row plus a device count. `state` and `current_location` are
/// daemon-written and are read-only here — the assistant can say where somebody
/// is and cannot move them.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PersonFacts {
    /// Their `ng_person` Item id.
    pub item_id: String,
    /// Their name.
    pub name: String,
    /// Free text an admin keeps about them.
    #[serde(default)]
    pub notes: Option<String>,
    /// Tell someone when one of their devices appears.
    #[serde(default)]
    pub notify_arrive: bool,
    /// Tell someone when the last of their devices disappears.
    #[serde(default)]
    pub notify_depart: bool,
    /// `home` / `away`, as the daemon last saw it.
    #[serde(default)]
    pub state: Option<String>,
    /// Where the daemon last placed them.
    #[serde(default)]
    pub current_location: Option<String>,
    /// How many devices name them as owner.
    #[serde(default)]
    pub device_count: i64,
}

impl PersonFacts {
    /// Them, named the way a proposal names a person.
    #[must_use]
    pub fn phrase(&self) -> String {
        person_phrase(&self.name, &self.item_id)
    }

    /// One listing line: name, uuid, device count, state, location.
    #[must_use]
    pub fn render_line(&self) -> String {
        let mut line = format!(
            "  {} — {} device{}",
            self.phrase(),
            self.device_count,
            if self.device_count == 1 { "" } else { "s" }
        );
        line.push_str(&format!(
            ", {}",
            self.state.as_deref().unwrap_or("state unknown")
        ));
        if let Some(location) = self
            .current_location
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            line.push_str(&format!(" at {location}"));
        }
        line.push('\n');
        line
    }
}

/// One event, as the event reads decode it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventFacts {
    /// The daemon's event vocabulary: `new_device`, `arp_spoof`, and the rest.
    pub event_type: String,
    /// When it happened, unix seconds.
    #[serde(default)]
    pub ts: Option<i64>,
    /// The daemon's structured detail, as JSON text.
    #[serde(default)]
    pub details: Option<String>,
    /// The device it was about, when the read joined one.
    #[serde(default)]
    pub mac: Option<String>,
}

impl EventFacts {
    /// This event as a timeline row.
    #[must_use]
    pub fn row(&self) -> TimelineRow {
        TimelineRow {
            label: self.event_type.clone(),
            start: self.ts,
            end: None,
            detail: self
                .details
                .as_deref()
                .map(|d| ellipsize(d, 80))
                .filter(|d| !d.is_empty() && d != "null" && d != "{}"),
        }
    }
}

/// One presence span inside a window, with the device it belongs to.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PresenceWindowFacts {
    /// The device's hardware address.
    pub mac: String,
    /// The name a human typed.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The daemon's resolved name.
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Reverse-DNS hostname.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name.
    #[serde(default)]
    pub mdns_name: Option<String>,
    /// Daemon's classification.
    #[serde(default)]
    pub device_type: Option<String>,
    /// The device's current state.
    #[serde(default)]
    pub state: Option<String>,
    /// The owning person's Item id.
    #[serde(default)]
    pub owner_item_id: Option<String>,
    /// The owner's name, from the mirror.
    #[serde(default)]
    pub owner_name: Option<String>,
    /// Span start, unix seconds.
    #[serde(default)]
    pub start: Option<i64>,
    /// Span end, unix seconds. `None` is still online.
    #[serde(default)]
    pub end: Option<i64>,
}

impl PresenceWindowFacts {
    /// What to call this device.
    #[must_use]
    pub fn label(&self) -> String {
        device_label(
            self.display_name.as_deref(),
            self.resolved_name.as_deref(),
            self.hostname.as_deref(),
            self.mdns_name.as_deref(),
            &self.mac,
        )
    }
}

// =============================================================================
// The overview's questions: who came and went, and what is new
// =============================================================================

/// How many movements one `arrivals_and_departures` answer reads at most. A
/// household's day is a handful; the cap is a fence, not an expectation.
pub const MAX_MOVEMENT_ROWS: i64 = 200;

/// How many devices one `new_devices` answer reads at most.
pub const MAX_NEW_DEVICE_ROWS: i64 = 100;

/// Parse an optional `YYYY-MM-DD` day argument.
///
/// `Ok(None)` when it is absent or blank, which means "today" and is resolved
/// against the database's clock rather than here: "today" on the overview is the
/// database's calendar day, and a tool that took the plugin's clock instead
/// could answer about a different day than the page beside it shows.
///
/// A date that does not exist ("2026-02-30") is refused rather than passed on,
/// because the view would simply match no rows and the model would report an
/// empty day as fact.
pub fn parse_day(arguments: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    let Some(raw) = optional_str(arguments, key)? else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let refuse = || {
        format!(
            "'{raw}' is not a day. Pass one as YYYY-MM-DD, e.g. 2026-09-25, or leave `{key}` out for today."
        )
    };
    let parts: Vec<&str> = raw.split('-').collect();
    let [year, month, day] = parts.as_slice() else {
        return Err(refuse());
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return Err(refuse());
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        year.parse::<i64>(),
        month.parse::<i64>(),
        day.parse::<i64>(),
    ) else {
        return Err(refuse());
    };
    // A real calendar day survives the round trip; 2026-02-30 comes back as
    // 2026-03-02 and is refused.
    if !(1..=12).contains(&m) || civil_from_days(days_from_civil(y, m, d)) != (y, m, d) {
        return Err(refuse());
    }
    Ok(Some(raw.to_string()))
}

/// One arrival or departure, as `SELECT_MOVEMENTS_ON_DAY` decodes it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MovementFacts {
    /// `person_arrived` or `person_departed`.
    pub event_type: String,
    /// When, unix seconds.
    #[serde(default)]
    pub ts: Option<i64>,
    /// The database's calendar day it fell on.
    #[serde(default)]
    pub day: Option<String>,
    /// The person's Item id, as text, when the event carried one.
    #[serde(default)]
    pub person_item_id: Option<String>,
    /// Their current name, or the one the event was recorded under.
    #[serde(default)]
    pub person_name: Option<String>,
    /// Where they arrived. Never set on a departure.
    #[serde(default)]
    pub location: Option<String>,
    /// The edge access point that is the evidence for it.
    #[serde(default)]
    pub via: Option<String>,
    /// The device that caused it, when that device still exists.
    #[serde(default)]
    pub device_mac: Option<String>,
    /// That device's typed name.
    #[serde(default)]
    pub device_display_name: Option<String>,
    /// That device's resolved name.
    #[serde(default)]
    pub device_resolved_name: Option<String>,
    /// That device's hostname.
    #[serde(default)]
    pub device_hostname: Option<String>,
}

impl MovementFacts {
    /// One line: when, who, what, where, and on which device.
    #[must_use]
    pub fn render_line(&self, now: i64) -> String {
        let who = self
            .person_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("Somebody (no person recorded)");
        let what = match self.event_type.as_str() {
            "person_arrived" => "arrived",
            "person_departed" => "left",
            other => other,
        };
        let mut line = format!("  {}: {who} {what}", stamp(self.ts, now));
        if let Some(place) = present(self.location.as_deref()) {
            line.push_str(&format!(", at {place}"));
        }
        if let Some(via) = present(self.via.as_deref()) {
            line.push_str(&format!(", via {via}"));
        }
        if let Some(mac) = present(self.device_mac.as_deref()) {
            let label = device_label(
                self.device_display_name.as_deref(),
                self.device_resolved_name.as_deref(),
                self.device_hostname.as_deref(),
                None,
                mac,
            );
            line.push_str(&format!(", on {}", device_phrase(&label, mac)));
        }
        line.push('\n');
        line
    }
}

/// One person who is home, as `SELECT_PEOPLE_HOME` decodes them.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HomeFacts {
    /// Their Item id, as text.
    pub item_id: String,
    /// Their name.
    pub name: String,
    /// Where the daemon last placed them.
    #[serde(default)]
    pub current_location: Option<String>,
    /// When they arrived, unix seconds. `None` for a person the daemon set home
    /// without recording an arrival.
    #[serde(default)]
    pub arrived: Option<i64>,
    /// How many of their unhidden devices are online.
    #[serde(default)]
    pub devices_online: i64,
}

impl HomeFacts {
    /// One line: who, since when, where, how many devices.
    #[must_use]
    pub fn render_line(&self, now: i64) -> String {
        let mut line = format!("  {}", person_phrase(&self.name, &self.item_id));
        match self.arrived {
            Some(_) => line.push_str(&format!(", home since {}", stamp(self.arrived, now))),
            None => line.push_str(", home (no arrival time recorded)"),
        }
        if let Some(place) = present(self.current_location.as_deref()) {
            line.push_str(&format!(", at {place}"));
        }
        line.push_str(&format!(
            ", {} device{} online\n",
            self.devices_online,
            if self.devices_online == 1 { "" } else { "s" }
        ));
        line
    }
}

/// Render an `arrivals_and_departures` answer.
///
/// The day's movements oldest first, then who is home now. Both, because "who
/// came home today" is usually asked to find out who is here, and a person who
/// arrived yesterday and never left is home with no movement today at all.
///
/// It says which day it read and that the day is the database's, for the same
/// reason the network context says when monitoring began: an empty answer about
/// the wrong day, stated confidently, is worse than no answer.
#[must_use]
pub fn render_movements(
    day: &str,
    today: &str,
    movements: &[MovementFacts],
    home: &[HomeFacts],
    now: i64,
) -> String {
    let which = if day == today { " (today)" } else { "" };
    let mut out =
        format!("Arrivals and departures on {day}{which}. Days are the database's calendar day.\n");
    if movements.is_empty() {
        out.push_str(&format!(
            "Nobody arrived or left on {day}. Only a person with at least one device \
             assigned to them can arrive or leave; unowned devices never make either.\n"
        ));
    } else {
        let arrivals = movements
            .iter()
            .filter(|m| m.event_type == "person_arrived")
            .count();
        let departures = movements
            .iter()
            .filter(|m| m.event_type == "person_departed")
            .count();
        out.push_str(&format!(
            "{arrivals} arrival{}, {departures} departure{}, oldest first:\n",
            if arrivals == 1 { "" } else { "s" },
            if departures == 1 { "" } else { "s" }
        ));
        for movement in movements {
            out.push_str(&movement.render_line(now));
        }
    }

    if home.is_empty() {
        out.push_str("Nobody is home now.\n");
    } else {
        out.push_str(&format!("Home now ({}):\n", home.len()));
        for person in home {
            out.push_str(&person.render_line(now));
        }
    }
    cap(&out, RESULT_MAX_BYTES)
}

/// One device first seen this week, as `SELECT_NEW_DEVICES` decodes it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NewDeviceFacts {
    /// `ng_devices.id`.
    pub id: i64,
    /// Hardware address.
    pub mac: String,
    /// A name a person typed.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The daemon's resolved name.
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Hostname.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name.
    #[serde(default)]
    pub mdns_name: Option<String>,
    /// OUI vendor.
    #[serde(default)]
    pub vendor: Option<String>,
    /// The fingerprint's verdict.
    #[serde(default)]
    pub device_type: Option<String>,
    /// How sure the fingerprint is, 0 to 1.
    #[serde(default)]
    pub device_type_confidence: Option<f64>,
    /// OS guess.
    #[serde(default)]
    pub os_family: Option<String>,
    /// Which signal the resolved name came from.
    #[serde(default)]
    pub identity_source: Option<String>,
    /// `online`, `idle`, `offline`.
    #[serde(default)]
    pub state: Option<String>,
    /// Most recent IPv4.
    #[serde(default)]
    pub last_ip: Option<String>,
    /// Owner's Item id, as text.
    #[serde(default)]
    pub owner_item_id: Option<String>,
    /// Owner's name, when the mirror has them.
    #[serde(default)]
    pub owner_name: Option<String>,
    /// First observation, unix seconds.
    #[serde(default)]
    pub first_seen: Option<i64>,
    /// Latest observation, unix seconds.
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// How many devices matched before the row limit.
    #[serde(default)]
    pub total: i64,
}

impl NewDeviceFacts {
    /// Whether a person still has to name or assign it: the /devices/todo test.
    #[must_use]
    pub fn is_todo(&self) -> bool {
        present(self.display_name.as_deref()).is_none()
            && present(self.owner_item_id.as_deref()).is_none()
    }

    /// One listing line.
    #[must_use]
    pub fn render_line(&self, now: i64) -> String {
        let label = descriptive_label(
            self.display_name.as_deref(),
            self.resolved_name.as_deref(),
            self.hostname.as_deref(),
            self.mdns_name.as_deref(),
            self.vendor.as_deref(),
            self.device_type.as_deref(),
            &self.mac,
        );
        let mut line = format!(
            "  {} [id {}]: first seen {}",
            device_phrase(&label, &self.mac),
            self.id,
            stamp(self.first_seen, now)
        );
        match (
            present(self.device_type.as_deref()),
            self.device_type_confidence,
        ) {
            (Some(kind), Some(c)) => {
                line.push_str(&format!("; looks like a {kind} ({:.0}% sure)", c * 100.0));
            }
            (Some(kind), None) => line.push_str(&format!("; looks like a {kind}")),
            (None, _) => line.push_str("; not yet identified"),
        }
        if let Some(os) = present(self.os_family.as_deref()) {
            line.push_str(&format!(", {os}"));
        }
        if let Some(vendor) = present(self.vendor.as_deref()) {
            line.push_str(&format!(", made by {vendor}"));
        }
        line.push_str(&format!(
            "; {}",
            present(self.state.as_deref()).unwrap_or("state unknown")
        ));
        match (
            present(self.owner_name.as_deref()),
            present(self.owner_item_id.as_deref()),
        ) {
            (Some(name), _) => line.push_str(&format!("; owner {name}")),
            (None, Some(id)) => line.push_str(&format!("; owner {id} (no such person)")),
            (None, None) => line.push_str("; no owner"),
        }
        if present(self.display_name.as_deref()).is_none() {
            match present(self.resolved_name.as_deref()) {
                Some(guess) => line.push_str(&format!(
                    "; not named by anyone (the daemon guesses '{guess}', from {})",
                    present(self.identity_source.as_deref()).unwrap_or("an unrecorded signal")
                )),
                None => line.push_str("; not named by anyone"),
            }
        }
        line.push('\n');
        line
    }
}

/// The `new_devices` window when the call does not give one: seven days, as
/// the overview's "New this week" is.
pub const DEFAULT_NEW_DEVICE_HOURS: i64 = 168;

/// Render a `new_devices` answer: what appeared in the last `hours`, and which
/// of it still needs a person to name or assign it.
///
/// The window is stated with its start in UTC, so "in the last 24 hours" is a
/// fact the model reads rather than one it infers from a relative phrase — the
/// inference is how a device first seen 37 hours earlier was reported as new
/// today (issue #32).
#[must_use]
pub fn render_new_devices(devices: &[NewDeviceFacts], hours: i64, now: i64) -> String {
    let window = hours_phrase(hours);
    let since = format_utc(now - hours * 3_600);
    if devices.is_empty() {
        return format!("No device was first seen in {window} (since {since}).\n");
    }
    let total = devices
        .first()
        .map_or(0, |d| d.total)
        .max(devices.len() as i64);
    let todo = devices.iter().filter(|d| d.is_todo()).count();
    let mut out = format!(
        "{total} device{} first seen in {window} (since {since}), newest first. \
         {todo} of {} {} neither a name a person gave it nor an owner, \
         which is what /devices/todo lists.\n",
        if total == 1 { "" } else { "s" },
        if (devices.len() as i64) < total {
            "those listed"
        } else {
            "them"
        },
        if todo == 1 { "has" } else { "have" }
    );
    let budget = RESULT_MAX_BYTES.saturating_sub(out.len() + FOOTER_RESERVE);
    let mut shown = 0i64;
    let mut rows = String::new();
    for device in devices {
        let line = device.render_line(now);
        if rows.len() + line.len() > budget {
            break;
        }
        rows.push_str(&line);
        shown += 1;
    }
    out.push_str(&rows);
    if shown < total {
        out.push_str(&format!(
            "Showing {shown} of {total}. Ask for a shorter window with hours to see the rest.\n"
        ));
    }
    out
}

/// A trimmed, non-empty value, or nothing.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Render a `who_was_online` answer.
///
/// Grouped by person, because the question is "who", and a device list is only
/// the evidence for it. A device with no resolvable owner is listed under
/// "nobody in particular" rather than dropped: an unowned device that was on the
/// network during the window is exactly what somebody asking this wants to know
/// about.
#[must_use]
pub fn render_who_was_online(spans: &[PresenceWindowFacts], now: i64) -> String {
    if spans.is_empty() {
        return "Nothing was online in that window.".to_string();
    }

    let mut owners: Vec<(String, Vec<&PresenceWindowFacts>)> = Vec::new();
    for span in spans {
        let owner = match (span.owner_name.as_deref(), span.owner_item_id.as_deref()) {
            (Some(name), Some(id)) => person_phrase(name, id),
            (None, Some(id)) => format!("an unknown person ({id})"),
            _ => "nobody in particular".to_string(),
        };
        match owners.iter_mut().find(|(name, _)| *name == owner) {
            Some((_, group)) => group.push(span),
            None => owners.push((owner, vec![span])),
        }
    }
    // Named people first, in the order their first span appeared; the unowned
    // group last, because it is the residue rather than an answer.
    owners.sort_by_key(|(name, _)| name == "nobody in particular");

    let mut out = String::new();
    for (owner, group) in owners {
        out.push_str(&format!("{owner}:\n"));
        for span in group {
            out.push_str(&format!("  {} — {}", span.label(), span.mac));
            match (span.start, span.end) {
                (Some(start), Some(end)) => {
                    out.push_str(&format!(", {} to {}", format_utc(start), format_utc(end)));
                }
                (Some(start), None) => {
                    out.push_str(&format!(", {} and still online", format_utc(start)));
                }
                _ => out.push_str(", times unknown"),
            }
            if let Some(start) = span.start {
                out.push_str(&format!(" [{}]", relative(start, now)));
            }
            out.push('\n');
        }
    }
    cap(&out, RESULT_MAX_BYTES)
}

/// The `netgrasp_person` scope's snapshot.
#[must_use]
pub fn render_person_snapshot(person: &PersonFacts, devices: &[DeviceFacts], now: i64) -> String {
    let mut out = String::with_capacity(1_024);
    out.push_str(&format!("Person: {}\n", person.name));
    out.push_str(&format!("Uuid: {}\n", person.item_id));
    out.push_str(&line_or("Notes", person.notes.as_deref(), "none"));
    out.push_str(&flag_line("Notify on arrival", person.notify_arrive));
    out.push_str(&flag_line("Notify on departure", person.notify_depart));
    out.push_str(&line_or("State", person.state.as_deref(), "unknown"));
    out.push_str(&line_or(
        "Current location",
        person.current_location.as_deref(),
        "unknown",
    ));

    out.push('\n');
    if devices.is_empty() {
        out.push_str("Devices: none assigned\n");
    } else {
        out.push_str(&format!("Devices ({}):\n", devices.len()));
        for device in devices {
            out.push_str(&device.render_line(now, false));
        }
    }

    cap(&out, SNAPSHOT_MAX_BYTES)
}

/// The `netgrasp_network` scope's snapshot.
///
/// When the data starts first (see [`render_monitoring_window`]), then the
/// device totals by state and by owner, then what the tools are for, then the
/// people, then the security-event count.
///
/// **No device lines.** This is sent with every model call in a conversation,
/// and it used to list every device grouped by owner: on a network of 171 it was
/// about 10,000 bytes, cut at that cap partway through the list, so it cost a
/// third of each call and still could not be counted from. What it carries now
/// is what no tool hands back unasked — the clock, where the data starts, the
/// totals and who the people are — and it says which tool answers the rest.
/// The totals are the database's, over every device.
#[must_use]
pub fn render_network_snapshot(
    people: &[PersonFacts],
    counts: &DeviceCounts,
    security_events_24h: i64,
    monitoring_since: Option<i64>,
    now: i64,
) -> String {
    let mut out = String::with_capacity(2_048);

    // First, because it bounds every answer below it: a window that starts
    // before this has no data for a reason that is not an outage.
    out.push_str(&render_monitoring_window(monitoring_since, now));
    out.push('\n');

    out.push_str(&format!("Devices by state ({} total): ", counts.total));
    out.push_str(&counted(&counts.state, |v| v.to_string()));
    out.push_str(".\n");
    out.push_str(&counts.owner_sentence());
    out.push_str(
        "This context lists no single device. For devices, and for exact counts by \
         state, vendor or type, call list_devices: its header counts every matching \
         device. Use find_devices to look one up by name or address, new_devices for \
         what appeared recently, and events for what happened (security events, \
         scans, alerts sent).\n",
    );
    out.push('\n');

    if people.is_empty() {
        out.push_str("People: none\n");
    } else {
        out.push_str(&format!("People ({}):\n", people.len()));
        for person in people {
            out.push_str(&person.render_line());
        }
    }
    out.push('\n');

    out.push_str(&format!(
        "Security events in the last 24 hours: {security_events_24h}\n"
    ));

    cap(&out, SNAPSHOT_MAX_BYTES)
}

// =============================================================================
// Device listings with exact counts
// =============================================================================

/// The longest row limit `list_devices` accepts.
pub const MAX_LIST_ROWS: i64 = 200;

/// How many rows `list_devices` returns when the call does not say.
///
/// Small on purpose: the counts above the rows answer every "how many"
/// question exactly, and every row stays in the conversation for every later
/// model call. A question that needs the rows themselves narrows the filter or
/// raises the limit.
pub const DEFAULT_LIST_ROWS: i64 = 40;

/// One row of [`crate::queries::SELECT_DEVICE_COUNTS`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceCount {
    /// `state`, `vendor`, `type`, `owner` or `hidden`.
    pub dimension: String,
    /// The value counted. `''` for an absent vendor or type.
    #[serde(default)]
    pub value: String,
    /// How many devices have it.
    pub devices: i64,
}

/// A filter's devices counted every way the header reports, gathered from
/// [`DeviceCount`] rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceCounts {
    /// Every matching device.
    pub total: i64,
    /// By state: online, idle, offline first, then the rest by name.
    pub state: Vec<(String, i64)>,
    /// By vendor, most first; `''` (no vendor recorded) last.
    pub vendor: Vec<(String, i64)>,
    /// By type, most first; `''` (not identified) last.
    pub kind: Vec<(String, i64)>,
    /// Devices with an owner the mirror knows.
    pub owned: i64,
    /// Devices with no owner.
    pub unowned: i64,
    /// Devices whose owner id names nobody.
    pub owner_missing: i64,
    /// Devices hidden from the listings.
    pub hidden: i64,
}

impl DeviceCounts {
    /// Gather the database's rows. The total is the sum of the state counts,
    /// which partition the devices.
    #[must_use]
    pub fn from_rows(rows: &[DeviceCount]) -> Self {
        let mut counts = Self::default();
        for row in rows {
            let entry = (row.value.clone(), row.devices);
            match row.dimension.as_str() {
                "state" => {
                    counts.total += row.devices;
                    counts.state.push(entry);
                }
                "vendor" => counts.vendor.push(entry),
                "type" => counts.kind.push(entry),
                "owner" => match row.value.as_str() {
                    "person" => counts.owned += row.devices,
                    "none" => counts.unowned += row.devices,
                    _ => counts.owner_missing += row.devices,
                },
                "hidden" if row.value == "yes" => counts.hidden += row.devices,
                _ => {}
            }
        }
        let rank = |state: &str| match state {
            "online" => 0,
            "idle" => 1,
            "offline" => 2,
            _ => 3,
        };
        counts
            .state
            .sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
        for list in [&mut counts.vendor, &mut counts.kind] {
            list.sort_by(|a, b| {
                a.0.is_empty()
                    .cmp(&b.0.is_empty())
                    .then_with(|| b.1.cmp(&a.1))
                    .then_with(|| a.0.cmp(&b.0))
            });
        }
        counts
    }

    /// How many are in one state.
    #[must_use]
    pub fn in_state(&self, state: &str) -> i64 {
        self.state
            .iter()
            .find(|(s, _)| s == state)
            .map_or(0, |(_, n)| *n)
    }

    /// "10 have an owner, 161 have none." and, when there are any, the
    /// dangling owners and the hidden devices.
    #[must_use]
    pub fn owner_sentence(&self) -> String {
        let mut out = format!(
            "Owners: {} belong to a person, {} to nobody",
            self.owned, self.unowned
        );
        if self.owner_missing > 0 {
            out.push_str(&format!(
                ", {} to an owner id that names no person",
                self.owner_missing
            ));
        }
        out.push('.');
        if self.hidden > 0 {
            out.push_str(&format!(" {} are hidden from listings.", self.hidden));
        }
        out.push('\n');
        out
    }

    /// The header lines: by state, by vendor, by type, by owner. Never cut.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "By state: {}.\n",
            counted(&self.state, |v| v.to_string())
        ));
        out.push_str(&format!(
            "By vendor: {}.\n",
            counted(&self.vendor, |v| if v.is_empty() {
                "no vendor recorded".to_string()
            } else {
                v.to_string()
            })
        ));
        out.push_str(&format!(
            "By type: {}.\n",
            counted(&self.kind, |v| if v.is_empty() {
                "not identified".to_string()
            } else {
                v.to_string()
            })
        ));
        out.push_str(&self.owner_sentence());
        out
    }
}

/// "online 73, idle 22, offline 76".
fn counted(list: &[(String, i64)], name: impl Fn(&str) -> String) -> String {
    if list.is_empty() {
        return "none".to_string();
    }
    list.iter()
        .map(|(value, n)| format!("{} {n}", name(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A row limit: defaulted, and held to `0..=max`.
///
/// # Errors
///
/// A sentence naming the range.
pub fn parse_limit(
    arguments: &serde_json::Value,
    key: &str,
    default: i64,
    max: i64,
) -> Result<i64, String> {
    match arguments.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(value) => value
            .as_i64()
            .filter(|n| (0..=max).contains(n))
            .ok_or_else(|| format!("'{key}' must be a whole number from 0 to {max}")),
    }
}

/// An hours argument: defaulted, and held to `1..=max`.
///
/// # Errors
///
/// A sentence naming the range.
pub fn parse_hours(
    arguments: &serde_json::Value,
    key: &str,
    default: i64,
    max: i64,
) -> Result<i64, String> {
    match arguments.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(value) => value
            .as_i64()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| format!("'{key}' must be a whole number of hours from 1 to {max}")),
    }
}

/// "the last 24 hours", "the last seven days", "the last 3 days".
#[must_use]
pub fn hours_phrase(hours: i64) -> String {
    match hours {
        1 => "the last hour".to_string(),
        168 => "the last seven days".to_string(),
        h if h >= 48 && h % 24 == 0 => format!("the last {} days", h / 24),
        h => format!("the last {h} hours"),
    }
}

/// Device rows under a byte budget: as many whole lines as fit.
///
/// Returns the lines and how many there are, so the caller can say "showing N
/// of M" rather than present a cut list as the whole.
#[must_use]
pub fn device_rows(devices: &[DeviceFacts], now: i64, budget: usize) -> (String, usize) {
    let mut out = String::new();
    let mut shown = 0;
    for device in devices {
        let line = device.listing_line(now);
        if out.len() + line.len() > budget {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    (out, shown)
}

/// Bytes kept back from a result's rows for the "showing N of M" footer.
const FOOTER_RESERVE: usize = 400;

/// Render a `list_devices` answer: exact counts first, then rows.
///
/// `what` is the filter in words ("(all)", "(vendor:ubiquiti)"). `counts` are
/// the database's, over every matching device; `devices` are the rows the
/// limit let through. The counts are never cut. The rows are, by the limit and
/// then by [`RESULT_MAX_BYTES`], and when they are the answer and its summary
/// both say "showing N of M" and how to narrow.
///
/// Returns the content and the one-line summary the chat shows.
#[must_use]
pub fn render_device_listing(
    what: &str,
    counts: &DeviceCounts,
    devices: &[DeviceFacts],
    now: i64,
    max_bytes: usize,
) -> (String, String) {
    if counts.total == 0 {
        return (
            format!("No devices {what}.\n"),
            format!("No devices {what}"),
        );
    }
    let plural = if counts.total == 1 { "" } else { "s" };
    let mut out = format!(
        "{} device{plural} {what}. Counts over all {}, from the database; \
         answer any \"how many\" from these, not from the rows:\n",
        counts.total, counts.total
    );
    out.push_str(&counts.render());

    let budget = max_bytes.saturating_sub(out.len() + FOOTER_RESERVE);
    let (rows, shown) = device_rows(devices, now, budget);
    if shown > 0 {
        out.push_str("Devices, most recently seen first:\n");
        out.push_str(&rows);
    }
    let total = counts.total;
    let all_shown = shown as i64 >= total;
    if !all_shown {
        out.push_str(&format!(
            "Showing {shown} of {total}. For others, narrow the filter ({DEVICE_FILTERS}) \
             or raise limit (up to {MAX_LIST_ROWS}).\n"
        ));
    }
    let states = counted(&counts.state, |v| v.to_string());
    let summary = if all_shown {
        format!("Listed {total} device{plural} {what}: {states}")
    } else {
        format!("Showing {shown} of {total} devices {what}: {states}")
    };
    (out, summary)
}

// =============================================================================
// The events tool
// =============================================================================

/// The event types the daemon writes, as found in `ng_events` on a running
/// install (2026-11): the vocabulary the `events` tool's description offers.
/// Not a filter: a type the daemon adds later is still counted and listed.
pub const KNOWN_EVENT_TYPES: &[&str] = &[
    "arp_scan",
    "arp_spoof",
    "gratuitous_arp",
    "identity_change",
    "ip_changed",
    "ip_conflict",
    "name_updated",
    "new_device",
    "person_arrived",
    "person_departed",
    "returned",
    "rogue_dhcp",
    "went_offline",
];

/// The default `events` window, in hours.
pub const DEFAULT_EVENT_HOURS: i64 = 24;

/// How many event rows `events` lists when the call does not say.
pub const DEFAULT_EVENT_ROWS: i64 = 20;

/// The most event rows `events` lists.
pub const MAX_EVENT_ROWS: i64 = 100;

/// How many devices the by-device counts name.
pub const EVENT_DEVICE_ROWS: i64 = 10;

/// An `events` call, parsed. The device is resolved by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    /// Window start, unix seconds, inclusive.
    pub start: i64,
    /// Window end, unix seconds, exclusive.
    pub end: i64,
    /// The window in words, with its preposition: "in the last 24 hours (…)",
    /// "from … to …".
    pub window: String,
    /// Only these types; empty for any.
    pub types: Vec<String>,
    /// Only security events.
    pub security: bool,
    /// Only events sent as alerts.
    pub alerts: bool,
    /// How many rows to list.
    pub limit: i64,
}

impl EventQuery {
    /// Parse the arguments against the clock: `hours` (default 24, at most
    /// seven days) ending now, or `from` and `to`, at most seven days apart.
    ///
    /// # Errors
    ///
    /// A sentence for the model.
    pub fn parse(arguments: &serde_json::Value, now: i64) -> Result<Self, String> {
        let from = optional_str(arguments, "from")?;
        let to = optional_str(arguments, "to")?;
        let (start, end, window) = match (from, to) {
            (Some(from), Some(to)) => {
                let (start, end) = parse_window(&from, &to)?;
                (
                    start,
                    end,
                    format!("from {} to {}", format_utc(start), format_utc(end)),
                )
            }
            (None, None) => {
                let hours = parse_hours(
                    arguments,
                    "hours",
                    DEFAULT_EVENT_HOURS,
                    MAX_WINDOW_DAYS * 24,
                )?;
                // One second past now, so an event written this second is in.
                (
                    now - hours * 3_600,
                    now + 1,
                    format!(
                        "in {} ({} to now, {})",
                        hours_phrase(hours),
                        format_utc(now - hours * 3_600),
                        format_utc(now)
                    ),
                )
            }
            _ => return Err("pass both 'from' and 'to', or neither".to_string()),
        };
        let types = match optional_str(arguments, "type")? {
            None => Vec::new(),
            Some(raw) => {
                let types: Vec<String> = raw
                    .split(',')
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect();
                if let Some(bad) = types
                    .iter()
                    .find(|t| !t.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
                {
                    return Err(format!(
                        "'{bad}' is not an event type. Known types: {}",
                        KNOWN_EVENT_TYPES.join(", ")
                    ));
                }
                types
            }
        };
        Ok(Self {
            start,
            end,
            window,
            types,
            security: optional_bool(arguments, "security")?.unwrap_or(false),
            alerts: optional_bool(arguments, "alerts")?.unwrap_or(false),
            limit: parse_limit(arguments, "limit", DEFAULT_EVENT_ROWS, MAX_EVENT_ROWS)?,
        })
    }

    /// The six parameters of the `events` statements' shared filter, `$1`–`$6`.
    /// `device_id` is the resolved device, or `0` for any.
    #[must_use]
    pub fn sql_params(&self, device_id: i64) -> Vec<serde_json::Value> {
        use serde_json::json;
        vec![
            json!(self.start),
            json!(self.end),
            json!(self.types.join(",")),
            json!(self.security),
            json!(device_id),
            json!(self.alerts),
        ]
    }
}

/// One row of [`crate::queries::SELECT_EVENT_COUNTS`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventTypeCount {
    /// The daemon's event type.
    pub event_type: String,
    /// How many in the window.
    pub events: i64,
    /// How many of those were sent as alerts.
    #[serde(default)]
    pub sent: i64,
}

/// One row of [`crate::queries::SELECT_EVENT_DEVICE_COUNTS`].
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventDeviceCount {
    /// The device, or `None` for events about no device.
    #[serde(default)]
    pub device_id: Option<i64>,
    /// How many events.
    pub events: i64,
    /// How many devices had events in the window, the busiest or not.
    #[serde(default)]
    pub device_total: i64,
    /// Its MAC, when the device still exists.
    #[serde(default)]
    pub mac: Option<String>,
    /// Typed name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Resolved name.
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Hostname.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name.
    #[serde(default)]
    pub mdns_name: Option<String>,
}

/// "Name (mac)", or a sentence for an event with no device.
fn event_device(
    mac: Option<&str>,
    display_name: Option<&str>,
    resolved_name: Option<&str>,
    hostname: Option<&str>,
    mdns_name: Option<&str>,
) -> String {
    match present(mac) {
        Some(mac) => device_phrase(
            &device_label(display_name, resolved_name, hostname, mdns_name, mac),
            mac,
        ),
        None => "no device".to_string(),
    }
}

/// One row of [`crate::queries::SELECT_EVENTS_IN_WINDOW`].
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventWindowFacts {
    /// The daemon's event type.
    pub event_type: String,
    /// When, unix seconds.
    #[serde(default)]
    pub ts: Option<i64>,
    /// The daemon's detail, as JSON text.
    #[serde(default)]
    pub details: Option<String>,
    /// Whether it was sent as an alert.
    #[serde(default)]
    pub notified: bool,
    /// The device's MAC, when there is one.
    #[serde(default)]
    pub mac: Option<String>,
    /// Typed name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Resolved name.
    #[serde(default)]
    pub resolved_name: Option<String>,
    /// Hostname.
    #[serde(default)]
    pub hostname: Option<String>,
    /// mDNS name.
    #[serde(default)]
    pub mdns_name: Option<String>,
}

impl EventWindowFacts {
    /// One line: when, what, which device, whether it alerted, the detail.
    #[must_use]
    pub fn render_line(&self, now: i64) -> String {
        let mut line = format!(
            "  {} {} — {}",
            stamp(self.ts, now),
            self.event_type,
            event_device(
                self.mac.as_deref(),
                self.display_name.as_deref(),
                self.resolved_name.as_deref(),
                self.hostname.as_deref(),
                self.mdns_name.as_deref(),
            )
        );
        if self.notified {
            line.push_str(", sent as an alert");
        }
        if let Some(detail) = self
            .details
            .as_deref()
            .map(|d| ellipsize(d, 80))
            .filter(|d| !d.is_empty() && d != "null" && d != "{}")
        {
            line.push_str(&format!(": {detail}"));
        }
        line.push('\n');
        line
    }
}

/// Render an `events` answer: counts by type, alerts sent and the busiest
/// devices, all counted by the database over every matching event, then the
/// newest rows under the byte cap.
///
/// `device` is the device the call was narrowed to, in words. Returns the
/// content and the chat summary.
#[must_use]
pub fn render_events(
    query: &EventQuery,
    device: Option<&str>,
    types: &[EventTypeCount],
    devices: &[EventDeviceCount],
    rows: &[EventWindowFacts],
    now: i64,
    max_bytes: usize,
) -> (String, String) {
    let mut scope = Vec::new();
    if query.security {
        scope.push("security events only".to_string());
    }
    if !query.types.is_empty() {
        scope.push(format!("type {}", query.types.join(" or ")));
    }
    if let Some(device) = device {
        scope.push(format!("about {device}"));
    }
    if query.alerts {
        scope.push("only those sent as alerts".to_string());
    }
    let scope = if scope.is_empty() {
        String::new()
    } else {
        format!(", {}", scope.join(", "))
    };
    let mut out = format!("Events {}{scope}.\n", query.window);

    let total: i64 = types.iter().map(|t| t.events).sum();
    if total == 0 {
        out.push_str("No events matched.\n");
        return (out, format!("No events {}{scope}", query.window));
    }
    let sent: i64 = types.iter().map(|t| t.sent).sum();
    out.push_str(&format!(
        "{total} event{} in all, {sent} of them sent as alerts. By type, counted by the \
         database over every one:\n",
        if total == 1 { "" } else { "s" }
    ));
    for t in types {
        out.push_str(&format!("  {}: {}", t.event_type, t.events));
        if crate::model::is_security_event(&t.event_type) {
            out.push_str(" (security)");
        }
        if t.sent > 0 {
            out.push_str(&format!(", {} sent as alerts", t.sent));
        }
        out.push('\n');
    }

    if let Some(first) = devices.first() {
        let named = devices.len() as i64;
        let of = first.device_total.max(named);
        if of > named {
            out.push_str(&format!("By device, busiest first ({named} of {of}):\n"));
        } else {
            out.push_str("By device, busiest first:\n");
        }
        for d in devices {
            out.push_str(&format!(
                "  {}: {}\n",
                event_device(
                    d.mac.as_deref(),
                    d.display_name.as_deref(),
                    d.resolved_name.as_deref(),
                    d.hostname.as_deref(),
                    d.mdns_name.as_deref(),
                ),
                d.events
            ));
        }
    }

    let budget = max_bytes.saturating_sub(out.len() + FOOTER_RESERVE);
    let mut lines = String::new();
    let mut shown = 0i64;
    for row in rows {
        let line = row.render_line(now);
        if lines.len() + line.len() > budget {
            break;
        }
        lines.push_str(&line);
        shown += 1;
    }
    if shown > 0 {
        out.push_str("Newest first:\n");
        out.push_str(&lines);
    }
    if shown < total {
        out.push_str(&format!(
            "Showing {shown} of {total} events. Count from the numbers above; for other rows \
             narrow by type, device, security or a shorter window, or raise limit (up to \
             {MAX_EVENT_ROWS}).\n"
        ));
    }
    let summary = if shown < total {
        format!("{total} events {}{scope}; showing {shown}", query.window)
    } else {
        format!("{total} events {}{scope}", query.window)
    };
    (out, summary)
}

/// How many devices are in each state, most common first, then alphabetical.
#[must_use]
pub fn count_by_state(devices: &[DeviceFacts]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for device in devices {
        let state = device
            .state
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string();
        match counts.iter_mut().find(|(name, _)| *name == state) {
            Some((_, count)) => *count += 1,
            None => counts.push((state, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    const JAMIE: &str = "5eed0000-0000-4000-8000-000000000001";
    const ARLO: &str = "5eed0000-0000-4000-8000-000000000003";
    /// 2026-08-24 12:00:00 UTC.
    const NOW: i64 = 1_787_572_800;

    fn jamie() -> PersonCandidate {
        PersonCandidate {
            item_id: JAMIE.to_string(),
            name: "Jamie".to_string(),
        }
    }

    fn arlo() -> PersonCandidate {
        PersonCandidate {
            item_id: ARLO.to_string(),
            name: "Arlo".to_string(),
        }
    }

    // -------------------------------------------------------------------------
    // Device references
    // -------------------------------------------------------------------------

    #[test]
    fn a_mac_is_recognized_in_every_form_people_write_one() {
        for raw in [
            "02:00:5e:00:00:04",
            "02-00-5E-00-00-04",
            "0200.5e00.0004",
            "02005E000004",
            "  02:00:5E:00:00:04  ",
        ] {
            assert_eq!(
                parse_device_ref(raw).unwrap(),
                DeviceRef::Mac("02:00:5e:00:00:04".to_string()),
                "{raw} should normalize to one device"
            );
        }
    }

    #[test]
    fn a_numeric_id_is_a_device_reference_and_zero_is_not() {
        assert_eq!(parse_device_ref("42").unwrap(), DeviceRef::Id(42));
        assert_eq!(parse_device_ref(" 7 ").unwrap(), DeviceRef::Id(7));
        // Identity columns start at 1, so 0 and negatives are not row ids.
        assert!(parse_device_ref("0").is_err());
        assert!(parse_device_ref("-3").is_err());
    }

    #[test]
    fn garbage_is_refused_with_a_sentence_that_says_what_to_do() {
        for raw in ["", "   ", "the amazon tablet", "aa:bb:cc", "not-a-mac"] {
            let error = parse_device_ref(raw).unwrap_err();
            assert!(
                error.contains("MAC") || error.contains("named"),
                "{raw} produced an unhelpful error: {error}"
            );
        }
        // Specifically: a model that guessed at a description is pointed at the
        // read tool instead of being left to guess again.
        assert!(
            parse_device_ref("the amazon tablet")
                .unwrap_err()
                .contains("find_devices")
        );
    }

    #[test]
    fn a_mac_that_is_not_twelve_hex_digits_is_not_a_mac() {
        assert!(!looks_like_mac("aa:bb:cc:dd:ee"));
        assert!(!looks_like_mac("aa:bb:cc:dd:ee:ff:00"));
        // Not hex, and not separators either.
        assert!(!looks_like_mac("zz:bb:cc:dd:ee:ff"));
        assert!(looks_like_mac("aa:bb:cc:dd:ee:ff"));
    }

    // -------------------------------------------------------------------------
    // Person references
    // -------------------------------------------------------------------------

    #[test]
    fn a_uuid_is_a_person_reference_and_a_name_is_too() {
        assert_eq!(
            parse_person_ref(JAMIE).unwrap(),
            PersonRef::Uuid(JAMIE.to_string())
        );
        assert_eq!(
            parse_person_ref("Jamie").unwrap(),
            PersonRef::Name("Jamie".to_string())
        );
        assert!(parse_person_ref("  ").is_err());
    }

    #[test]
    fn a_name_resolves_only_when_it_matches_exactly_and_uniquely() {
        let people = vec![jamie(), arlo()];

        assert_eq!(
            resolve_person(&PersonRef::Name("jamie".into()), &people).unwrap(),
            jamie(),
            "case does not have to match"
        );
        assert_eq!(
            resolve_person(&PersonRef::Uuid(JAMIE.into()), &people).unwrap(),
            jamie()
        );

        // A partial match is not a match: guessing on a write is the whole thing
        // this refuses to do.
        let error = resolve_person(&PersonRef::Name("Jam".into()), &people).unwrap_err();
        assert!(error.contains("no person is called 'Jam'"), "{error}");
        assert!(error.contains("list_people"), "{error}");

        let error = resolve_person(
            &PersonRef::Uuid("5eed0000-0000-4000-8000-00000000dead".into()),
            &people,
        )
        .unwrap_err();
        assert!(error.contains("no person has the id"), "{error}");
    }

    #[test]
    fn an_ambiguous_name_names_every_candidate_rather_than_picking_one() {
        let people = vec![
            PersonCandidate {
                item_id: "aaaa0000-0000-4000-8000-000000000001".into(),
                name: "Sam".into(),
            },
            PersonCandidate {
                item_id: "bbbb0000-0000-4000-8000-000000000002".into(),
                name: "sam".into(),
            },
        ];
        let error = resolve_person(&PersonRef::Name("Sam".into()), &people).unwrap_err();
        assert!(error.contains("matches 2 people"), "{error}");
        assert!(error.contains("aaaa0000"), "{error}");
        assert!(error.contains("bbbb0000"), "{error}");
        assert!(error.contains("uuid"), "{error}");
    }

    // -------------------------------------------------------------------------
    // Arguments
    // -------------------------------------------------------------------------

    #[test]
    fn required_and_optional_arguments_are_told_apart() {
        let args =
            json!({"device": "02:00:5e:00:00:04", "person": null, "empty": "  ", "flag": true});

        assert_eq!(require_str(&args, "device").unwrap(), "02:00:5e:00:00:04");
        assert!(
            require_str(&args, "missing")
                .unwrap_err()
                .contains("required")
        );
        assert!(require_str(&args, "empty").unwrap_err().contains("empty"));

        // null is "nobody", not "you forgot an argument" — this is how
        // unassigning is said.
        assert_eq!(optional_str(&args, "person").unwrap(), None);
        assert_eq!(optional_str(&args, "absent").unwrap(), None);
        assert_eq!(optional_str(&args, "empty").unwrap(), None);
        assert_eq!(
            optional_str(&args, "device").unwrap().as_deref(),
            Some("02:00:5e:00:00:04")
        );
        assert!(optional_str(&args, "flag").unwrap_err().contains("string"));

        assert_eq!(optional_bool(&args, "flag").unwrap(), Some(true));
        assert_eq!(optional_bool(&args, "absent").unwrap(), None);
        assert!(
            optional_bool(&args, "device")
                .unwrap_err()
                .contains("true or false")
        );

        assert!(require_bool(&args, "flag").unwrap());
        assert!(require_bool(&args, "absent").is_err());
    }

    #[test]
    fn a_day_count_is_defaulted_and_range_checked() {
        assert_eq!(parse_days(&json!({}), "days", 7).unwrap(), 7);
        assert_eq!(parse_days(&json!({"days": null}), "days", 7).unwrap(), 7);
        assert_eq!(parse_days(&json!({"days": 30}), "days", 7).unwrap(), 30);
        assert_eq!(parse_days(&json!({"days": 90}), "days", 7).unwrap(), 90);

        for bad in [json!({"days": 0}), json!({"days": 91}), json!({"days": -1})] {
            assert!(
                parse_days(&bad, "days", 7)
                    .unwrap_err()
                    .contains("between 1 and 90")
            );
        }
        assert!(
            parse_days(&json!({"days": "seven"}), "days", 7)
                .unwrap_err()
                .contains("whole number")
        );
    }

    #[test]
    fn a_presence_window_must_be_ordered_and_no_wider_than_a_week() {
        let (start, end) = parse_window("2026-08-20T00:00:00Z", "2026-08-21T00:00:00Z").unwrap();
        assert_eq!(end - start, 86_400);

        // Backwards.
        assert!(
            parse_window("2026-08-21T00:00:00Z", "2026-08-20T00:00:00Z")
                .unwrap_err()
                .contains("after")
        );
        // Equal is not a window.
        assert!(parse_window("2026-08-20T00:00:00Z", "2026-08-20T00:00:00Z").is_err());
        // Too wide.
        assert!(
            parse_window("2026-08-01T00:00:00Z", "2026-08-20T00:00:00Z")
                .unwrap_err()
                .contains("7 days")
        );
        // Unparseable.
        assert!(
            parse_window("last tuesday", "now")
                .unwrap_err()
                .contains("ISO 8601")
        );
    }

    #[test]
    fn iso_8601_is_parsed_in_the_forms_a_model_actually_emits() {
        // Midnight UTC on 2026-08-24.
        let midnight = 1_787_529_600;
        assert_eq!(parse_iso8601("2026-08-24").unwrap(), midnight);
        assert_eq!(parse_iso8601("2026-08-24T00:00:00Z").unwrap(), midnight);
        assert_eq!(parse_iso8601("2026-08-24 00:00:00").unwrap(), midnight);
        assert_eq!(parse_iso8601("2026-08-24T00:00").unwrap(), midnight);
        assert_eq!(parse_iso8601("2026-08-24T00:00:00.123Z").unwrap(), midnight);
        // An offset moves the instant, in both directions.
        assert_eq!(
            parse_iso8601("2026-08-24T01:00:00+01:00").unwrap(),
            midnight
        );
        assert_eq!(
            parse_iso8601("2026-08-23T23:00:00-01:00").unwrap(),
            midnight
        );
        assert_eq!(parse_iso8601("2026-08-24T12:00:00Z").unwrap(), NOW);

        for bad in [
            "",
            "not a time",
            "2026-13-01",
            "2026-08-32",
            "2026-08-24T25:00:00Z",
        ] {
            assert!(parse_iso8601(bad).is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn the_civil_date_conversions_are_each_others_inverse() {
        for days in [-25_567_i64, -1, 0, 1, 19_000, 20_690, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        // A leap day survives the round trip.
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn a_device_filter_parses_including_the_owner_prefix() {
        assert_eq!(parse_device_filter("all").unwrap(), DeviceFilter::All);
        assert_eq!(parse_device_filter("").unwrap(), DeviceFilter::All);
        assert_eq!(
            parse_device_filter("unowned").unwrap(),
            DeviceFilter::Unowned
        );
        assert_eq!(parse_device_filter("online").unwrap(), DeviceFilter::Online);
        assert_eq!(parse_device_filter("hidden").unwrap(), DeviceFilter::Hidden);
        assert_eq!(
            parse_device_filter("owner:Arlo").unwrap(),
            DeviceFilter::Owner("Arlo".to_string())
        );
        assert!(
            parse_device_filter("owner:")
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            parse_device_filter("sideways")
                .unwrap_err()
                .contains("unowned")
        );
    }

    // -------------------------------------------------------------------------
    // Naming and describing
    // -------------------------------------------------------------------------

    #[test]
    fn the_name_ladder_prefers_what_a_human_typed_and_ends_at_the_mac() {
        assert_eq!(
            device_label(
                Some("Office printer"),
                Some("printer"),
                Some("printer.lan"),
                None,
                "mac"
            ),
            "Office printer"
        );
        assert_eq!(
            device_label(None, Some("printer"), Some("printer.lan"), None, "mac"),
            "printer"
        );
        assert_eq!(
            device_label(None, None, Some("printer.lan"), None, "mac"),
            "printer.lan"
        );
        assert_eq!(
            device_label(None, None, None, Some("HP-LaserJet"), "mac"),
            "HP-LaserJet"
        );
        assert_eq!(
            device_label(None, None, None, None, "02:00:5e:00:00:08"),
            "02:00:5e:00:00:08"
        );
        // A blank is not a name.
        assert_eq!(device_label(Some("  "), None, None, None, "mac"), "mac");
    }

    #[test]
    fn a_nameless_device_is_described_by_what_the_daemon_saw() {
        // A proposal card has no context, so "Assign 02:00:5e:00:00:04 to Jamie"
        // asks somebody to approve a change to a device they cannot picture.
        assert_eq!(
            descriptive_label(
                None,
                None,
                None,
                None,
                Some("Amazon Technologies"),
                Some("tablet"),
                "mac"
            ),
            "Amazon tablet"
        );
        assert_eq!(
            descriptive_label(
                None,
                None,
                None,
                None,
                Some("Apple, Inc."),
                Some("phone"),
                "mac"
            ),
            "Apple phone"
        );
        assert_eq!(
            descriptive_label(None, None, None, None, Some("Synology Inc."), None, "mac"),
            "Synology device"
        );
        assert_eq!(
            descriptive_label(None, None, None, None, None, Some("router"), "mac"),
            "router"
        );
        // Nothing observed at all: the MAC, which is the one thing every device
        // has.
        assert_eq!(
            descriptive_label(None, None, None, None, None, None, "mac"),
            "mac"
        );
        // A name always wins over a guess.
        assert_eq!(
            descriptive_label(
                Some("Gateway"),
                None,
                None,
                None,
                Some("Ubiquiti Inc."),
                Some("router"),
                "mac"
            ),
            "Gateway"
        );
    }

    #[test]
    fn a_device_phrase_says_the_mac_once_when_that_is_all_there_is() {
        assert_eq!(
            device_phrase("Amazon tablet", "02:00:5e:00:00:04"),
            "Amazon tablet (02:00:5e:00:00:04)"
        );
        assert_eq!(
            device_phrase("02:00:5e:00:00:08", "02:00:5e:00:00:08"),
            "02:00:5e:00:00:08"
        );
    }

    #[test]
    fn every_assignment_description_states_what_it_replaces() {
        // The sentence the demo scenario produces, exactly.
        assert_eq!(
            describe_assign(
                "Amazon tablet",
                "02:00:5e:00:00:04",
                Some("Jamie"),
                Some("Arlo")
            ),
            "Assign Amazon tablet (02:00:5e:00:00:04) to Jamie (currently Arlo)"
        );
        assert_eq!(
            describe_assign("printer", "02:00:5e:00:00:06", Some("Jamie"), None),
            "Assign printer (02:00:5e:00:00:06) to Jamie"
        );
        assert_eq!(
            describe_assign("Amazon tablet", "02:00:5e:00:00:04", None, Some("Arlo")),
            "Unassign Amazon tablet (02:00:5e:00:00:04) from Arlo"
        );
        // A no-op still says so rather than pretending to be a change.
        assert_eq!(
            describe_assign("tablet", "mac", Some("Jamie"), Some("Jamie")),
            "Leave tablet (mac) assigned to Jamie"
        );
        assert_eq!(
            describe_assign("tablet", "mac", None, None),
            "Leave tablet (mac) unassigned"
        );
    }

    #[test]
    fn the_other_descriptions_name_the_thing_and_the_change() {
        assert_eq!(
            describe_rename_device("02:00:5e:00:00:08", "printer", "Office printer"),
            "Rename 02:00:5e:00:00:08 from 'printer' to 'Office printer'"
        );
        assert_eq!(describe_create_person("Aurora"), "Create person 'Aurora'");
        assert_eq!(
            describe_rename_person("Arlo", "Arlo Andrews"),
            "Rename person 'Arlo' to 'Arlo Andrews'"
        );
        assert_eq!(
            describe_delete_person("Arlo (5eed…0003)", 0),
            "Delete person Arlo (5eed…0003) (no devices)"
        );
        assert_eq!(
            describe_delete_person("Arlo", 1),
            "Delete person Arlo (1 device still assigned)"
        );
        assert_eq!(
            describe_delete_person("Arlo", 3),
            "Delete person Arlo (3 devices still assigned)"
        );

        assert_eq!(
            describe_set_notes("tablet", "mac", "on the guest VLAN"),
            "Set the notes on tablet (mac) to 'on the guest VLAN'"
        );
        assert_eq!(
            describe_set_notes("tablet", "mac", "   "),
            "Clear the notes on tablet (mac)"
        );

        assert_eq!(
            describe_set_flags("tablet", "mac", Some(true), Some(false)),
            "For tablet (mac): hide it from listings, and turn arrival and departure alerts off"
        );
        assert_eq!(
            describe_set_flags("tablet", "mac", None, Some(true)),
            "For tablet (mac): turn arrival and departure alerts on"
        );
        assert_eq!(
            describe_set_flags("tablet", "mac", None, None),
            "Change nothing about tablet (mac)"
        );

        assert_eq!(
            describe_set_notify("Jamie", true, true),
            "Notify about Jamie when they arrive and when they leave"
        );
        assert_eq!(
            describe_set_notify("Jamie", false, false),
            "Notify about Jamie never"
        );
        assert_eq!(
            describe_set_person_notes("Jamie", ""),
            "Clear the notes on Jamie"
        );
    }

    #[test]
    fn a_long_note_is_shortened_in_the_description_but_not_in_the_write() {
        let long = "a".repeat(200);
        let description = describe_set_notes("tablet", "mac", &long);
        assert!(description.len() < 120, "{description}");
        assert!(description.ends_with("…'"), "{description}");
    }

    // --- the change set the card shows ------------------------------------

    /// A card names the columns the write will touch, in the words the tools
    /// use. "and nothing else" is the claim that was false: a rename's card
    /// said it would rename a device, and the write also muted it.
    #[test]
    fn a_card_names_the_columns_that_will_change_and_says_there_are_no_others() {
        assert_eq!(
            describe_change_set(&["display_name"]),
            "Changes the name, and nothing else"
        );
        assert_eq!(
            describe_change_set(&["notify"]),
            "Changes the arrival and departure alerts, and nothing else"
        );
        assert_eq!(
            describe_change_set(&["hidden", "notify"]),
            "Changes whether it is hidden and the arrival and departure alerts, and nothing else"
        );
        assert_eq!(
            describe_change_set(&["display_name", "notes", "owner_item_id"]),
            "Changes the name, the notes and the owner, and nothing else"
        );
        assert_eq!(describe_change_set(&[]), "Changes nothing");
    }

    /// The card is written for a person, so it says "the owner" rather than
    /// `owner_item_id` — a column name is not something anybody was offered.
    /// Asserted as "no snake_case token", which is what a raw column name looks
    /// like and what an English clause never does.
    #[test]
    fn a_card_names_no_column_in_its_database_spelling() {
        let all = describe_change_set(crate::columns::USER_OWNED);
        assert!(
            !all.contains('_'),
            "the card shows a raw column name: {all}"
        );
        for column in crate::columns::USER_OWNED {
            let clause = describe_change_set(&[column]);
            assert_ne!(
                clause,
                format!("Changes {column}, and nothing else"),
                "{column} reached the card unrendered"
            );
        }
    }

    /// A change set built from a `DeviceEdit` is the edit's own column list, so
    /// the sentence cannot name something the statement will not write.
    #[test]
    fn a_cards_change_set_comes_from_the_edit_the_write_uses() {
        let edit = crate::model::DeviceEdit {
            display_name: Some("Office printer".into()),
            ..crate::model::DeviceEdit::default()
        };
        let card = describe_change_set(&edit.columns());
        assert_eq!(card, "Changes the name, and nothing else");
        assert!(!card.contains("alert"), "{card}");
        assert!(!card.contains("owner"), "{card}");
    }

    // -------------------------------------------------------------------------
    // Time rendering
    // -------------------------------------------------------------------------

    /// Issue #32: the relative phrase is what the model reasons from, so it
    /// rounds, and above a day it carries the hours too. 37 hours read "1 day
    /// ago" and was reported as "in the last 24 hours".
    #[test]
    fn relative_times_round_and_never_drop_the_hours_past_a_day() {
        let hour = 3_600;
        assert_ne!(relative(NOW - 37 * hour, NOW), "1 day ago");
        assert_eq!(relative(NOW - 37 * hour, NOW), "1 day 13 hours ago");
        // 1 hour 56 minutes: the nearest hour, said to be approximate.
        assert_eq!(relative(NOW - (hour + 56 * 60), NOW), "about 2 hours ago");
        assert_eq!(relative(NOW - (hour + 20 * 60), NOW), "about 1 hour ago");
        // Just under a day stays in hours, so it never reads as a day or more.
        assert_eq!(
            relative(NOW - (23 * hour + 40 * 60), NOW),
            "about 24 hours ago"
        );
        assert_eq!(relative(NOW - 24 * hour, NOW), "1 day ago");
        // 47 hours 50 minutes rounds to two days, not "1 day 23 hours".
        assert_eq!(relative(NOW - (47 * hour + 50 * 60), NOW), "2 days ago");
        assert_eq!(
            relative(NOW - (30 * hour + 10 * 60), NOW),
            "1 day 6 hours ago"
        );
        assert_eq!(relative(NOW - 59 * 60 - 40, NOW), "about 1 hour ago");
        assert_eq!(relative(NOW + 37 * hour, NOW), "1 day 13 hours from now");
    }

    #[test]
    fn timestamps_are_absolute_utc_and_relative_together() {
        assert_eq!(format_utc(NOW), "2026-08-24 12:00:00 UTC");
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");

        assert_eq!(relative(NOW, NOW), "just now");
        assert_eq!(relative(NOW - 89, NOW), "1 minute ago");
        assert_eq!(relative(NOW - 90, NOW), "2 minutes ago");
        assert_eq!(relative(NOW - 7_200, NOW), "2 hours ago");
        assert_eq!(relative(NOW - 86_400 * 3, NOW), "3 days ago");
        assert_eq!(relative(NOW + 7_200, NOW), "2 hours from now");

        assert_eq!(
            stamp(Some(NOW - 3_600), NOW),
            "2026-08-24 11:00:00 UTC (1 hour ago)"
        );
        assert_eq!(stamp(None, NOW), "never");
    }

    // -------------------------------------------------------------------------
    // Snapshots
    // -------------------------------------------------------------------------

    fn facts() -> DeviceFacts {
        DeviceFacts {
            id: 4,
            mac: "02:00:5e:00:00:04".into(),
            vendor: Some("Amazon Technologies".into()),
            device_type: Some("tablet".into()),
            os_family: Some("Android".into()),
            state: Some("offline".into()),
            last_ip: Some("10.0.2.18".into()),
            first_seen: Some(NOW - 86_400 * 190),
            last_seen: Some(NOW - 86_400 * 2),
            owner_item_id: Some(ARLO.into()),
            owner_name: Some("Arlo".into()),
            notes: Some("Tablet is on the guest VLAN.".into()),
            ..DeviceFacts::default()
        }
    }

    #[test]
    fn a_device_snapshot_names_everything_a_question_could_be_about() {
        let presence = vec![TimelineRow {
            label: String::new(),
            start: Some(NOW - 86_400 * 2 - 3_600),
            end: Some(NOW - 86_400 * 2),
            detail: None,
        }];
        let locations = vec![TimelineRow {
            label: "Guest".into(),
            start: Some(NOW - 86_400 * 2 - 3_600),
            end: None,
            detail: Some("Guest AP".into()),
        }];
        let events = vec![TimelineRow {
            label: "device_offline".into(),
            start: Some(NOW - 86_400 * 2),
            end: None,
            detail: None,
        }];

        let snapshot = render_device_snapshot(&facts(), &presence, &locations, &events, NOW);

        // The seed's tablet has no name of any kind, so the card-facing label is
        // what the daemon observed: the vendor's first word and the type.
        assert!(
            snapshot.contains("Device: Amazon tablet (02:00:5e:00:00:04)"),
            "{snapshot}"
        );
        assert!(snapshot.contains("MAC: 02:00:5e:00:00:04"), "{snapshot}");
        assert!(snapshot.contains("Numeric id: 4"), "{snapshot}");
        assert!(
            snapshot.contains(&format!("Owner: Arlo ({ARLO})")),
            "{snapshot}"
        );
        assert!(
            snapshot.contains("Last seen: 2026-08-22 12:00:00 UTC (2 days ago)"),
            "{snapshot}"
        );
        assert!(snapshot.contains("Hidden: no"), "{snapshot}");
        assert!(snapshot.contains("Notify: no"), "{snapshot}");
        assert!(snapshot.contains("Guest — "), "{snapshot}");
        assert!(snapshot.contains("(ongoing)"), "{snapshot}");
        assert!(snapshot.contains("(Guest AP)"), "{snapshot}");
        assert!(snapshot.contains("device_offline"), "{snapshot}");
        // A field the daemon never filled in is absent, not "none": a snapshot of
        // eight "none"s reads as a device nobody knows anything about.
        assert!(!snapshot.contains("Hostname:"), "{snapshot}");
    }

    // --- the one title ----------------------------------------------------

    /// The assistant's view of a device and the sync's view of the same row
    /// produce the same title, because they are the same function. When they
    /// were two, a printer's Item was titled after the OUI holder while the
    /// card called it by the name the daemon had resolved.
    #[test]
    fn the_title_the_assistant_computes_is_the_one_the_sync_derives() {
        let printer = DeviceFacts {
            id: 31,
            mac: "aa:bb:cc:00:01:02".into(),
            resolved_name: Some("Brother HL-L8360CDW series".into()),
            vendor: Some("CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD.".into()),
            ..DeviceFacts::default()
        };
        assert_eq!(printer.title(), "Brother HL-L8360CDW series");
        assert_eq!(printer.title(), printer.label());

        // And the same inputs through the sync's own entry point.
        let mut row = DeviceRow::new(31, "aa:bb:cc:00:01:02");
        row.resolved_name = Some("Brother HL-L8360CDW series".into());
        row.vendor = Some("CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD.".into());
        assert_eq!(crate::sync::derive_title(&row), printer.title());
    }

    /// The title falls back to the vendor where the card falls back to the
    /// vendor *and the type*: a title is a name, a card is a sentence somebody
    /// has to recognise the thing from. Both stop at the MAC.
    #[test]
    fn an_unnamed_device_is_titled_by_its_vendor_and_carded_by_its_type() {
        let tablet = facts();
        assert_eq!(tablet.title(), "Amazon Technologies device");
        assert_eq!(tablet.descriptive(), "Amazon tablet");

        let bare = DeviceFacts {
            mac: "02:00:5e:00:00:04".into(),
            ..DeviceFacts::default()
        };
        assert_eq!(bare.title(), "02:00:5e:00:00:04");
        assert_eq!(bare.label(), "02:00:5e:00:00:04");
    }

    // --- when the data starts ---------------------------------------------

    /// The sentence the network scope was missing. A model that finds zero
    /// presence spans and has not been told when monitoring began explains the
    /// emptiness as an outage, which is what happened.
    #[test]
    fn the_monitoring_window_says_when_the_data_starts_and_what_time_it_is() {
        let rendered = render_monitoring_window(Some(NOW - 3_600), NOW);
        assert!(
            rendered.contains("Monitoring data begins at 2026-08-24 11:00:00 UTC (1 hour ago)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("no data before that"),
            "the model is left to infer what an empty earlier window means: {rendered}"
        );
        assert!(
            rendered.contains("Current time: 2026-08-24 12:00:00 UTC"),
            "{rendered}"
        );
    }

    /// A database with nothing in it says so, rather than saying monitoring
    /// began at the epoch.
    #[test]
    fn a_database_with_no_observations_says_that_rather_than_a_timestamp() {
        let rendered = render_monitoring_window(None, NOW);
        assert!(
            rendered.contains("nothing has been observed yet"),
            "{rendered}"
        );
        assert!(!rendered.contains("1970"), "{rendered}");
        assert!(rendered.contains("Current time:"), "{rendered}");
    }

    /// The device scope gets the same sentence about its own device: its
    /// history starts at its first sighting whatever the rest of the network
    /// has been recording.
    #[test]
    fn a_device_snapshot_says_when_that_devices_history_starts() {
        let snapshot = render_device_snapshot(&facts(), &[], &[], &[], NOW);
        assert!(
            snapshot.contains("Monitoring data begins at 2026-02-15 12:00:00 UTC"),
            "{snapshot}"
        );
        assert!(snapshot.contains("Current time:"), "{snapshot}");
    }

    #[test]
    fn an_unowned_device_says_none_rather_than_leaving_the_line_out() {
        let mut facts = facts();
        facts.owner_item_id = None;
        facts.owner_name = None;
        facts.notes = None;
        let snapshot = render_device_snapshot(&facts, &[], &[], &[], NOW);
        assert!(snapshot.contains("Owner: none"), "{snapshot}");
        assert!(snapshot.contains("Notes: none"), "{snapshot}");
        assert!(
            snapshot.contains("Recent presence: none recorded"),
            "{snapshot}"
        );
    }

    #[test]
    fn a_snapshot_stays_under_the_cap_however_much_history_there_is() {
        // Two fences, and this proves the first one does the work: each timeline
        // renders at most HISTORY_ROWS rows and says how many it left out, so a
        // device with a year of history produces a snapshot the same size as one
        // with a day of it, and `cap` never has to fire.
        let rows: Vec<TimelineRow> = (0..500)
            .map(|i| TimelineRow {
                label: format!("location-{i}-{}", "x".repeat(60)),
                start: Some(NOW - i),
                end: Some(NOW),
                detail: None,
            })
            .collect();
        let snapshot = render_device_snapshot(&facts(), &rows, &rows, &rows, NOW);
        assert!(snapshot.len() <= SNAPSHOT_MAX_BYTES, "{}", snapshot.len());
        assert!(
            !snapshot.contains("[truncated]"),
            "the row cap should be enough"
        );
        assert!(snapshot.contains("… 490 more not shown"), "{snapshot}");
    }

    #[test]
    fn the_second_fence_cuts_at_a_line_boundary_and_says_so() {
        // A plugin that hands back more than the cap regardless — a device with
        // a pathological hostname on every line — is cut, and cut between lines
        // so no surviving line is half a fact.
        let text = (0..2_000)
            .map(|i| format!("Location {i}: somewhere with a long name\n"))
            .collect::<String>();
        let capped = cap(&text, SNAPSHOT_MAX_BYTES);
        assert!(capped.len() <= SNAPSHOT_MAX_BYTES + 16, "{}", capped.len());
        assert!(
            capped.ends_with("[truncated]"),
            "{}",
            &capped[capped.len() - 40..]
        );
        for line in capped.lines().filter(|l| *l != "[truncated]") {
            assert!(text.contains(&format!("{line}\n")), "half a line: {line}");
        }
    }

    #[test]
    fn a_timeline_says_how_many_rows_it_did_not_show() {
        let rows: Vec<TimelineRow> = (0..25)
            .map(|i| TimelineRow {
                label: format!("r{i}"),
                start: Some(NOW - i),
                end: Some(NOW),
                detail: None,
            })
            .collect();
        let rendered = render_timeline("Recent presence", &rows, NOW);
        assert_eq!(
            rendered.lines().filter(|l| l.starts_with("  r")).count(),
            HISTORY_ROWS
        );
        assert!(rendered.contains("… 15 more not shown"), "{rendered}");
    }

    fn device_line(mac: &str, owner: Option<&str>, state: &str) -> DeviceFacts {
        DeviceFacts {
            mac: mac.into(),
            device_type: Some("tablet".into()),
            state: Some(state.into()),
            last_seen: Some(NOW - 60),
            owner_item_id: owner.map(str::to_string),
            ..DeviceFacts::default()
        }
    }

    #[test]
    fn a_person_snapshot_lists_their_devices_or_says_there_are_none() {
        let person = PersonFacts {
            item_id: ARLO.into(),
            name: "Arlo".into(),
            notes: Some("notes".into()),
            notify_arrive: true,
            notify_depart: false,
            state: Some("away".into()),
            current_location: None,
            device_count: 1,
        };
        let devices = vec![device_line("02:00:5e:00:00:04", Some(ARLO), "offline")];

        let snapshot = render_person_snapshot(&person, &devices, NOW);
        assert!(snapshot.contains("Person: Arlo"), "{snapshot}");
        assert!(snapshot.contains(&format!("Uuid: {ARLO}")), "{snapshot}");
        assert!(snapshot.contains("Notify on arrival: yes"), "{snapshot}");
        assert!(snapshot.contains("Notify on departure: no"), "{snapshot}");
        assert!(snapshot.contains("Devices (1):"), "{snapshot}");
        assert!(snapshot.contains("02:00:5e:00:00:04"), "{snapshot}");

        let bare = PersonFacts {
            notes: None,
            notify_arrive: false,
            notify_depart: false,
            ..person
        };
        let empty = render_person_snapshot(&bare, &[], NOW);
        assert!(empty.contains("Devices: none assigned"), "{empty}");
        assert!(empty.contains("Notes: none"), "{empty}");
    }

    /// What [`crate::queries::SELECT_DEVICE_COUNTS`] returns for these
    /// devices, worked out here the way the statement's `GROUP BY`s do it, so
    /// the renderer can be tested without a database. The statement itself is
    /// run against the daemon's schema in `daemon_schema_test`.
    fn count_rows(devices: &[DeviceFacts], people: &[&str]) -> Vec<DeviceCount> {
        let mut rows: Vec<DeviceCount> = Vec::new();
        let mut bump = |dimension: &str, value: &str| match rows
            .iter_mut()
            .find(|r| r.dimension == dimension && r.value == value)
        {
            Some(row) => row.devices += 1,
            None => rows.push(DeviceCount {
                dimension: dimension.into(),
                value: value.into(),
                devices: 1,
            }),
        };
        for d in devices {
            bump("state", d.state.as_deref().unwrap_or("unknown"));
            bump("vendor", d.vendor.as_deref().unwrap_or(""));
            bump("type", d.device_type.as_deref().unwrap_or(""));
            let owner = match d.owner_item_id.as_deref() {
                None => "none",
                Some(id) if people.contains(&id) => "person",
                Some(_) => "missing",
            };
            bump("owner", owner);
            bump("hidden", if d.hidden { "yes" } else { "no" });
        }
        rows
    }

    #[test]
    fn a_network_snapshot_gives_totals_and_people_and_no_device_lines() {
        let people = vec![
            PersonFacts {
                item_id: JAMIE.into(),
                name: "Jamie".into(),
                device_count: 1,
                state: Some("home".into()),
                current_location: Some("Studio".into()),
                ..PersonFacts::default()
            },
            PersonFacts {
                item_id: ARLO.into(),
                name: "Arlo".into(),
                device_count: 1,
                state: Some("away".into()),
                ..PersonFacts::default()
            },
        ];
        let devices = vec![
            device_line("02:00:5e:00:00:01", Some(JAMIE), "online"),
            device_line("02:00:5e:00:00:04", Some(ARLO), "offline"),
            device_line("02:00:5e:00:00:06", None, "idle"),
            // An owner id with no person behind it: a real state, counted on
            // its own rather than as owned.
            device_line(
                "02:00:5e:00:00:0d",
                Some("5eed0000-0000-4000-8000-00000000dead"),
                "online",
            ),
        ];
        let counts = DeviceCounts::from_rows(&count_rows(&devices, &[JAMIE, ARLO]));

        let snapshot = render_network_snapshot(&people, &counts, 3, Some(NOW - 86_400), NOW);

        assert!(
            snapshot.contains("Monitoring data begins at 2026-08-23 12:00:00 UTC (1 day ago)"),
            "{snapshot}"
        );
        assert!(snapshot.contains("People (2):"), "{snapshot}");
        assert!(snapshot.contains("Jamie (5eed0000"), "{snapshot}");
        assert!(snapshot.contains("1 device, home at Studio"), "{snapshot}");
        assert!(
            snapshot.contains("Devices by state (4 total): online 2, idle 1, offline 1."),
            "{snapshot}"
        );
        assert!(
            snapshot.contains(
                "Owners: 2 belong to a person, 1 to nobody, 1 to an owner id that names no person."
            ),
            "{snapshot}"
        );
        assert!(snapshot.contains("call list_devices"), "{snapshot}");
        assert!(
            snapshot.contains("Security events in the last 24 hours: 3"),
            "{snapshot}"
        );
        // The point of it: no device is listed, so the context does not grow
        // with the network.
        for mac in ["02:00:5e:00:00:01", "02:00:5e:00:00:0d"] {
            assert!(!snapshot.contains(mac), "{mac} listed: {snapshot}");
        }
    }

    /// The network context stays the same size however many devices there
    /// are: it is sent with every model call, and on a 171-device network it
    /// used to be most of the 10,000-byte snapshot cap.
    #[test]
    fn the_network_snapshot_does_not_grow_with_the_device_count() {
        let devices: Vec<DeviceFacts> = (0..171).map(synthetic_device).collect();
        let counts = DeviceCounts::from_rows(&count_rows(&devices, &[]));
        let snapshot = render_network_snapshot(&[], &counts, 0, Some(NOW - 86_400), NOW);
        assert!(
            snapshot.len() < 1_500,
            "{} bytes: {snapshot}",
            snapshot.len()
        );
        assert!(
            snapshot.contains("Devices by state (171 total): online 73, idle 22, offline 76."),
            "{snapshot}"
        );
    }

    #[test]
    fn an_empty_network_still_renders_something_a_model_can_read() {
        let snapshot = render_network_snapshot(&[], &DeviceCounts::default(), 0, None, NOW);
        assert!(
            snapshot.contains("nothing has been observed yet"),
            "{snapshot}"
        );
        assert!(snapshot.contains("People: none"), "{snapshot}");
        assert!(
            snapshot.contains("Devices by state (0 total): none."),
            "{snapshot}"
        );
    }

    /// One of 171 synthetic devices: 73 online, 22 idle, 76 offline; 41 from
    /// one vendor, 21 from another, 45 with no vendor, the rest spread out.
    fn synthetic_device(i: usize) -> DeviceFacts {
        let state = match i {
            0..73 => "online",
            73..95 => "idle",
            _ => "offline",
        };
        // Vendors interleaved with states, so a row cap that keeps the first
        // rows would see a skewed sample of each.
        let vendor = match i % 171 {
            n if n % 4 == 0 && n < 164 => Some("Vendor A".to_string()),
            n if n % 8 == 1 => Some("Vendor B".to_string()),
            n if n % 4 == 2 || n % 4 == 3 && n < 60 => None,
            n => Some(format!("Vendor {}", n % 7)),
        };
        DeviceFacts {
            id: i as i64 + 1,
            mac: format!("02:00:5e:00:{:02x}:{:02x}", i / 256, i % 256),
            resolved_name: Some(format!("device-{i}")),
            vendor,
            device_type: Some(["phone", "media_player", "computer"][i % 3].to_string()),
            state: Some(state.to_string()),
            last_seen: Some(NOW - i as i64 * 600),
            ..DeviceFacts::default()
        }
    }

    /// Issue #33: on 171 devices the listing stopped at about 109 lines and
    /// the model counted those. The header's counts are the database's over
    /// every device, and they are exact however hard the rows are capped.
    #[test]
    fn a_listing_of_171_devices_has_exact_header_counts_whatever_the_cap() {
        let devices: Vec<DeviceFacts> = (0..171).map(synthetic_device).collect();
        let rows = count_rows(&devices, &[]);
        let counts = DeviceCounts::from_rows(&rows);
        let vendor_a = devices
            .iter()
            .filter(|d| d.vendor.as_deref() == Some("Vendor A"))
            .count();
        let no_vendor = devices.iter().filter(|d| d.vendor.is_none()).count();
        assert_eq!(counts.total, 171);

        for cap in [RESULT_MAX_BYTES, 6_000, 2_500, 0] {
            let (content, summary) = render_device_listing("(all)", &counts, &devices, NOW, cap);
            assert!(content.starts_with("171 devices (all)."), "{content}");
            assert!(
                content.contains("By state: online 73, idle 22, offline 76."),
                "cap {cap}: {content}"
            );
            assert!(
                content.contains(&format!("By vendor: Vendor A {vendor_a},")),
                "cap {cap}: {content}"
            );
            assert!(
                content.contains(&format!("no vendor recorded {no_vendor}.")),
                "cap {cap}: {content}"
            );
            assert!(
                content.contains("By type: computer 57, media_player 57, phone 57."),
                "cap {cap}: {content}"
            );
            assert!(
                content.contains("Owners: 0 belong to a person, 171 to nobody."),
                "cap {cap}: {content}"
            );
            let listed = content.lines().filter(|l| l.starts_with("  ")).count();
            assert!(listed < 171, "cap {cap} listed every row");
            assert!(
                content.contains(&format!("Showing {listed} of 171.")),
                "cap {cap}: {content}"
            );
            assert!(
                summary.starts_with(&format!("Showing {listed} of 171 devices (all)")),
                "cap {cap}: {summary}"
            );
            assert!(
                content.contains("vendor:<name>"),
                "how to narrow: {content}"
            );
        }

        // Every row fits when the cap allows and the limit lets them through.
        let few: Vec<DeviceFacts> = devices[..5].to_vec();
        let five = DeviceCounts::from_rows(&count_rows(&few, &[]));
        let (content, summary) = render_device_listing("(all)", &five, &few, NOW, RESULT_MAX_BYTES);
        assert!(!content.contains("Showing"), "{content}");
        assert!(summary.starts_with("Listed 5 devices (all)"), "{summary}");
    }

    #[test]
    fn a_listing_line_carries_the_vendor_and_the_owner() {
        let mut device = synthetic_device(0);
        device.owner_item_id = Some(JAMIE.into());
        device.owner_name = Some("Jamie".into());
        let line = device.listing_line(NOW);
        assert!(line.contains("Vendor A, phone, online"), "{line}");
        assert!(line.contains("owner Jamie"), "{line}");
        assert!(!line.contains(JAMIE), "{line}");
    }

    #[test]
    fn device_filters_cover_state_vendor_and_type() {
        assert_eq!(parse_device_filter("idle").unwrap(), DeviceFilter::Idle);
        assert_eq!(
            parse_device_filter("Offline").unwrap(),
            DeviceFilter::Offline
        );
        assert_eq!(
            parse_device_filter("vendor: Ubiquiti").unwrap(),
            DeviceFilter::Vendor("Ubiquiti".into())
        );
        assert_eq!(
            parse_device_filter("type:phone").unwrap(),
            DeviceFilter::Type("phone".into())
        );
        assert!(parse_device_filter("vendor:").is_err());
        assert!(parse_device_filter("sideways").is_err());
        let params = DeviceFilter::Vendor("Ubi".into()).sql_params(None);
        assert_eq!(params[1], json!("Ubi"));
        assert_eq!(params[0], json!(""));
        let params = DeviceFilter::Owner("Jamie".into()).sql_params(Some(JAMIE));
        assert_eq!(params[5], json!(JAMIE));
    }

    #[test]
    fn an_events_call_parses_a_window_and_refuses_a_wide_one() {
        let q = EventQuery::parse(&json!({}), NOW).unwrap();
        assert_eq!((q.start, q.end), (NOW - 86_400, NOW + 1));
        assert!(q.window.starts_with("in the last 24 hours"), "{}", q.window);
        let q = EventQuery::parse(
            &json!({"from": "2026-11-17T23:00:00Z", "to": "2026-11-18T23:00:00Z",
                    "type": "gratuitous_arp", "security": true}),
            NOW,
        )
        .unwrap();
        assert_eq!(q.types, vec!["gratuitous_arp".to_string()]);
        assert!(q.security);
        assert_eq!(q.sql_params(7)[4], json!(7));
        assert!(EventQuery::parse(&json!({"hours": 169}), NOW).is_err());
        assert!(EventQuery::parse(&json!({"from": "2026-11-17T23:00:00Z"}), NOW).is_err());
        assert!(EventQuery::parse(&json!({"type": "arp scan; drop"}), NOW).is_err());
        assert!(
            EventQuery::parse(
                &json!({"from": "2026-11-01T00:00:00Z", "to": "2026-11-09T00:00:00Z"}),
                NOW
            )
            .is_err()
        );
    }

    #[test]
    fn an_events_answer_counts_by_type_before_any_row_and_says_what_it_cut() {
        let q = EventQuery::parse(&json!({"limit": 3}), NOW).unwrap();
        let types = vec![
            EventTypeCount {
                event_type: "arp_scan".into(),
                events: 812,
                sent: 0,
            },
            EventTypeCount {
                event_type: "went_offline".into(),
                events: 30,
                sent: 4,
            },
        ];
        let devices = vec![EventDeviceCount {
            device_id: Some(1),
            events: 800,
            device_total: 12,
            mac: Some("02:00:5e:00:00:01".into()),
            resolved_name: Some("scanner".into()),
            ..EventDeviceCount::default()
        }];
        let rows: Vec<EventWindowFacts> = (0..3)
            .map(|i| EventWindowFacts {
                event_type: "arp_scan".into(),
                ts: Some(NOW - i * 60),
                details: Some("{\"targets\": 254}".into()),
                mac: Some("02:00:5e:00:00:01".into()),
                ..EventWindowFacts::default()
            })
            .collect();
        let (content, summary) =
            render_events(&q, None, &types, &devices, &rows, NOW, RESULT_MAX_BYTES);
        assert!(
            content.contains("842 events in all, 4 of them sent as alerts"),
            "{content}"
        );
        assert!(
            content.contains("  arp_scan: 812 (security)\n"),
            "{content}"
        );
        assert!(
            content.contains("  went_offline: 30, 4 sent as alerts\n"),
            "{content}"
        );
        assert!(
            content.contains("By device, busiest first (1 of 12):"),
            "{content}"
        );
        assert!(
            content.contains("scanner (02:00:5e:00:00:01): 800"),
            "{content}"
        );
        assert!(content.contains("Showing 3 of 842 events."), "{content}");
        assert!(
            content.find("arp_scan: 812") < content.find("Newest first"),
            "{content}"
        );
        assert_eq!(summary, format!("842 events {}; showing 3", q.window));

        let (none, _) = render_events(&q, None, &[], &[], &[], NOW, RESULT_MAX_BYTES);
        assert!(none.contains("No events matched."), "{none}");
    }

    #[test]
    fn state_counts_are_ordered_most_common_first() {
        let devices = vec![
            device_line("a", None, "idle"),
            device_line("b", None, "online"),
            device_line("c", None, "online"),
            DeviceFacts {
                mac: "d".into(),
                ..DeviceFacts::default()
            },
        ];
        assert_eq!(
            count_by_state(&devices),
            vec![
                ("online".to_string(), 2),
                ("idle".to_string(), 1),
                ("unknown".to_string(), 1)
            ]
        );
    }

    #[test]
    fn who_was_online_groups_by_person_and_puts_the_unowned_last() {
        let span = |mac: &str, owner: Option<(&str, &str)>, end: Option<i64>| PresenceWindowFacts {
            mac: mac.into(),
            owner_item_id: owner.map(|(id, _)| id.to_string()),
            owner_name: owner.map(|(_, name)| name.to_string()),
            start: Some(NOW - 3_600),
            end,
            ..PresenceWindowFacts::default()
        };
        let spans = vec![
            span("02:00:5e:00:00:06", None, Some(NOW - 60)),
            span("02:00:5e:00:00:01", Some((JAMIE, "Jamie")), None),
            span("02:00:5e:00:00:04", Some((ARLO, "Arlo")), Some(NOW - 600)),
        ];

        let rendered = render_who_was_online(&spans, NOW);
        let jamie_at = rendered.find("Jamie").expect("Jamie is listed");
        let nobody_at = rendered
            .find("nobody in particular")
            .expect("the unowned device is listed, not dropped");
        assert!(jamie_at < nobody_at, "the residue goes last:\n{rendered}");
        assert!(rendered.contains("and still online"), "{rendered}");
        assert!(rendered.contains("02:00:5e:00:00:06"), "{rendered}");

        assert_eq!(
            render_who_was_online(&[], NOW),
            "Nothing was online in that window."
        );
    }

    #[test]
    fn an_owner_id_with_no_person_behind_it_is_said_out_loud() {
        // The seed carries one of these on purpose. Saying "none" would be a lie
        // the model would then repeat to the person.
        let facts = DeviceFacts {
            mac: "02:00:5e:00:00:0d".into(),
            owner_item_id: Some("5eed0000-0000-4000-8000-00000000dead".into()),
            owner_name: None,
            ..DeviceFacts::default()
        };
        assert!(facts.owner_phrase().contains("no person with that id"));
        assert!(facts.owner().is_none());

        let unowned = DeviceFacts::default();
        assert_eq!(unowned.owner_phrase(), "none");
    }

    #[test]
    fn an_event_becomes_a_timeline_row_without_repeating_an_empty_detail() {
        let event = EventFacts {
            event_type: "arp_spoof".into(),
            ts: Some(NOW),
            details: Some(r#"{"claimed_ip":"10.0.1.1"}"#.into()),
            mac: None,
        };
        let row = event.row();
        assert_eq!(row.label, "arp_spoof");
        assert_eq!(row.start, Some(NOW));
        assert!(row.detail.unwrap().contains("claimed_ip"));

        for empty in [None, Some("null".to_string()), Some("{}".to_string())] {
            let row = EventFacts {
                event_type: "new_device".into(),
                ts: Some(NOW),
                details: empty,
                mac: None,
            }
            .row();
            assert!(row.detail.is_none(), "an empty detail is not a detail");
        }
    }

    #[test]
    fn capping_never_splits_a_multibyte_character() {
        let text = "naïve café ☕\nsecond line\nthird line\n";
        for max in 1..text.len() {
            let capped = cap(text, max);
            assert!(capped.is_char_boundary(capped.len()));
        }
        assert_eq!(cap(text, 10_000), text);
    }

    // --- the overview's questions ----------------------------------------

    #[test]
    fn a_day_is_optional_and_must_be_a_real_calendar_day() {
        let arg = |v: serde_json::Value| serde_json::json!({ "day": v });
        assert_eq!(parse_day(&serde_json::json!({}), "day"), Ok(None));
        assert_eq!(parse_day(&arg(serde_json::json!("  ")), "day"), Ok(None));
        assert_eq!(
            parse_day(&arg(serde_json::json!("2026-09-25")), "day"),
            Ok(Some("2026-09-25".to_string()))
        );
        assert_eq!(
            parse_day(&arg(serde_json::json!("2028-02-29")), "day"),
            Ok(Some("2028-02-29".to_string())),
            "a leap day is a day"
        );
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "25/09/2026",
            "2026-9-25",
            "today",
            "2026-09-25T00:00",
        ] {
            let refused = parse_day(&arg(serde_json::json!(bad)), "day");
            assert!(refused.is_err(), "{bad} was accepted");
            assert!(refused.unwrap_err().contains("YYYY-MM-DD"));
        }
    }

    fn movement(event_type: &str, name: Option<&str>, at: i64) -> MovementFacts {
        MovementFacts {
            event_type: event_type.to_string(),
            ts: Some(at),
            day: Some("2026-09-25".into()),
            person_item_id: name.map(|_| "0193a5a0-0000-7000-8000-00000000000a".to_string()),
            person_name: name.map(str::to_string),
            ..MovementFacts::default()
        }
    }

    #[test]
    fn a_day_of_movements_reads_in_order_and_ends_with_who_is_home() {
        let now = 1_790_000_000;
        let mut arrived = movement("person_arrived", Some("Jamie"), now - 3_600);
        arrived.location = Some("Studio".into());
        arrived.via = Some("Driveway AP".into());
        arrived.device_mac = Some("02:00:5e:00:00:02".into());
        arrived.device_resolved_name = Some("jamie-phone".into());
        let left = movement("person_departed", None, now - 600);
        let home = [HomeFacts {
            item_id: "0193a5a0-0000-7000-8000-00000000000a".into(),
            name: "Jamie".into(),
            current_location: Some("Studio".into()),
            arrived: Some(now - 3_600),
            devices_online: 2,
        }];

        let out = render_movements("2026-09-25", "2026-09-25", &[arrived, left], &home, now);
        assert!(
            out.starts_with("Arrivals and departures on 2026-09-25 (today)."),
            "{out}"
        );
        assert!(
            out.contains("1 arrival, 1 departure, oldest first"),
            "{out}"
        );
        assert!(
            out.contains(
                "Jamie arrived, at Studio, via Driveway AP, on jamie-phone (02:00:5e:00:00:02)"
            ),
            "{out}"
        );
        assert!(out.contains("Somebody (no person recorded) left"), "{out}");
        assert!(out.find("arrived").unwrap_or(0) < out.find(" left").unwrap_or(0));
        assert!(out.contains("Home now (1):"), "{out}");
        assert!(out.contains("home since"), "{out}");
        assert!(out.contains("2 devices online"), "{out}");
        assert!(out.contains("UTC"), "every time says its zone: {out}");
    }

    #[test]
    fn a_quiet_day_says_so_and_says_why_a_day_can_be_quiet() {
        let out = render_movements("2026-09-20", "2026-09-25", &[], &[], 1_790_000_000);
        assert!(
            out.starts_with("Arrivals and departures on 2026-09-20."),
            "not today: {out}"
        );
        assert!(
            out.contains("Nobody arrived or left on 2026-09-20"),
            "{out}"
        );
        assert!(out.contains("assigned to them"), "{out}");
        assert!(out.contains("Nobody is home now."), "{out}");
    }

    #[test]
    fn a_person_home_with_no_arrival_time_is_home_without_an_invented_time() {
        let person = HomeFacts {
            item_id: "0193a5a0-0000-7000-8000-00000000000c".into(),
            name: "Arlo".into(),
            current_location: None,
            arrived: None,
            devices_online: 1,
        };
        let line = person.render_line(1_790_000_000);
        assert!(line.contains("home (no arrival time recorded)"), "{line}");
        assert!(line.contains("1 device online"), "{line}");
        assert!(!line.contains("never"), "{line}");
    }

    fn new_device() -> NewDeviceFacts {
        NewDeviceFacts {
            id: 12,
            mac: "02:00:5e:00:00:0c".into(),
            resolved_name: Some("guest-phone".into()),
            identity_source: Some("dhcp".into()),
            vendor: Some("Apple, Inc.".into()),
            device_type: Some("phone".into()),
            device_type_confidence: Some(0.92),
            os_family: Some("iOS".into()),
            state: Some("online".into()),
            first_seen: Some(1_790_000_000 - 7_200),
            ..NewDeviceFacts::default()
        }
    }

    #[test]
    fn a_new_device_line_says_what_it_looks_like_and_what_is_left_to_do() {
        let now = 1_790_000_000;
        let line = new_device().render_line(now);
        for expected in [
            "guest-phone (02:00:5e:00:00:0c) [id 12]",
            "first seen",
            "looks like a phone (92% sure)",
            "iOS",
            "made by Apple, Inc.",
            "online",
            "no owner",
            "the daemon guesses 'guest-phone', from dhcp",
        ] {
            assert!(line.contains(expected), "missing {expected:?}: {line}");
        }

        let mut unknown = new_device();
        unknown.resolved_name = None;
        unknown.device_type = None;
        unknown.device_type_confidence = None;
        unknown.vendor = None;
        let line = unknown.render_line(now);
        assert!(line.contains("not yet identified"), "{line}");
        assert!(line.contains("not named by anyone"), "{line}");
        assert!(
            !line.contains("% sure"),
            "a confidence with no verdict: {line}"
        );
    }

    #[test]
    fn the_new_device_answer_counts_what_still_needs_a_person() {
        let now = 1_790_000_000;
        let mut named = new_device();
        named.id = 13;
        named.display_name = Some("Guest phone".into());
        let mut owned = new_device();
        owned.id = 14;
        owned.owner_item_id = Some("0193a5a0-0000-7000-8000-00000000000a".into());
        owned.owner_name = Some("Jamie".into());

        let out = render_new_devices(&[new_device(), named, owned], 168, now);
        assert!(
            out.starts_with("3 devices first seen in the last seven days (since "),
            "{out}"
        );
        assert!(out.contains("1 of them has neither a name"), "{out}");
        assert!(out.contains("/devices/todo"), "{out}");
        assert!(out.contains("owner Jamie"), "{out}");

        assert_eq!(
            render_new_devices(&[], 24, now),
            "No device was first seen in the last 24 hours (since 2026-09-20 14:13:20 UTC).\n"
        );
    }

    /// "jamie " is Jamie: the rule the owner form, Add person, the bulk form
    /// and the assistant all ask through.
    #[test]
    fn a_name_differing_only_in_case_or_surrounding_space_is_the_same_person() {
        assert!(same_name("Jamie", "jamie "));
        assert!(same_name("  JAMIE", "Jamie"));
        assert!(same_name("Élodie", "élodie"));
        assert!(!same_name("Jamie", "Jaime"));
        assert!(!same_name("Mary Ann", "Maryann"));
        assert_eq!(name_key("  Jamie\t"), "jamie");
    }

    #[test]
    fn a_possessive_mdns_name_suggests_its_owner_and_nothing_else_does() {
        assert_eq!(
            possessive_owner("Jeremy's Mac Studio").as_deref(),
            Some("Jeremy")
        );
        assert_eq!(
            possessive_owner("JAMIE\u{2019}s iPhone").as_deref(),
            Some("JAMIE")
        );
        assert_eq!(
            possessive_owner("Me's MacBook Pro (4)").as_deref(),
            Some("Me")
        );
        assert_eq!(possessive_owner("HP-LaserJet"), None);
        assert_eq!(possessive_owner("'s iPad"), None);
        assert_eq!(possessive_owner("Jeremys Mac"), None);
    }
}
