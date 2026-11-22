//! The Alerts page: what you will be told about, whether it will reach you,
//! a test to prove it, and what was sent (`docs/UI.md`, Alerts).
//!
//! Three paths, each a page and its form, served by `tap_api` like the row
//! menu's forms and sharing their shell (`forms.rs`):
//!
//! - `/alerts`: the page (`GET`) and Send test (`POST`).
//! - `/alerts/add`: add an alert for a person or a device.
//! - `/alerts/devices`: "only the devices I choose", the bulk form.
//!
//! # The daemon is the only sender
//!
//! The plugin has no outbound HTTP and cannot read netgraspd's config, so it
//! never sends anything and never knows where alerts go. What it shows comes
//! from three daemon tables: `ng_notify_status` (configured or not, by which
//! transport, how the last attempt went), `ng_notify_requests` (the test it
//! asked for and the daemon's answer), and `ng_events.notified` (what was
//! delivered). The topic and the token are in none of them.
//!
//! Send test inserts one row into `ng_notify_requests`. The daemon reads it on
//! its reconcile tick, sends one test alert, and writes the outcome back.
//!
//! # Every write is an existing one
//!
//! Turning a person's alert on or off posts to the person notify form's own
//! save; a device's, to the device notify form's. The add form and the bulk
//! form call the same functions those saves call (`assist_host::apply_person_save`
//! and `apply_device_edit`), so an alert set here and one set from a row menu
//! or in a conversation are set by one function.

use netgrasp_core::alerts::{
    self, AlertingCounts, NotifyStatus, SentAlert, TestRequest, arrive_rule, depart_rule,
    device_rule,
};
use netgrasp_core::assist::{DeviceFacts, PersonFacts};
use netgrasp_core::model::DeviceEdit;
use netgrasp_core::queries;
use serde_json::json;
use trovato_sdk::types::{ApiRequest, ApiResponse};

use crate::forms::{
    DEVICE_NOTIFY, FORMS_SCRIPT, PERSON_NOTIFY, PERSON_PRESENCE, done, escape_html, field, problem,
    resolve_device, sorted_people, themed,
};
use crate::people_forms::{back_or, device_meta, fields, token_and_back};
use crate::{assist_host, db};

/// The page, and Send test.
pub(crate) const ALERTS: &str = "/alerts";
/// Add an alert.
pub(crate) const ALERTS_ADD: &str = "/alerts/add";
/// Only the devices I choose.
pub(crate) const ALERTS_DEVICES: &str = "/alerts/devices";

/// How many delivered alerts the page lists.
const RECENT_ROWS: i64 = 20;

/// How many devices the page lists under each heading before it counts the
/// rest. The bulk form lists every one.
const DEVICES_SHOWN: usize = 12;

/// How many devices the bulk form will list. Past this it says so.
const BULK_LIMIT: i64 = 500;

// ===========================================================================
// What the page reads
// ===========================================================================

/// Everything the page draws, read in one place so a test can render it.
#[derive(Debug, Default)]
pub(crate) struct AlertsView {
    pub today: String,
    pub status: Option<NotifyStatus>,
    pub test: Option<TestRequest>,
    pub people: Vec<PersonFacts>,
    pub alerting: Vec<DeviceFacts>,
    pub counts: AlertingCounts,
    pub recent: Vec<SentAlert>,
}

fn read_view() -> Result<AlertsView, String> {
    #[derive(serde::Deserialize)]
    struct Today {
        today: String,
    }
    let read = |what: &str, e: netgrasp_core::CoreError| format!("could not read {what}: {e}");
    let today = db::query_rows::<Today>(queries::SELECT_TODAY, &[])
        .map_err(|e| read("the date", e))?
        .into_iter()
        .next()
        .map(|t| t.today)
        .unwrap_or_default();
    let status = db::query_rows::<NotifyStatus>(queries::SELECT_NOTIFY_STATUS, &[])
        .map_err(|e| read("the delivery status", e))?
        .into_iter()
        .next();
    let test = db::query_rows::<TestRequest>(queries::SELECT_LATEST_TEST_REQUEST, &[])
        .map_err(|e| read("the last test", e))?
        .into_iter()
        .next();
    let people = sorted_people(assist_host::load_people()?);
    let alerting =
        db::query_rows::<DeviceFacts>(queries::SELECT_DEVICES_ALERTING, &[json!(BULK_LIMIT)])
            .map_err(|e| read("the devices", e))?;
    let counts = db::query_rows::<AlertingCounts>(queries::SELECT_ALERTING_COUNTS, &[])
        .map_err(|e| read("the device counts", e))?
        .into_iter()
        .next()
        .unwrap_or_default();
    let recent = db::query_rows::<SentAlert>(queries::SELECT_RECENTLY_SENT, &[json!(RECENT_ROWS)])
        .map_err(|e| read("what was sent", e))?;
    Ok(AlertsView {
        today,
        status,
        test,
        people,
        alerting,
        counts,
        recent,
    })
}

// ===========================================================================
// The page
// ===========================================================================

pub(crate) fn alerts_page(request: &ApiRequest) -> ApiResponse {
    match read_view() {
        Ok(view) => page("Alerts", &render(&view, &request.csrf_token)),
        Err(message) => problem("/overview", &message),
    }
}

/// The page body. Separate from the reads so a unit test can draw it.
pub(crate) fn render(view: &AlertsView, token: &str) -> String {
    let mut out = String::with_capacity(8192);
    let token = escape_html(token);

    // Delivery.
    out.push_str("<section class=\"ng-alerts__section ng-alerts__delivery\"><h2>Delivery</h2>");
    let configured = view.status.as_ref().is_some_and(|s| s.notify_configured);
    let ok = view.status.as_ref().and_then(|s| s.last_delivery_ok);
    let state = match (configured, ok) {
        (false, _) => "off",
        (true, Some(false)) => "failing",
        (true, _) => "on",
    };
    out.push_str(&format!(
        "<div class=\"ng-alerts__status ng-alerts__status--{state}\">"
    ));
    for sentence in alerts::delivery_sentences(view.status.as_ref(), &view.today) {
        out.push_str(&format!("<p>{}</p>", escape_html(&sentence)));
    }
    out.push_str("</div>");
    if let Some(sentence) = alerts::test_sentence(view.test.as_ref(), &view.today) {
        out.push_str(&format!(
            "<p class=\"ng-alerts__test\">{}</p>",
            escape_html(&sentence)
        ));
    }
    if configured {
        out.push_str(&format!(
            "<form method=\"post\" action=\"{ALERTS}\" class=\"ng-alerts__send\">\
             <input type=\"hidden\" name=\"_token\" value=\"{token}\">\
             <button type=\"submit\">Send a test alert</button></form>"
        ));
    }
    out.push_str("</section>");

    // People, then their devices.
    out.push_str(&format!(
        "<section class=\"ng-alerts__section\"><h2>People</h2>\
         <p class=\"ng-alerts__actions\"><a class=\"button\" href=\"{ALERTS_ADD}\">Add an alert</a></p>"
    ));
    if view.people.is_empty() {
        out.push_str(
            "<p class=\"ng-empty\">Nobody here yet. <a href=\"/people/add?back=%2Falerts\">Add a person</a> \
             and give them their devices, and you can be told when they get home.</p>",
        );
    }
    out.push_str("<ul class=\"ng-alerts__people\">");
    for person in &view.people {
        out.push_str(&person_block(person, view, &token));
    }
    out.push_str("</ul></section>");

    // Devices nobody owns.
    let unowned: Vec<&DeviceFacts> = view
        .alerting
        .iter()
        .filter(|d| d.owner_name.is_none())
        .collect();
    out.push_str(&format!(
        "<section class=\"ng-alerts__section\"><h2>Devices</h2>\
         <p class=\"ng-alerts__count\">{count}</p>\
         <p class=\"ng-alerts__actions\"><a class=\"button\" href=\"{ALERTS_DEVICES}\">Only the devices I choose</a></p>",
        count = escape_html(&alerts::devices_sentence(view.counts)),
    ));
    if !unowned.is_empty() {
        out.push_str("<h3>Nobody's</h3><ul class=\"ng-alerts__rules\">");
        for device in unowned.iter().take(DEVICES_SHOWN) {
            out.push_str(&device_rule_line(device, &token));
        }
        out.push_str("</ul>");
        if unowned.len() > DEVICES_SHOWN {
            out.push_str(&format!(
                "<p class=\"ng-alerts__more\">and {} more. <a href=\"{ALERTS_DEVICES}\">See them all</a>.</p>",
                unowned.len() - DEVICES_SHOWN
            ));
        }
    }
    out.push_str("</section>");

    // Recently sent.
    out.push_str("<section class=\"ng-alerts__section\"><h2>Recently sent</h2>");
    if view.recent.is_empty() {
        out.push_str("<p class=\"ng-empty\">Nothing has been sent yet.</p>");
    } else {
        out.push_str("<ul class=\"ng-alerts__sent\">");
        for alert in &view.recent {
            out.push_str(&format!(
                "<li><span class=\"ng-alerts__when\">{when}</span> {what}</li>",
                when = escape_html(&alerts::when(
                    alert.day.as_deref(),
                    alert.clock.as_deref(),
                    &view.today,
                    None
                )),
                what = escape_html(&alerts::sent_sentence(alert)),
            ));
        }
        out.push_str("</ul>");
    }
    out.push_str("</section>");
    out
}

/// One person: their rules as sentences, each with the switch that turns it
/// off, then the devices of theirs that alert.
fn person_block(person: &PersonFacts, view: &AlertsView, token: &str) -> String {
    let name = escape_html(&person.name);
    let id = escape_html(&person.item_id);
    let mut out =
        format!("<li class=\"ng-alerts__person\"><h3><a href=\"/item/{id}\">{name}</a></h3>");
    // An untracked person never arrives or leaves, so their rules never fire.
    // Say so rather than list them as if they would; never "away".
    if netgrasp_core::presence::state_label(person.state.as_deref()) == "Not tracked" {
        out.push_str(&format!(
            "<p class=\"ng-alerts__presence\">Not tracked: nobody is told when {name} \
             arrives or leaves until <a href=\"{PERSON_PRESENCE}?target={id}&amp;back={ALERTS}\">\
             tracking is on for them</a>.</p>"
        ));
    }
    out.push_str("<ul class=\"ng-alerts__rules\">");
    // Each switch posts both flags, because the person notify save reads both:
    // an absent one is off. So turning one off carries the other as it is.
    let switch = |arrive: bool, depart: bool, label: &str| {
        format!(
            "<form method=\"post\" action=\"{PERSON_NOTIFY}\" class=\"ng-alerts__switch\">\
             <input type=\"hidden\" name=\"_token\" value=\"{token}\">\
             <input type=\"hidden\" name=\"target\" value=\"{id}\">\
             <input type=\"hidden\" name=\"back\" value=\"{ALERTS}\">\
             {a}{d}<button type=\"submit\">{label}</button></form>",
            a = if arrive {
                "<input type=\"hidden\" name=\"arrive\" value=\"1\">"
            } else {
                ""
            },
            d = if depart {
                "<input type=\"hidden\" name=\"depart\" value=\"1\">"
            } else {
                ""
            },
        )
    };
    let (arrive, depart) = (person.notify_arrive, person.notify_depart);
    if arrive {
        out.push_str(&format!(
            "<li class=\"ng-alerts__rule\"><span>{}</span>{}</li>",
            escape_html(&arrive_rule(&person.name)),
            switch(false, depart, "Turn off")
        ));
    }
    if depart {
        out.push_str(&format!(
            "<li class=\"ng-alerts__rule\"><span>{}</span>{}</li>",
            escape_html(&depart_rule(&person.name)),
            switch(arrive, false, "Turn off")
        ));
    }
    if !arrive {
        out.push_str(&format!(
            "<li class=\"ng-alerts__rule ng-alerts__rule--off\"><span>Not told when {name} gets home.</span>{}</li>",
            switch(true, depart, "Tell me")
        ));
    }
    for device in view
        .alerting
        .iter()
        .filter(|d| d.owner_item_id.as_deref() == Some(person.item_id.as_str()))
    {
        out.push_str(&device_rule_line(device, token));
    }
    out.push_str("</ul></li>");
    out
}

/// "Tell me when Jamie's iPhone comes online." and the switch that mutes it,
/// which is the device notify form's own save.
fn device_rule_line(device: &DeviceFacts, token: &str) -> String {
    format!(
        "<li class=\"ng-alerts__rule ng-alerts__rule--device\"><span>{sentence}</span>\
         <form method=\"post\" action=\"{DEVICE_NOTIFY}\" class=\"ng-alerts__switch\">\
         <input type=\"hidden\" name=\"_token\" value=\"{token}\">\
         <input type=\"hidden\" name=\"target\" value=\"{mac}\">\
         <input type=\"hidden\" name=\"back\" value=\"{ALERTS}\">\
         <input type=\"hidden\" name=\"value\" value=\"0\">\
         <button type=\"submit\">Turn off</button></form></li>",
        sentence = escape_html(&device_rule(&device.label())),
        mac = escape_html(&device.mac),
    )
}

/// The Alerts pages are wider than a one-field form, so they get their own
/// wrapper class rather than `forms::themed`'s.
fn page(title: &str, body: &str) -> ApiResponse {
    ApiResponse::themed(
        title,
        format!(
            "<link rel=\"stylesheet\" href=\"/static/css/netgrasp.css\">\n\
             <div class=\"ng-page ng-alerts-page\">{body}</div>"
        ),
    )
}

// ===========================================================================
// Send test
// ===========================================================================

pub(crate) fn alerts_test_save(request: &ApiRequest) -> ApiResponse {
    if let Err(e) = db::exec(queries::INSERT_TEST_REQUEST, &[json!(request.user_id)]) {
        return problem(ALERTS, &format!("Could not ask netgraspd for a test: {e}"));
    }
    // Longer than `done`'s two seconds: the daemon answers on its reconcile
    // tick, ten seconds by default, and the page it returns to shows the answer.
    let body = format!(
        r#"<meta http-equiv="refresh" content="12; url={ALERTS}">
<p class="ng-form__done">Asked netgraspd to send a test alert.</p>
<p>It sends it on its next check, within about ten seconds, to wherever alerts go. The Alerts page then says whether it was delivered.</p>
<p><a href="{ALERTS}">Back to Alerts</a></p>"#
    );
    themed("Test alert requested", &body)
}

// ===========================================================================
// Add an alert
// ===========================================================================

pub(crate) fn alerts_add_form(request: &ApiRequest) -> ApiResponse {
    let back = back_or(request.query.get("back").map(String::as_str), ALERTS);
    let people = match assist_host::load_people() {
        Ok(people) => sorted_people(people),
        Err(message) => return problem(&back, &message),
    };
    let devices = match db::query_rows::<DeviceFacts>(
        queries::SELECT_DEVICES_FOR_ALERTS,
        &[json!(BULK_LIMIT)],
    ) {
        Ok(devices) => devices,
        Err(e) => return problem(&back, &format!("could not read the devices: {e}")),
    };
    let chosen_person = request.query.get("person").cloned().unwrap_or_default();
    let chosen_device = request.query.get("device").cloned().unwrap_or_default();

    let mut html = format!(
        r#"<form method="post" action="{ALERTS_ADD}" class="ng-form ng-form--alerts">
{hidden}
<fieldset><legend>When somebody comes or goes</legend>
"#,
        hidden = token_and_back(request, &back),
    );
    if people.is_empty() {
        html.push_str(
            "<p>Nobody here yet. <a href=\"/people/add?back=%2Falerts%2Fadd\">Add a person</a> first.</p>",
        );
    } else {
        let mut options = String::new();
        for person in &people {
            options.push_str(&format!(
                r#"<option value="{id}"{sel}>{name}</option>"#,
                id = escape_html(&person.item_id),
                sel = if person.item_id == chosen_person {
                    " selected"
                } else {
                    ""
                },
                name = escape_html(&person.name),
            ));
        }
        html.push_str(&format!(
            r#"<p><label for="ng-person">Tell me when</label>
<select id="ng-person" name="person">{options}</select></p>
<p><label for="ng-when">does this</label>
<select id="ng-when" name="when"><option value="arrive">gets home</option><option value="depart">leaves</option></select></p>
<p><button type="submit" name="kind" value="person">Add this alert</button></p>
"#
        ));
    }
    html.push_str("</fieldset>\n<fieldset><legend>When a device comes online</legend>\n");
    if devices.is_empty() {
        html.push_str("<p>There are no devices yet.</p>");
    } else {
        let mut options = String::new();
        for device in &devices {
            let owner = device
                .owner_name
                .as_deref()
                .map(|o| format!(" ({o}'s)"))
                .unwrap_or_default();
            let state = if device.notify { ", already on" } else { "" };
            options.push_str(&format!(
                r#"<option value="{mac}"{sel}>{label}{owner}{state}</option>"#,
                mac = escape_html(&device.mac),
                sel = if device.mac == chosen_device {
                    " selected"
                } else {
                    ""
                },
                label = escape_html(&device.label()),
                owner = escape_html(&owner),
            ));
        }
        html.push_str(&format!(
            r#"<p><label for="ng-device">Tell me when this comes online</label>
<input type="search" class="ng-form__filter" data-ng-filter="ng-device" placeholder="Search devices" aria-label="Search devices" hidden>
<select id="ng-device" name="device">{options}</select></p>
<p class="ng-form__hint">"Comes online" is a device netgraspd had seen go offline coming back, and a device it has never seen before.</p>
<p><button type="submit" name="kind" value="device">Add this alert</button></p>
"#
        ));
    }
    html.push_str(&format!(
        "</fieldset>\n<p><a href=\"{back}\">Cancel</a></p>\n</form>{FORMS_SCRIPT}",
        back = escape_html(&back),
    ));
    themed("Add an alert", &html)
}

pub(crate) fn alerts_add_save(request: &ApiRequest) -> ApiResponse {
    let back = back_or(Some(&field(&request.body, "back")), ALERTS);
    match field(&request.body, "kind").as_str() {
        "person" => add_person_alert(&request.body, &back),
        "device" => add_device_alert(&request.body, &back),
        _ => problem(&back, "Choose a person or a device. Nothing was changed."),
    }
}

fn add_person_alert(body: &str, back: &str) -> ApiResponse {
    let target = field(body, "person");
    let when = field(body, "when");
    if when != "arrive" && when != "depart" {
        return problem(back, "Choose gets home or leaves. Nothing was changed.");
    }
    let Ok(person) = assist_host::load_person(&target) else {
        return problem(back, "That person no longer exists. Nothing was changed.");
    };
    let (arrive, depart) = if when == "arrive" {
        (true, person.notify_depart)
    } else {
        (person.notify_arrive, true)
    };
    let sentence = if when == "arrive" {
        arrive_rule(&person.name)
    } else {
        depart_rule(&person.name)
    };
    let already =
        (when == "arrive" && person.notify_arrive) || (when == "depart" && person.notify_depart);
    if already {
        return done(
            back,
            &format!(
                "That alert was already on: <strong>{}</strong>",
                escape_html(&sentence)
            ),
        );
    }
    // The same write the person notify form makes: the Item, every other field
    // carried forward, then the mirror the daemon reads.
    let existing = match assist_host::load_person_item(&person.item_id) {
        Ok(item) => item,
        Err(message) => return problem(back, &message),
    };
    let payload = assist_host::person_payload(
        &person.item_id,
        &existing,
        None,
        None,
        Some((arrive, depart)),
    );
    match assist_host::apply_person_save(&payload) {
        Ok(_) => done(
            back,
            &format!("Alert added: <strong>{}</strong>", escape_html(&sentence)),
        ),
        Err(message) => problem(back, &message),
    }
}

fn add_device_alert(body: &str, back: &str) -> ApiResponse {
    let facts = match resolve_device(&field(body, "device")) {
        Ok(facts) => facts,
        Err(message) => return problem(back, &format!("{message}. Nothing was changed.")),
    };
    let sentence = device_rule(&facts.label());
    if facts.notify {
        return done(
            back,
            &format!(
                "That alert was already on: <strong>{}</strong>",
                escape_html(&sentence)
            ),
        );
    }
    let edit = DeviceEdit {
        notify: Some(true),
        ..DeviceEdit::default()
    };
    match assist_host::apply_device_edit(&facts, &edit) {
        Ok(_) => done(
            back,
            &format!("Alert added: <strong>{}</strong>", escape_html(&sentence)),
        ),
        Err(message) => problem(back, &message),
    }
}

// ===========================================================================
// Only the devices I choose
// ===========================================================================

pub(crate) fn alerts_devices_form(request: &ApiRequest) -> ApiResponse {
    let back = back_or(request.query.get("back").map(String::as_str), ALERTS);
    let devices = match db::query_rows::<DeviceFacts>(
        queries::SELECT_DEVICES_FOR_ALERTS,
        &[json!(BULK_LIMIT)],
    ) {
        Ok(devices) => devices,
        Err(e) => return problem(&back, &format!("could not read the devices: {e}")),
    };
    if devices.is_empty() {
        return themed(
            "Only the devices I choose",
            &format!(
                "<p>There are no devices yet.</p><p><a href=\"{}\">Back</a></p>",
                escape_html(&back)
            ),
        );
    }
    let on = devices.iter().filter(|d| d.notify).count();
    let mut html = format!(
        r#"<form method="post" action="{ALERTS_DEVICES}" class="ng-form ng-form--bulk">
{hidden}
<p>Tick the devices you want to hear about when they come online. Every device left unticked is muted. {on} of {total} are ticked now.</p>
<fieldset class="ng-form__devices"><legend>{total} device{s}, by owner</legend>
<ul class="ng-checklist">"#,
        hidden = token_and_back(request, &back),
        total = devices.len(),
        s = if devices.len() == 1 { "" } else { "s" },
    );
    for device in &devices {
        let owner = device
            .owner_name
            .as_deref()
            .map(|o| format!("{o}'s · "))
            .unwrap_or_default();
        // `listed` says which devices this page showed, so a device that
        // appeared after it was drawn is not muted for being unticked.
        html.push_str(&format!(
            r#"<li><input type="hidden" name="listed" value="{mac}"><label><input type="checkbox" name="device" value="{mac}"{checked}> <span class="ng-checklist__name">{label}</span> <span class="ng-checklist__meta">{owner}{meta}</span></label></li>"#,
            mac = escape_html(&device.mac),
            checked = if device.notify { " checked" } else { "" },
            label = escape_html(&device.label()),
            owner = escape_html(&owner),
            meta = escape_html(&device_meta(device)),
        ));
    }
    if (devices.len() as i64) >= BULK_LIMIT {
        html.push_str(&format!(
            "</ul><p class=\"ng-form__hint\">Only the first {BULK_LIMIT} are listed; the rest are left as they are.</p>"
        ));
    } else {
        html.push_str("</ul>");
    }
    html.push_str(&format!(
        r#"</fieldset>
<p><button type="submit">Save</button> <a href="{back}">Cancel</a></p>
</form>"#,
        back = escape_html(&back),
    ));
    themed("Only the devices I choose", &html)
}

pub(crate) fn alerts_devices_save(request: &ApiRequest) -> ApiResponse {
    let back = back_or(Some(&field(&request.body, "back")), ALERTS);
    let listed = fields(&request.body, "listed");
    if listed.is_empty() {
        return problem(&back, "No devices were listed. Nothing was changed.");
    }
    let ticked: std::collections::HashSet<String> = fields(&request.body, "device")
        .into_iter()
        .map(|m| netgrasp_core::assist::normalize_mac(&m))
        .collect();

    let mut changes: Vec<(DeviceFacts, bool)> = Vec::new();
    for raw in &listed {
        let facts = match resolve_device(raw) {
            Ok(facts) => facts,
            Err(message) => return problem(&back, &format!("{message}. Nothing was changed.")),
        };
        let want = ticked.contains(&netgrasp_core::assist::normalize_mac(&facts.mac));
        if facts.notify != want {
            changes.push((facts, want));
        }
    }
    if changes.is_empty() {
        return done(
            &back,
            "Nothing to change: those were already the devices that alert.",
        );
    }
    match assist_host::set_device_alerts(&changes) {
        Ok(()) => {
            let on = changes.iter().filter(|(_, to)| *to).count();
            let off = changes.len() - on;
            let mut parts = Vec::new();
            if on > 0 {
                parts.push(format!(
                    "{on} device{s} will now alert",
                    s = if on == 1 { "" } else { "s" }
                ));
            }
            if off > 0 {
                parts.push(format!(
                    "{off} device{s} muted",
                    s = if off == 1 { "" } else { "s" }
                ));
            }
            done(&back, &format!("{}.", parts.join("; ")))
        }
        Err(message) => problem(&back, &format!("{message}. Nothing was changed.")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const JAMIE: &str = "5eed0000-0000-4000-8000-000000000001";

    fn jamie(arrive: bool, depart: bool) -> PersonFacts {
        PersonFacts {
            item_id: JAMIE.into(),
            name: "Jamie".into(),
            notify_arrive: arrive,
            notify_depart: depart,
            ..PersonFacts::default()
        }
    }

    fn phone() -> DeviceFacts {
        DeviceFacts {
            id: 2,
            mac: "02:00:5e:00:00:02".into(),
            display_name: Some("Jamie's iPhone".into()),
            notify: true,
            owner_item_id: Some(JAMIE.into()),
            owner_name: Some("Jamie".into()),
            ..DeviceFacts::default()
        }
    }

    fn printer() -> DeviceFacts {
        DeviceFacts {
            id: 5,
            mac: "02:00:5e:00:00:05".into(),
            display_name: Some("Office printer".into()),
            notify: true,
            ..DeviceFacts::default()
        }
    }

    fn view() -> AlertsView {
        AlertsView {
            today: "2026-11-17".into(),
            status: Some(NotifyStatus {
                notify_configured: true,
                notify_backend: Some("ntfy".into()),
                last_delivery_ok: Some(true),
                last_delivery_at: Some(1),
                last_delivery_day: Some("2026-11-17".into()),
                last_delivery_clock: Some("18:42".into()),
                updated_at: Some(1),
            }),
            test: None,
            people: vec![jamie(true, false)],
            alerting: vec![phone(), printer()],
            counts: AlertingCounts {
                alerting: 2,
                devices: 3,
            },
            recent: vec![SentAlert {
                id: 1,
                event_type: "person_arrived".into(),
                day: Some("2026-11-17".into()),
                clock: Some("18:42".into()),
                person_name: Some("Jamie".into()),
                device_name: Some("Jamie's iPhone".into()),
                ..SentAlert::default()
            }],
        }
    }

    #[test]
    fn the_page_says_every_rule_as_a_sentence_grouped_by_person_then_device() {
        let html = render(&view(), "tok");
        let jamie_at = html.find("Tell me when Jamie gets home.").expect(&html);
        let phone_at = html
            .find("Tell me when Jamie&#x27;s iPhone comes online.")
            .expect(&html);
        let printer_at = html
            .find("Tell me when Office printer comes online.")
            .expect(&html);
        assert!(jamie_at < phone_at && phone_at < printer_at, "{html}");
        assert!(!html.contains("Tell me when Jamie leaves."), "{html}");
        assert!(html.contains("2 devices of 3 alert when they come back online."));
    }

    #[test]
    fn each_rule_is_switched_off_by_the_existing_save_it_belongs_to() {
        let html = render(&view(), "tok");
        // The person's arrival switch posts to the person notify save with the
        // departure flag carried as it is (off), so it posts no flag at all.
        let person_form = html
            .split("<form")
            .find(|f| f.contains(PERSON_NOTIFY) && f.contains("Turn off"))
            .expect(&html);
        assert!(person_form.contains(&format!("value=\"{JAMIE}\"")));
        assert!(!person_form.contains("name=\"arrive\""), "{person_form}");
        assert!(!person_form.contains("name=\"depart\""), "{person_form}");
        assert!(person_form.contains("name=\"back\" value=\"/alerts\""));
        // The device's switch posts value 0 to the device notify save.
        let device_form = html
            .split("<form")
            .find(|f| f.contains(DEVICE_NOTIFY) && f.contains("02:00:5e:00:00:05"))
            .expect(&html);
        assert!(device_form.contains("name=\"value\" value=\"0\""));
        // Every form carries the token.
        assert_eq!(
            html.matches("<form").count(),
            html.matches("name=\"_token\" value=\"tok\"").count()
        );
    }

    #[test]
    fn turning_one_person_rule_off_keeps_the_other() {
        let mut v = view();
        v.people = vec![jamie(true, true)];
        let html = render(&v, "tok");
        let arrive_switch = html
            .split("<li class=\"ng-alerts__rule\">")
            .find(|li| li.starts_with("<span>Tell me when Jamie gets home."))
            .expect(&html);
        assert!(arrive_switch.contains("name=\"depart\" value=\"1\""));
        assert!(!arrive_switch.contains("name=\"arrive\""));
    }

    /// An untracked person is listed as not tracked, never as away, and the
    /// page links to the switch; a tracked one carries no such line.
    #[test]
    fn an_untracked_person_is_not_tracked_on_the_alerts_page() {
        let mut v = view();
        v.people = vec![jamie(true, false)];
        let html = render(&v, "tok");
        assert!(
            html.contains("Not tracked: nobody is told when Jamie arrives"),
            "{html}"
        );
        assert!(html.contains("/netgrasp/person/presence?target="), "{html}");
        assert!(!html.contains("Away"), "{html}");

        let mut home = jamie(true, false);
        home.state = Some("home".into());
        v.people = vec![home];
        let html = render(&v, "tok");
        assert!(!html.contains("Not tracked"), "{html}");
    }

    #[test]
    fn delivery_status_test_button_and_recent_sends_are_on_the_page() {
        let html = render(&view(), "tok");
        assert!(html.contains("Alerts go to ntfy."), "{html}");
        assert!(html.contains("Last alert delivered today at 18:42."));
        assert!(html.contains("Send a test alert"));
        assert!(html.contains("Jamie is home: Jamie&#x27;s iPhone joined at 18:42"));
    }

    #[test]
    fn with_nowhere_to_send_there_is_no_test_button_and_the_page_says_why() {
        let mut v = view();
        v.status = Some(NotifyStatus::default());
        let html = render(&v, "tok");
        assert!(!html.contains("Send a test alert"), "{html}");
        assert!(html.contains("[notify.ntfy]"), "{html}");
        v.status = None;
        let html = render(&v, "tok");
        assert!(!html.contains("Send a test alert"), "{html}");
        assert!(html.contains("netgraspd has not said"), "{html}");
    }

    #[test]
    fn a_name_read_off_the_network_is_escaped_on_the_page() {
        let mut v = view();
        v.alerting[1].display_name = Some("<script>x</script>".into());
        let html = render(&v, "tok");
        assert!(!html.contains("<script>x"), "{html}");
    }
}
