//! The People pages' forms: Add person, sort devices in bulk, merge, delete.
//!
//! The same shape as the row menu's forms in `forms.rs`, and the same shell: a
//! `GET` renders a page carrying the token the kernel minted for it, and the
//! `POST` from that page is the only thing that changes anything. Every write
//! goes through `assist_host`, the path the assistant's tools use too, so a
//! person created here and one created in a conversation are created by one
//! function (`docs/UI.md`, People model).
//!
//! Merge and delete are two pages each, never one click: the first `GET` says
//! exactly what will change, and nothing changes until its button is pressed.

use netgrasp_core::assist::{self, DeviceFacts, PersonFacts};
use std::collections::BTreeMap;
use trovato_sdk::types::{ApiRequest, ApiResponse};

use crate::assist_host::{self, PersonFound};
use crate::forms::{
    FORMS_SCRIPT, chosen_owner, done, escape_html, field, forget_created, owner_fields,
    owner_words, posted_target_and_back, problem, safe_back, sorted_people, themed,
    themed_with_status,
};

/// Add a person.
pub(crate) const PERSON_ADD: &str = "/people/add";
/// Sort several devices at once.
pub(crate) const DEVICES_ASSIGN: &str = "/devices/assign";
/// Merge one person into another.
pub(crate) const PERSON_MERGE: &str = "/netgrasp/person/merge";
/// Delete a person.
pub(crate) const PERSON_DELETE: &str = "/netgrasp/person/delete";

/// Where the People pages return to.
const PEOPLE: &str = "/people";

/// Where the bulk form returns to.
const TO_SORT: &str = "/devices/todo";

// ===========================================================================
// Add person
// ===========================================================================

pub(crate) fn person_add_form(request: &ApiRequest) -> ApiResponse {
    let back = back_or(request.query.get("back").map(String::as_str), PEOPLE);
    let body = format!(
        r#"<form method="post" action="{action}" class="ng-form">
{hidden}
<p><label for="ng-name">Name</label>
<input type="text" id="ng-name" name="name" maxlength="{max}" autocomplete="off" autofocus required></p>
<p><label><input type="checkbox" name="arrive" value="1"> Alert me when they get home</label></p>
<p class="ng-form__hint">A person is home when any device they own is online. Give them devices from the owner form or from <a href="{assign}">Sort devices</a>.</p>
<p><button type="submit">Add person</button> <a href="{back_attr}">Cancel</a></p>
</form>"#,
        action = PERSON_ADD,
        hidden = token_and_back(request, &back),
        max = assist_host::MAX_PERSON_NAME,
        assign = DEVICES_ASSIGN,
        back_attr = escape_html(&back),
    );
    themed("Add person", &body)
}

pub(crate) fn person_add_save(request: &ApiRequest) -> ApiResponse {
    let back = back_or(Some(&field(&request.body, "back")), PEOPLE);
    let name = field(&request.body, "name");
    let arrive = field(&request.body, "arrive") == "1";
    match assist_host::find_or_create_person(&name, arrive) {
        Ok(PersonFound::Created(person)) => done(
            &person_page(&person.item_id),
            &format!("Added <strong>{}</strong>.", escape_html(&person.name)),
        ),
        // Refused, and linked: the person the reader meant already exists.
        Ok(PersonFound::Existing(person)) => themed_with_status(
            422,
            "Already here",
            &format!(
                r#"<p class="ng-form__problem"><a href="{page}">{name}</a> already exists, so nobody new was added.</p>
<p><a href="{back}">Back</a></p>"#,
                page = escape_html(&person_page(&person.item_id)),
                name = escape_html(&person.name),
                back = escape_html(&back),
            ),
        ),
        Err(message) => problem(&back, &format!("{message} Nothing was changed.")),
    }
}

// ===========================================================================
// Sort devices in bulk
// ===========================================================================

/// One owner an mDNS name suggests, and the devices that suggest them.
struct Suggestion {
    /// The name as the first device spelled it ("Jeremy", "JAMIE").
    name: String,
    /// The lower-cased key the link carries.
    key: String,
    count: usize,
}

/// Group the to-sort devices by the owner their mDNS name announces.
fn suggestions(devices: &[DeviceFacts]) -> Vec<Suggestion> {
    let mut by_key: BTreeMap<String, Suggestion> = BTreeMap::new();
    for device in devices {
        let Some(owner) = device
            .mdns_name
            .as_deref()
            .and_then(assist::possessive_owner)
        else {
            continue;
        };
        let key = assist::name_key(&owner);
        by_key
            .entry(key.clone())
            .or_insert(Suggestion {
                name: owner,
                key,
                count: 0,
            })
            .count += 1;
    }
    let mut out: Vec<Suggestion> = by_key.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Whether a device is one a suggestion names.
fn suggested(device: &DeviceFacts, key: &str) -> bool {
    device
        .mdns_name
        .as_deref()
        .and_then(assist::possessive_owner)
        .is_some_and(|owner| assist::name_key(&owner) == key)
}

pub(crate) fn devices_assign_form(request: &ApiRequest) -> ApiResponse {
    let back = back_or(request.query.get("back").map(String::as_str), TO_SORT);
    let devices = match assist_host::load_devices_to_sort() {
        Ok(devices) => devices,
        Err(message) => return problem(&back, &message),
    };
    let people = match assist_host::load_people() {
        Ok(people) => sorted_people(people),
        Err(message) => return problem(&back, &message),
    };
    if devices.is_empty() {
        return themed(
            "Sort devices",
            &format!(
                r#"<p>Every device has a name or an owner. Nothing is left to sort.</p>
<p><a href="{back}">Back</a></p>"#,
                back = escape_html(&back)
            ),
        );
    }

    let suggest = request
        .query
        .get("suggest")
        .map(|s| assist::name_key(s))
        .unwrap_or_default();
    // `person=` preselects an owner: the person page's "Add a device" link.
    let preselected = request.query.get("person").cloned().or_else(|| {
        people
            .iter()
            .find(|p| !suggest.is_empty() && assist::name_key(&p.name) == suggest)
            .map(|p| p.item_id.clone())
    });

    let mut html = String::new();
    let hints = suggestions(&devices);
    if !hints.is_empty() {
        html.push_str(
            r#"<div class="ng-form__suggestions"><p>Their names say whose they are:</p><ul>"#,
        );
        for hint in &hints {
            html.push_str(&format!(
                r#"<li>{count} device{s} named &ldquo;{name}&rsquo;s &hellip;&rdquo;: <a href="{href}">select them</a></li>"#,
                count = hint.count,
                s = if hint.count == 1 { " is" } else { "s are" },
                name = escape_html(&hint.name),
                href = escape_html(&format!(
                    "{DEVICES_ASSIGN}?suggest={}",
                    url_encode(&hint.key)
                )),
            ));
        }
        html.push_str("</ul><p class=\"ng-form__hint\">A suggestion only: nothing is assigned until you press Assign.</p></div>");
    }

    html.push_str(&format!(
        r#"<form method="post" action="{action}" class="ng-form ng-form--bulk">
{hidden}
<fieldset class="ng-form__devices"><legend>{count} device{s} with no name and no owner, online first</legend>
<ul class="ng-checklist">"#,
        action = DEVICES_ASSIGN,
        hidden = token_and_back(request, &back),
        count = devices.len(),
        s = if devices.len() == 1 { "" } else { "s" },
    ));
    for device in &devices {
        let ticked = !suggest.is_empty() && suggested(device, &suggest);
        html.push_str(&format!(
            r#"<li><label><input type="checkbox" name="device" value="{mac}"{checked}> <span class="ng-checklist__name">{label}</span> <span class="ng-checklist__meta">{meta}</span></label></li>"#,
            mac = escape_html(&device.mac),
            checked = if ticked { " checked" } else { "" },
            label = escape_html(&device.label()),
            meta = escape_html(&device_meta(device)),
        ));
    }
    html.push_str("</ul></fieldset>\n");
    html.push_str(&owner_fields(&people, preselected.as_deref(), "owner"));
    // A suggestion for somebody who does not exist yet fills in the new name,
    // in the spelling the device used.
    if preselected.is_none()
        && let Some(hint) = hints.iter().find(|h| h.key == suggest)
    {
        html = html.replacen(
            r#"name="new_person" "#,
            &format!(r#"name="new_person" value="{}" "#, escape_html(&hint.name)),
            1,
        );
    }
    html.push_str(&format!(
        r#"
<p><button type="submit">Assign</button> <a href="{back}">Cancel</a></p>
</form>{FORMS_SCRIPT}"#,
        back = escape_html(&back),
    ));
    themed("Sort devices", &html)
}

pub(crate) fn devices_assign_save(request: &ApiRequest) -> ApiResponse {
    let back = back_or(Some(&field(&request.body, "back")), TO_SORT);
    let refs = fields(&request.body, "device");
    if refs.is_empty() {
        return problem(&back, "No device was ticked. Nothing was changed.");
    }
    let mut devices = Vec::with_capacity(refs.len());
    for raw in &refs {
        match crate::forms::resolve_device(raw) {
            Ok(facts) => devices.push(facts),
            Err(message) => return problem(&back, &format!("{message}. Nothing was changed.")),
        }
    }
    // Resolved before any write, so a typo in the name changes nothing.
    let owner = match chosen_owner(&request.body, "owner") {
        Ok(Some(owner)) => owner,
        Ok(None) => {
            return problem(
                &back,
                "Choose who they belong to, or type a name. Nothing was changed.",
            );
        }
        Err(message) => return problem(&back, &format!("{message} Nothing was changed.")),
    };

    match assist_host::move_devices(&devices, Some(&owner.0.item_id)) {
        Ok(()) => done(
            &back,
            &format!(
                "{count} device{s} now belong{verb} to {named}.",
                count = devices.len(),
                s = if devices.len() == 1 { "" } else { "s" },
                verb = if devices.len() == 1 { "s" } else { "" },
                named = owner_words(Some(&owner), &field(&request.body, "new_person")),
            ),
        ),
        Err(message) => {
            if owner.1 {
                forget_created(&owner.0);
            }
            problem(&back, &format!("{message}. Nothing was changed."))
        }
    }
}

// ===========================================================================
// Merge
// ===========================================================================

pub(crate) fn person_merge_form(request: &ApiRequest) -> ApiResponse {
    let target = request.query.get("target").cloned().unwrap_or_default();
    let back = back_or(request.query.get("back").map(String::as_str), PEOPLE);
    let from = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };
    let people = match assist_host::load_people() {
        Ok(people) => sorted_people(people),
        Err(message) => return problem(&back, &message),
    };
    let others: Vec<&PersonFacts> = people
        .iter()
        .filter(|p| p.item_id != from.item_id)
        .collect();

    let into_id = request.query.get("into").cloned().unwrap_or_default();
    if into_id.is_empty() {
        return merge_choose(&from, &others, &back);
    }
    if into_id == from.item_id {
        return problem(&back, "A person cannot be merged into themselves.");
    }
    let Some(into) = others.iter().find(|p| p.item_id == into_id) else {
        return problem(&back, "That person no longer exists.");
    };
    merge_confirm(request, &from, into, &back)
}

/// Step one: who to keep. A `GET` form, because choosing changes nothing.
fn merge_choose(from: &PersonFacts, others: &[&PersonFacts], back: &str) -> ApiResponse {
    if others.is_empty() {
        return themed(
            "Merge",
            &format!(
                r#"<p>{name} is the only person here, so there is nobody to merge them into.</p>
<p><a href="{back}">Back</a></p>"#,
                name = escape_html(&from.name),
                back = escape_html(back),
            ),
        );
    }
    let mut options = String::new();
    for person in others {
        options.push_str(&format!(
            r#"<option value="{id}">{name}</option>"#,
            id = escape_html(&person.item_id),
            name = escape_html(&person.name),
        ));
    }
    let body = format!(
        r#"<form method="get" action="{action}" class="ng-form">
<input type="hidden" name="target" value="{target}">
<input type="hidden" name="back" value="{back}">
<p class="ng-form__subject">{name}</p>
<p>Merge {name} into somebody else when they are the same person. Choose the one to keep: {name}'s devices become theirs, and {name} is removed.</p>
<p><label for="ng-into">Keep</label>
<select id="ng-into" name="into">{options}</select></p>
<p><button type="submit">Show what will change</button> <a href="{back}">Cancel</a></p>
</form>"#,
        action = PERSON_MERGE,
        target = escape_html(&from.item_id),
        back = escape_html(back),
        name = escape_html(&from.name),
    );
    themed(&format!("Merge {} into…", from.name), &body)
}

/// Step two: exactly what will change, and the button that changes it.
fn merge_confirm(
    request: &ApiRequest,
    from: &PersonFacts,
    into: &PersonFacts,
    back: &str,
) -> ApiResponse {
    let devices = match assist_host::load_devices_of(&from.item_id) {
        Ok(devices) => devices,
        Err(message) => return problem(back, &message),
    };
    let movements = assist_host::person_movement_count(&from.item_id).unwrap_or(0);
    let from_name = escape_html(&from.name);
    let into_name = escape_html(&into.name);

    let mut html = format!(
        r#"<form method="post" action="{action}" class="ng-form ng-form--confirm">
<input type="hidden" name="_token" value="{token}">
<input type="hidden" name="target" value="{target}">
<input type="hidden" name="into" value="{into_id}">
<input type="hidden" name="back" value="{back_attr}">
<p class="ng-form__subject">Merge {from_name} into {into_name}</p>
"#,
        action = PERSON_MERGE,
        token = escape_html(&request.csrf_token),
        target = escape_html(&from.item_id),
        into_id = escape_html(&into.item_id),
        back_attr = escape_html(back),
    );

    // The devices.
    if devices.is_empty() {
        html.push_str(&format!(
            "<p>{from_name} has no devices, so none move.</p>\n"
        ));
    } else {
        html.push_str(&format!(
            "<p>{n} device{s} move{verb} from {from_name} to {into_name}:</p>\n<ul class=\"ng-form__list\">",
            n = devices.len(),
            s = if devices.len() == 1 { "" } else { "s" },
            verb = if devices.len() == 1 { "s" } else { "" },
        ));
        for device in &devices {
            html.push_str(&format!(
                "<li>{}</li>",
                escape_html(&assist::device_phrase(&device.descriptive(), &device.mac))
            ));
        }
        html.push_str("</ul>\n");
    }

    // The history.
    html.push_str(&format!(
        "<p>{phrase} {into_name}'s, under {into_name}'s name. The daemon's records of them are not changed.</p>\n",
        phrase = match movements {
            0 => format!("{from_name} has no comings and goings recorded; any that arrive late are shown as"),
            1 => format!("{from_name}'s 1 past coming or going is shown as"),
            n => format!("{from_name}'s {n} past comings and goings are shown as"),
        }
    ));

    // The alerts: whose win, and what would be lost.
    html.push_str(&format!(
        "<p>Alerts: {into_name}'s settings win. {into_name}: {kept}. {from_name}: {merged}.</p>\n",
        kept = alerts_words(into.notify_arrive, into.notify_depart),
        merged = alerts_words(from.notify_arrive, from.notify_depart),
    ));
    let lost_arrive = from.notify_arrive && !into.notify_arrive;
    let lost_depart = from.notify_depart && !into.notify_depart;
    if lost_arrive || lost_depart {
        let what = match (lost_arrive, lost_depart) {
            (true, true) => "arrival and departure alerts",
            (true, false) => "arrival alerts",
            _ => "departure alerts",
        };
        // Pre-ticked: a merge never quietly turns an alert off.
        html.push_str(&format!(
            r#"<p><label><input type="checkbox" name="keep" value="1" checked> {from_name} had {what} on. Keep them for {into_name}</label></p>
"#
        ));
    } else {
        html.push_str("<p>No alert is lost.</p>\n");
    }

    html.push_str(&format!(
        r#"<p>Then {from_name} is removed.</p>
<p><button type="submit">Merge {from_name} into {into_name}</button> <a href="{back}">Cancel</a></p>
</form>"#,
        back = escape_html(back),
    ));
    themed(&format!("Merge {} into {}", from.name, into.name), &html)
}

pub(crate) fn person_merge_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let back = back_or(Some(&back), PEOPLE);
    let into_id = field(&request.body, "into");
    let keep = field(&request.body, "keep") == "1";
    let from = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };
    let into = match assist_host::load_person(&into_id) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };

    match assist_host::merge_people(&from, &into, keep) {
        Ok(moved) => done(
            &person_page(&into.item_id),
            &format!(
                "<strong>{from}</strong> was merged into <strong>{into}</strong>. {n} device{s} moved.",
                from = escape_html(&from.name),
                into = escape_html(&into.name),
                n = moved.len(),
                s = if moved.len() == 1 { "" } else { "s" },
            ),
        ),
        Err(message) => problem(&back, &format!("{message}. Nothing was changed.")),
    }
}

// ===========================================================================
// Delete
// ===========================================================================

pub(crate) fn person_delete_form(request: &ApiRequest) -> ApiResponse {
    let target = request.query.get("target").cloned().unwrap_or_default();
    let back = back_or(request.query.get("back").map(String::as_str), PEOPLE);
    let person = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };
    let devices = match assist_host::load_devices_of(&person.item_id) {
        Ok(devices) => devices,
        Err(message) => return problem(&back, &message),
    };
    let name = escape_html(&person.name);

    let mut html = format!(
        r#"<form method="post" action="{action}" class="ng-form ng-form--confirm">
<input type="hidden" name="_token" value="{token}">
<input type="hidden" name="target" value="{target}">
<input type="hidden" name="back" value="{back_attr}">
<p class="ng-form__subject">Delete {name}</p>
"#,
        action = PERSON_DELETE,
        token = escape_html(&request.csrf_token),
        target = escape_html(&person.item_id),
        back_attr = escape_html(&back),
    );
    if devices.is_empty() {
        html.push_str(&format!("<p>{name} has no devices.</p>\n"));
    } else {
        html.push_str(&format!(
            "<p>{lead} will have no owner. {they} not deleted, and {stay} on every list:</p>\n<ul class=\"ng-form__list\">",
            lead = if devices.len() == 1 {
                "This device".to_string()
            } else {
                format!("These {} devices", devices.len())
            },
            they = if devices.len() == 1 { "It is" } else { "They are" },
            stay = if devices.len() == 1 { "stays" } else { "stay" },
        ));
        for device in &devices {
            html.push_str(&format!(
                "<li>{}</li>",
                escape_html(&assist::device_phrase(&device.descriptive(), &device.mac))
            ));
        }
        html.push_str("</ul>\n");
    }
    html.push_str(&format!(
        r#"<p>{name}'s alerts go with them. Their past comings and goings stay, under the name {name}.</p>
<p><button type="submit">Delete {name}</button> <a href="{back}">Cancel</a></p>
</form>"#,
        back = escape_html(&back),
    ));
    themed(&format!("Delete {}", person.name), &html)
}

pub(crate) fn person_delete_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let back = back_or(Some(&back), PEOPLE);
    let person = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };
    match assist_host::remove_person(&person) {
        // Back to the list, not to `back`: that may be the page of the person
        // who no longer exists.
        Ok(unassigned) => done(
            PEOPLE,
            &format!(
                "<strong>{name}</strong> was deleted. {n} device{s} now belong{verb} to nobody.",
                name = escape_html(&person.name),
                n = unassigned.len(),
                s = if unassigned.len() == 1 { "" } else { "s" },
                verb = if unassigned.len() == 1 { "s" } else { "" },
            ),
        ),
        Err(message) => problem(&back, &format!("{message}. Nothing was changed.")),
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// A person's page.
pub(crate) fn person_page(item_id: &str) -> String {
    format!("/item/{item_id}")
}

/// A return path, or `fallback` when there is none or it leaves the site.
pub(crate) fn back_or(raw: Option<&str>, fallback: &str) -> String {
    match raw.map(str::trim) {
        Some(value) if !value.is_empty() => {
            let safe = safe_back(Some(value));
            if safe == value {
                safe
            } else {
                fallback.to_string()
            }
        }
        _ => fallback.to_string(),
    }
}

/// The token and the way back, as the two hidden fields every POST form here
/// starts with.
pub(crate) fn token_and_back(request: &ApiRequest, back: &str) -> String {
    format!(
        r#"<input type="hidden" name="_token" value="{token}">
<input type="hidden" name="back" value="{back}">"#,
        token = escape_html(&request.csrf_token),
        back = escape_html(back),
    )
}

/// One line about a device for the bulk list: vendor, address, online or not.
pub(crate) fn device_meta(device: &DeviceFacts) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(mdns) = device.mdns_name.as_deref().filter(|m| !m.trim().is_empty())
        && device.label() != mdns
    {
        parts.push(mdns.to_string());
    }
    if let Some(vendor) = device.vendor.as_deref().filter(|v| !v.trim().is_empty()) {
        parts.push(vendor.to_string());
    }
    parts.push(device.mac.clone());
    if device.state.as_deref() == Some("online") {
        parts.push("online".to_string());
    }
    parts.join(" · ")
}

/// A person's alerts in words, for the merge page.
fn alerts_words(arrive: bool, depart: bool) -> &'static str {
    match (arrive, depart) {
        (true, true) => "alerts on arrival and departure",
        (true, false) => "alerts on arrival",
        (false, true) => "alerts on departure",
        (false, false) => "no alerts",
    }
}

/// Every value of a repeated field, in order.
///
/// The bulk form posts one `device` per ticked box. `ApiRequest::query` keeps
/// only the last value of a repeated key, which is why the selection travels in
/// a POST body and is read here, never in the query string (`docs/UI.md`).
pub(crate) fn fields(body: &str, name: &str) -> Vec<String> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| field(&format!("{name}={value}"), name))
        .filter(|value| !value.trim().is_empty())
        .collect()
}

/// Percent-encode a query value: everything but the unreserved characters.
pub(crate) fn url_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn device(mac: &str, mdns: Option<&str>) -> DeviceFacts {
        DeviceFacts {
            mac: mac.to_string(),
            mdns_name: mdns.map(str::to_string),
            ..DeviceFacts::default()
        }
    }

    #[test]
    fn a_repeated_field_is_read_whole_and_in_order() {
        let body = "_token=t&device=02%3A00%3A00%3A00%3A00%3A01&owner=&device=02%3A00%3A00%3A00%3A00%3A02&device=";
        assert_eq!(
            fields(body, "device"),
            vec!["02:00:00:00:00:01", "02:00:00:00:00:02"]
        );
        assert!(fields(body, "missing").is_empty());
    }

    #[test]
    fn suggestions_group_by_owner_ignoring_case_most_first() {
        let devices = vec![
            device("02:00:00:00:00:01", Some("Jeremy's Mac Studio")),
            device("02:00:00:00:00:02", Some("JAMIE\u{2019}s iPhone")),
            device("02:00:00:00:00:03", Some("jeremy's iPad")),
            device("02:00:00:00:00:04", Some("HP-LaserJet")),
            device("02:00:00:00:00:05", None),
        ];
        let hints = suggestions(&devices);
        assert_eq!(hints.len(), 2);
        assert_eq!(
            (
                hints[0].name.as_str(),
                hints[0].key.as_str(),
                hints[0].count
            ),
            ("Jeremy", "jeremy", 2)
        );
        assert_eq!((hints[1].key.as_str(), hints[1].count), ("jamie", 1));
        assert!(suggested(&devices[2], "jeremy"));
        assert!(!suggested(&devices[3], "jeremy"));
    }

    #[test]
    fn a_return_path_off_the_site_falls_back_to_the_page_given() {
        assert_eq!(back_or(Some("https://evil.example/"), PEOPLE), PEOPLE);
        assert_eq!(back_or(None, TO_SORT), TO_SORT);
        assert_eq!(back_or(Some("/item/abc"), PEOPLE), "/item/abc");
    }

    #[test]
    fn a_suggestion_key_is_encoded_for_the_link() {
        assert_eq!(url_encode("jamie"), "jamie");
        assert_eq!(url_encode("mary ann"), "mary%20ann");
        assert_eq!(url_encode("é"), "%C3%A9");
    }
}
