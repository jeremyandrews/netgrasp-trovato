//! The plain forms behind the row menu: rename, owner, hidden, alerts.
//!
//! Four things a person does to a device and two they do to a person, each one
//! a page with a single field on it, served by `tap_api` and posted back with no
//! JavaScript anywhere in the path.
//!
//! # Why these are pages and not fields in the menu
//!
//! The menu that links here is drawn by a gather content template, and **a
//! gather template cannot render a form token.** The gather route renders it in
//! a context holding only that query's own data, and the site context carrying
//! `csrf_token` is built afterwards for the page wrapper
//! (`crates/kernel/src/routes/gather.rs`, `render_gather_with_theme`;
//! `routes/helpers.rs`, `inject_site_context`). The kernel refuses a
//! state-changing plugin request with no token *before* dispatching
//! (`routes/plugin_api.rs`), so a form posting straight from a row would 403 and
//! this module would never be called.
//!
//! `ApiRequest::csrf_token` is the one place a plugin can get a valid token, so
//! the write starts here instead: a `GET` renders the field with the token in
//! it, and the `POST` from that page is what changes anything. It is the shape
//! `plugins/trovato_contact` uses in the kernel tree, for the same reason.
//! `FRICTION.md`, `G-GATHER-TEMPLATE-NO-CSRF`.
//!
//! # Every write goes through the assistant's own path
//!
//! Not a second write path beside it — [`assist_host::apply_device_edit`] and
//! [`assist_host::apply_person_save`], the same functions the assistant's tools
//! call. That is the whole point of the module: a device renamed from a menu and
//! a device renamed in a conversation mint the Item the same way, coerce the MAC
//! the same way, write the same columns and leave `sync_state` alone the same
//! way, because they are one function called twice. A form that built its own
//! `UPDATE` would be a second answer to "what does renaming a device mean", and
//! the two would drift on the first change to either.
//!
//! In particular the edit is **sparse**: a rename names `display_name` and
//! nothing else, so `writeback::build_partial_update` writes one column. The
//! defect that discipline exists for — a rename turning a device's alerts off —
//! is `CHANGELOG.md` and `DESIGN.md` Decision 9, and it would be reintroduced by
//! any form here that posted a whole overlay.
//!
//! # The permission is checked twice, and the two checks disagree
//!
//! The kernel gates the route on the menu entry's `permission`, with an
//! `administer site` bypass (`routes/plugin_api.rs`). Every handler below checks
//! `administer netgrasp` again through the host, which has **no** such bypass
//! (`G-USER-API-NO-ADMIN-BYPASS`). The assistant's tools have exactly this
//! property and for the same reason, and the answer is the same: a site that
//! wants somebody using this grants the permission for real.
//!
//! # What this cannot do: come back with a redirect
//!
//! `ApiResponse` carries `status`, `body`, `content_type`, `theme` and `title`
//! and **no headers**, and the kernel builds the response from the status and
//! the body alone, setting only `Content-Type`. There is therefore no way to
//! return a `303` to the listing, which is what a form like this should do. Each
//! handler answers with a themed confirmation naming what changed, a link back,
//! and a `<meta http-equiv="refresh">` that returns on its own after a moment —
//! the closest thing to a redirect that needs no JavaScript.
//! `FRICTION.md`, `G-API-RESPONSE-NO-HEADERS`.

use netgrasp_core::assist::{self, DeviceFacts, PersonFacts};
use netgrasp_core::model::DeviceEdit;
use trovato_sdk::types::{ApiRequest, ApiResponse, MenuRoute};

use crate::{PERM_ADMINISTER, assist_host};

/// Where each form lives. One path per action, two methods each.
const DEVICE_RENAME: &str = "/netgrasp/device/rename";
const DEVICE_OWNER: &str = "/netgrasp/device/owner";
const DEVICE_HIDDEN: &str = "/netgrasp/device/hidden";
const DEVICE_NOTIFY: &str = "/netgrasp/device/notify";
const PERSON_RENAME: &str = "/netgrasp/person/rename";
const PERSON_NOTIFY: &str = "/netgrasp/person/notify";

/// The listing a form returns to when it was reached without one, or with one
/// that is not a local path.
const DEFAULT_BACK: &str = "/devices";

/// Longest name accepted, matching the Item title column the value lands in.
const MAX_NAME: usize = 255;

/// How many people the owner form will draw as a select.
///
/// Above this the page says so and sends the reader to the assistant instead,
/// which can be told "give this to Jamie" and resolve the name — a select of
/// two hundred people is not a control anybody can use, and a long page of
/// radio buttons is not better. The menu entry itself cannot make this call:
/// it is drawn by a gather template, which has no people list in its context to
/// count (`G-GATHER-TEMPLATE-NO-CSRF`, same context, second consequence).
const OWNER_SELECT_LIMIT: usize = 25;

/// The routes, for `tap_menu` to return alongside the navigation.
///
/// `MenuRoute::api` is invisible and public by default; every one of these is
/// gated on `administer netgrasp`, because every one of them writes — including
/// the `GET`s, which do not write but do disclose a device's name, owner and
/// flags to whoever opens them.
pub fn routes() -> Vec<MenuRoute> {
    let mut routes = Vec::new();
    for (path, stem) in [
        (DEVICE_RENAME, "device_rename"),
        (DEVICE_OWNER, "device_owner"),
        (DEVICE_HIDDEN, "device_hidden"),
        (DEVICE_NOTIFY, "device_notify"),
        (PERSON_RENAME, "person_rename"),
        (PERSON_NOTIFY, "person_notify"),
    ] {
        routes
            .push(MenuRoute::api("GET", path, format!("{stem}_form")).permission(PERM_ADMINISTER));
        routes
            .push(MenuRoute::api("POST", path, format!("{stem}_save")).permission(PERM_ADMINISTER));
    }
    routes
}

/// Serve one request.
///
/// Dispatches on the callback rather than the path, which is what
/// `ApiRequest::callback` is for: the path is matched by the kernel and the
/// callback is the name this plugin gave the handler.
pub fn serve(request: &ApiRequest) -> ApiResponse {
    // The belt over the kernel's braces, and it is not the same check: the
    // host's `current-user-has-permission` has no `administer site` bypass.
    if !assist_host::may_administer() {
        return refuse(403, "You do not have permission to change Netgrasp.");
    }

    match request.callback.as_str() {
        "device_rename_form" => device_rename_form(request),
        "device_rename_save" => device_rename_save(request),
        "device_owner_form" => device_owner_form(request),
        "device_owner_save" => device_owner_save(request),
        "device_hidden_form" => device_flag_form(request, Flag::Hidden),
        "device_hidden_save" => device_flag_save(request, Flag::Hidden),
        "device_notify_form" => device_flag_form(request, Flag::Notify),
        "device_notify_save" => device_flag_save(request, Flag::Notify),
        "person_rename_form" => person_rename_form(request),
        "person_rename_save" => person_rename_save(request),
        "person_notify_form" => person_notify_form(request),
        "person_notify_save" => person_notify_save(request),
        other => refuse(404, &format!("no such form: {other}")),
    }
}

/// Which of a device's two flags a request is about.
///
/// One pair of handlers for both, because they differ only in a column name and
/// three strings. Two copies would be two places to forget that an edit must
/// name one column and not both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flag {
    Hidden,
    Notify,
}

impl Flag {
    fn path(self) -> &'static str {
        match self {
            Flag::Hidden => DEVICE_HIDDEN,
            Flag::Notify => DEVICE_NOTIFY,
        }
    }

    /// Whether the flag is currently set, read off the device row.
    fn of(self, facts: &DeviceFacts) -> bool {
        match self {
            Flag::Hidden => facts.hidden,
            Flag::Notify => facts.notify,
        }
    }

    /// The edit that sets it, naming this column and no other.
    fn edit(self, to: bool) -> DeviceEdit {
        match self {
            Flag::Hidden => DeviceEdit {
                hidden: Some(to),
                ..DeviceEdit::default()
            },
            Flag::Notify => DeviceEdit {
                notify: Some(to),
                ..DeviceEdit::default()
            },
        }
    }

    /// The page title, the question, and the button, for the direction this is
    /// about to go.
    fn wording(self, currently: bool) -> (&'static str, &'static str, &'static str) {
        match (self, currently) {
            (Flag::Hidden, true) => (
                "Unhide device",
                "This device is hidden from the device lists.",
                "Unhide it",
            ),
            (Flag::Hidden, false) => (
                "Hide device",
                "This device appears in the device lists.",
                "Hide it",
            ),
            (Flag::Notify, true) => (
                "Mute alerts",
                "This device raises an alert when it arrives and when it leaves.",
                "Mute its alerts",
            ),
            (Flag::Notify, false) => (
                "Unmute alerts",
                "This device raises no arrival or departure alerts.",
                "Unmute its alerts",
            ),
        }
    }

    fn done(self, to: bool) -> &'static str {
        match (self, to) {
            (Flag::Hidden, true) => "is now hidden from the device lists",
            (Flag::Hidden, false) => "now appears in the device lists",
            (Flag::Notify, true) => "now raises arrival and departure alerts",
            (Flag::Notify, false) => "is now muted",
        }
    }
}

// ===========================================================================
// Device: rename
// ===========================================================================

fn device_rename_form(request: &ApiRequest) -> ApiResponse {
    let (target, back) = target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };

    let field = format!(
        r#"<p><label for="ng-value">Name</label>
<input type="text" id="ng-value" name="value" value="{name}" maxlength="{max}" autofocus></p>"#,
        name = escape_html(&facts.label()),
        max = MAX_NAME,
    );
    form_page(
        request,
        DEVICE_RENAME,
        &target,
        &back,
        "Rename device",
        &device_subject(&facts),
        &field,
        "Rename",
    )
}

fn device_rename_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };
    let name = field(&request.body, "value");

    // A blank name is not a rename to nothing; it is a rename nobody meant. The
    // way to take a typed name off a device is the assistant, which can say so
    // in a sentence, or the Item's own form.
    if name.trim().is_empty() {
        return problem(&back, "A device needs a name. Nothing was changed.");
    }
    if name.chars().count() > MAX_NAME {
        return problem(
            &back,
            &format!("That name is longer than {MAX_NAME} characters. Nothing was changed."),
        );
    }

    let edit = DeviceEdit {
        display_name: Some(name.trim().to_string()),
        ..DeviceEdit::default()
    };
    match assist_host::apply_device_edit(&facts, &edit) {
        Ok(_) => done(
            &back,
            &format!(
                "{subject} is now called <strong>{name}</strong>.",
                subject = escape_html(&facts.phrase()),
                name = escape_html(name.trim()),
            ),
        ),
        Err(message) => problem(&back, &message),
    }
}

// ===========================================================================
// Device: owner
// ===========================================================================

fn device_owner_form(request: &ApiRequest) -> ApiResponse {
    let (target, back) = target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };
    let people = match assist_host::load_people() {
        Ok(people) => people,
        Err(message) => return problem(&back, &message),
    };

    // Too many to choose from. The assistant can be told a name; a select of
    // hundreds cannot be read.
    if people.len() > OWNER_SELECT_LIMIT {
        let body = format!(
            "<p>{subject}</p>\
             <p>There are {count} people on this network, which is more than this \
             page can usefully list. Ask the assistant to assign it instead — it \
             takes a name.</p>\
             <p><a class=\"button\" href=\"{assistant}\">Ask the assistant</a> \
             <a href=\"{back}\">Back</a></p>",
            subject = device_subject(&facts),
            count = people.len(),
            assistant = escape_html(&assistant_href(&facts)),
            back = escape_html(&back),
        );
        return themed("Assign owner", &body);
    }

    let mut options = String::from(r#"<option value="">nobody</option>"#);
    for person in &people {
        let selected = facts
            .owner_item_id
            .as_deref()
            .is_some_and(|id| id == person.item_id);
        options.push_str(&format!(
            r#"<option value="{id}"{sel}>{name}</option>"#,
            id = escape_html(&person.item_id),
            sel = if selected { " selected" } else { "" },
            name = escape_html(&person.name),
        ));
    }

    let field = format!(
        r#"<p><label for="ng-value">Owner</label>
<select id="ng-value" name="value">{options}</select></p>"#
    );
    form_page(
        request,
        DEVICE_OWNER,
        &target,
        &back,
        "Assign owner",
        &device_subject(&facts),
        &field,
        "Assign",
    )
}

fn device_owner_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };
    let chosen = field(&request.body, "value");
    let chosen = chosen.trim();

    // An empty value is "nobody", which is a real instruction and not a missing
    // one — `Some(None)` unassigns, the outer `None` would leave the column
    // alone. The two are different statements and this is where they part.
    let (owner, named) = if chosen.is_empty() {
        (None, "nobody".to_string())
    } else {
        let people = match assist_host::load_people() {
            Ok(people) => people,
            Err(message) => return problem(&back, &message),
        };
        // Resolved against the people who exist rather than trusted: the value
        // came off a form, `owner_item_id` is a uuid column, and an id that
        // matches nobody would either fail the write with a cast error or store
        // a dangling reference the device tables would have to render as a chip.
        match people.iter().find(|p| p.item_id == chosen) {
            Some(person) => (Some(person.item_id.clone()), person.name.clone()),
            None => {
                return problem(&back, "That person no longer exists. Nothing was changed.");
            }
        }
    };

    let edit = DeviceEdit {
        owner_item_id: Some(owner),
        ..DeviceEdit::default()
    };
    match assist_host::apply_device_edit(&facts, &edit) {
        Ok(_) => done(
            &back,
            &format!(
                "{subject} now belongs to <strong>{named}</strong>.",
                subject = escape_html(&facts.phrase()),
                named = escape_html(&named),
            ),
        ),
        Err(message) => problem(&back, &message),
    }
}

// ===========================================================================
// Device: the two flags
// ===========================================================================

fn device_flag_form(request: &ApiRequest, flag: Flag) -> ApiResponse {
    let (target, back) = target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };

    let currently = flag.of(&facts);
    let (title, says, button) = flag.wording(currently);
    // The single field is the value being moved to, so the page that asked and
    // the request that arrives cannot disagree about which way it was going —
    // two people on two tabs both clicking "Hide" write `true` twice rather
    // than toggling it back and forth.
    let field = format!(
        r#"<p>{says}</p><input type="hidden" name="value" value="{to}">"#,
        to = if currently { "0" } else { "1" },
    );
    form_page(
        request,
        flag.path(),
        &target,
        &back,
        title,
        &device_subject(&facts),
        &field,
        button,
    )
}

fn device_flag_save(request: &ApiRequest, flag: Flag) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let facts = match resolve_device(&target) {
        Ok(facts) => facts,
        Err(message) => return problem(&back, &message),
    };
    let to = field(&request.body, "value") == "1";

    match assist_host::apply_device_edit(&facts, &flag.edit(to)) {
        Ok(_) => done(
            &back,
            &format!(
                "{subject} {what}.",
                subject = escape_html(&facts.phrase()),
                what = flag.done(to),
            ),
        ),
        Err(message) => problem(&back, &message),
    }
}

// ===========================================================================
// Person: rename and alerts
// ===========================================================================

fn person_rename_form(request: &ApiRequest) -> ApiResponse {
    let (target, back) = target_and_back(request);
    let person = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };

    let field = format!(
        r#"<p><label for="ng-value">Name</label>
<input type="text" id="ng-value" name="value" value="{name}" maxlength="{max}" autofocus></p>"#,
        name = escape_html(&person.name),
        max = MAX_NAME,
    );
    form_page(
        request,
        PERSON_RENAME,
        &target,
        &back,
        "Rename person",
        &person_subject(&person),
        &field,
        "Rename",
    )
}

fn person_rename_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let name = field(&request.body, "value");
    if name.trim().is_empty() {
        return problem(&back, "A person needs a name. Nothing was changed.");
    }
    if name.chars().count() > MAX_NAME {
        return problem(
            &back,
            &format!("That name is longer than {MAX_NAME} characters. Nothing was changed."),
        );
    }

    let existing = match assist_host::load_person_item(&target) {
        Ok(item) => item,
        Err(message) => return problem(&back, &message),
    };
    // Every other field carried forward: `Item::update` replaces `fields`
    // wholesale, so a payload naming only the title deletes the notes and both
    // notification flags.
    let payload = assist_host::person_payload(&target, &existing, Some(name.trim()), None, None);
    match assist_host::apply_person_save(&payload) {
        Ok(_) => done(
            &back,
            &format!(
                "That person is now called <strong>{name}</strong>.",
                name = escape_html(name.trim()),
            ),
        ),
        Err(message) => problem(&back, &message),
    }
}

fn person_notify_form(request: &ApiRequest) -> ApiResponse {
    let (target, back) = target_and_back(request);
    let person = match assist_host::load_person(&target) {
        Ok(person) => person,
        Err(message) => return problem(&back, &message),
    };

    // Two checkboxes, which is one field's worth of decision: when they arrive,
    // when they leave. An unticked checkbox posts nothing at all, which is what
    // the save below reads them with.
    let field = format!(
        r#"<p><label><input type="checkbox" name="arrive" value="1"{arrive}> Tell me when they arrive</label></p>
<p><label><input type="checkbox" name="depart" value="1"{depart}> Tell me when they leave</label></p>"#,
        arrive = if person.notify_arrive { " checked" } else { "" },
        depart = if person.notify_depart { " checked" } else { "" },
    );
    form_page(
        request,
        PERSON_NOTIFY,
        &target,
        &back,
        "Arrival alerts",
        &person_subject(&person),
        &field,
        "Save",
    )
}

fn person_notify_save(request: &ApiRequest) -> ApiResponse {
    let (target, back) = posted_target_and_back(request);
    let arrive = field(&request.body, "arrive") == "1";
    let depart = field(&request.body, "depart") == "1";

    let existing = match assist_host::load_person_item(&target) {
        Ok(item) => item,
        Err(message) => return problem(&back, &message),
    };
    let payload =
        assist_host::person_payload(&target, &existing, None, None, Some((arrive, depart)));
    match assist_host::apply_person_save(&payload) {
        Ok(_) => done(&back, &alerts_sentence(arrive, depart)),
        Err(message) => problem(&back, &message),
    }
}

fn alerts_sentence(arrive: bool, depart: bool) -> String {
    match (arrive, depart) {
        (true, true) => "They will be announced when they arrive and when they leave.".into(),
        (true, false) => "They will be announced when they arrive.".into(),
        (false, true) => "They will be announced when they leave.".into(),
        (false, false) => "They will not be announced.".into(),
    }
}

// ===========================================================================
// The shared shell
// ===========================================================================

/// One device, however the menu named it.
///
/// The reference is a MAC or an `ng_devices.id`, parsed by the same function the
/// assistant's tools parse theirs with — which is what lets an event row, whose
/// only handle on a device is `device_id`, reach the same forms a device row
/// does.
fn resolve_device(target: &str) -> Result<DeviceFacts, String> {
    let reference = assist::parse_device_ref(target)?;
    assist_host::load_device(&reference)
}

/// The device named the way the assistant's proposal cards name one: the label
/// somebody reads plus the MAC that identifies it.
fn device_subject(facts: &DeviceFacts) -> String {
    escape_html(&facts.descriptive())
}

fn person_subject(person: &PersonFacts) -> String {
    escape_html(&person.name)
}

/// Where a conversation about this device would go, for the owner form's
/// overflow branch.
fn assistant_href(facts: &DeviceFacts) -> String {
    match facts.trovato_item_id.as_deref().filter(|id| !id.is_empty()) {
        Some(item) => format!("/ai/assistant/netgrasp_device/{item}"),
        None => format!("/ai/assistant/netgrasp_network?device={}", facts.mac),
    }
}

/// The `target` and `back` of a `GET`, out of the query string.
fn target_and_back(request: &ApiRequest) -> (String, String) {
    (
        request.query.get("target").cloned().unwrap_or_default(),
        safe_back(request.query.get("back").map(String::as_str)),
    )
}

/// The same two out of a posted body.
///
/// They are re-posted as hidden fields rather than kept in the query string,
/// because the form's `action` is the bare path: a value that has been through
/// a text field and back is the one the browser sends, and there is then one
/// place a handler reads them from.
fn posted_target_and_back(request: &ApiRequest) -> (String, String) {
    let body = &request.body;
    (
        field(body, "target"),
        safe_back(Some(field(body, "back")).as_deref()),
    )
}

/// A return path that is a path on this site, and not anywhere else.
///
/// A `back` parameter is an open redirect waiting to happen: it arrives in a URL
/// somebody may have been handed, and it ends up in an `href` and in a `<meta
/// refresh>`. Only an absolute path is accepted — one leading slash and no
/// second, which rules out `//evil.example` (a protocol-relative URL a browser
/// reads as another origin) — and anything else falls back to the device list
/// rather than being repaired.
fn safe_back(raw: Option<&str>) -> String {
    let candidate = raw.unwrap_or("").trim();
    let plausible = candidate.starts_with('/')
        && !candidate.starts_with("//")
        && !candidate.contains(['\\', '\r', '\n', '"', '\''])
        && candidate.len() <= 512;
    if plausible {
        candidate.to_string()
    } else {
        DEFAULT_BACK.to_string()
    }
}

/// A themed page, carrying netgrasp's stylesheet.
///
/// The link is in the body rather than in `<head>`, which is where a stylesheet
/// belongs and is not somewhere a plugin can reach: `ApiResponse` carries a body
/// and a title, and the kernel renders that body as page content. Browsers honour
/// a stylesheet link in the body, and `templates/gather/netgrasp/page.html` does
/// the same thing for the same reason — a gather content template cannot reach
/// `<head>` either.
///
/// Without it these pages arrive with the site's chrome and none of netgrasp's
/// own styles, because `netgrasp.css` is loaded by the gather chrome and a
/// plugin-served page is not a gather.
fn themed(title: &str, body: &str) -> ApiResponse {
    ApiResponse::themed(title, with_stylesheet(body))
}

fn themed_with_status(status: u16, title: &str, body: &str) -> ApiResponse {
    ApiResponse::themed_with_status(status, title, with_stylesheet(body))
}

fn with_stylesheet(body: &str) -> String {
    format!(
        "<link rel=\"stylesheet\" href=\"/static/css/netgrasp.css\">\n\
         <div class=\"ng-page ng-page--form\">{body}</div>"
    )
}

/// The form page: one field, the kernel's token, and the way back.
///
/// `action` is the path this was served from, so the `GET` and the `POST` are
/// one URL and the form needs no `action` cleverness. The token is the one the
/// kernel minted for *this* request — single-use, so a page rendered once and
/// submitted twice is refused the second time, which is the behaviour every
/// kernel form has.
#[allow(clippy::too_many_arguments)] // One page; each argument is a distinct part of it.
fn form_page(
    request: &ApiRequest,
    action: &str,
    target: &str,
    back: &str,
    title: &str,
    subject: &str,
    field_html: &str,
    submit: &str,
) -> ApiResponse {
    let body = format!(
        r#"<form method="post" action="{action}" class="ng-form">
<input type="hidden" name="_token" value="{token}">
<input type="hidden" name="target" value="{target}">
<input type="hidden" name="back" value="{back}">
<p class="ng-form__subject">{subject}</p>
{field_html}
<p><button type="submit">{submit}</button> <a href="{back}">Cancel</a></p>
</form>"#,
        action = escape_html(action),
        token = escape_html(&request.csrf_token),
        target = escape_html(target),
        back = escape_html(back),
        submit = escape_html(submit),
    );
    themed(title, &body)
}

/// It worked: say what changed and go back.
///
/// The `<meta refresh>` is the redirect this cannot send. `ApiResponse` has no
/// headers and the kernel sets only `Content-Type`, so a `303` to the listing is
/// not expressible; two seconds is long enough to read one sentence and short
/// enough not to feel stuck, and the link beside it works whether or not the
/// browser honours the meta. `FRICTION.md`, `G-API-RESPONSE-NO-HEADERS`.
fn done(back: &str, message: &str) -> ApiResponse {
    let body = format!(
        r#"<meta http-equiv="refresh" content="2; url={back}">
<p class="ng-form__done">{message}</p>
<p><a href="{back}">Back to the list</a></p>"#,
        back = escape_html(back),
    );
    themed("Done", &body)
}

/// It did not work, and nothing was changed.
///
/// 422 rather than 200: the request was understood and not acted on. No meta
/// refresh — a page that reports a failure and then leaves on its own is a page
/// nobody gets to read.
fn problem(back: &str, message: &str) -> ApiResponse {
    let body = format!(
        r#"<p class="ng-form__problem">{message}</p>
<p><a href="{back}">Back to the list</a></p>"#,
        message = escape_html(message),
        back = escape_html(back),
    );
    themed_with_status(422, "Nothing changed", &body)
}

/// A refusal, as JSON, because it is not a page anybody navigated to on purpose.
fn refuse(status: u16, message: &str) -> ApiResponse {
    ApiResponse::error(status, message)
}

/// Read one field out of a URL-encoded body, first occurrence winning.
///
/// Hand-rolled because the SDK ships no form decoding, the same way it ships no
/// HTML escaping (`G-SDK-NO-ESCAPE`). This is the third copy of this function in
/// the tree, after `trovato_contact` and `argus`.
fn field(body: &str, name: &str) -> String {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| percent_decode(&value.replace('+', " ")))
        .unwrap_or_default()
}

/// Percent-decode a form value, leaving an invalid escape as written.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape text for an HTML body or a double-quoted attribute.
///
/// The kernel does not sanitize a plugin's response body — the contract every
/// view tap has — so the plugin does. Both quote forms are covered because these
/// values land in attributes as well as in text, and a device's name is a string
/// the daemon read off the network.
fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_route_is_gated_on_the_administer_permission() {
        let routes = routes();
        assert_eq!(routes.len(), 12, "six forms, two methods each");
        for route in &routes {
            assert_eq!(
                route.permission, PERM_ADMINISTER,
                "{} is not gated",
                route.path
            );
            assert_eq!(route.handler_type, "api", "{} is not routed", route.path);
            assert!(
                !route.callback.is_empty(),
                "{} names no handler, so the kernel will not dispatch it",
                route.path
            );
            assert!(
                !route.visible,
                "{} is a form, not a navigation entry",
                route.path
            );
        }
    }

    /// Every path is served by both methods, and the two name different
    /// handlers. One method missing is a form that renders and cannot post, or
    /// posts and cannot render.
    #[test]
    fn every_form_answers_both_methods_with_its_own_handler() {
        let routes = routes();
        for path in [
            DEVICE_RENAME,
            DEVICE_OWNER,
            DEVICE_HIDDEN,
            DEVICE_NOTIFY,
            PERSON_RENAME,
            PERSON_NOTIFY,
        ] {
            let mine: Vec<_> = routes.iter().filter(|r| r.path == path).collect();
            assert_eq!(mine.len(), 2, "{path} is not served twice");
            let mut methods: Vec<&str> = mine.iter().map(|r| r.method.as_str()).collect();
            methods.sort_unstable();
            assert_eq!(methods, ["GET", "POST"], "{path} methods");
            assert_ne!(
                mine[0].callback, mine[1].callback,
                "{path} uses one handler for both methods"
            );
        }
    }

    /// Every callback the routes name is a callback `serve` answers. A route
    /// naming a handler the dispatch does not have is a 404 on a path the menu
    /// links to.
    #[test]
    fn every_declared_callback_is_dispatched() {
        for route in routes() {
            let request = ApiRequest::new(route.callback.clone(), "GET", route.path, "", true);
            // Dispatch reaches the permission check first, which in a native
            // test build answers false — so what this asserts is that the
            // callback is not the *unknown* branch.
            let response = serve(&request);
            assert_ne!(
                response.status, 404,
                "{} names a callback serve does not answer",
                route.callback
            );
        }
    }

    #[test]
    fn a_back_path_that_leaves_the_site_is_refused() {
        // The cases that matter: another origin, a protocol-relative URL the
        // browser also reads as another origin, a scheme, and a backslash a
        // browser may normalise to a slash.
        for hostile in [
            "https://evil.example/",
            "//evil.example/",
            "javascript:alert(1)",
            "/\\evil.example",
            "",
            "devices",
        ] {
            assert_eq!(
                safe_back(Some(hostile)),
                DEFAULT_BACK,
                "{hostile} was accepted as a return path"
            );
        }
        assert_eq!(safe_back(Some("/devices/online")), "/devices/online");
        assert_eq!(safe_back(Some("/events?page=2")), "/events?page=2");
        assert_eq!(safe_back(None), DEFAULT_BACK);
    }

    #[test]
    fn a_form_value_survives_url_encoding() {
        let body =
            "_token=abc&target=02%3A00%3A5e%3A00%3A00%3A04&value=Office+printer&back=%2Fdevices";
        assert_eq!(field(body, "value"), "Office printer");
        assert_eq!(field(body, "target"), "02:00:5e:00:00:04");
        assert_eq!(field(body, "back"), "/devices");
        assert_eq!(field(body, "missing"), "");
    }

    /// An unticked checkbox posts nothing, which has to read as false rather
    /// than as "leave it alone" — the two flags are saved together, so an
    /// absent one is a real instruction.
    #[test]
    fn an_unticked_checkbox_reads_as_off() {
        assert_eq!(field("arrive=1", "depart"), "");
        assert!(field("arrive=1", "arrive") == "1");
    }

    #[test]
    fn a_name_a_daemon_read_off_the_wire_is_escaped() {
        let nasty = r#"<script>alert("x")</script>&'"#;
        let escaped = escape_html(nasty);
        assert!(!escaped.contains('<'), "{escaped}");
        assert!(!escaped.contains('>'), "{escaped}");
        assert!(!escaped.contains('"'), "{escaped}");
        assert!(!escaped.contains('\''), "{escaped}");
        assert!(escaped.contains("&amp;"), "{escaped}");
    }

    /// The flag pair is the same handler twice, so the wording has to come out
    /// of the flag rather than out of the caller.
    #[test]
    fn each_flag_names_only_its_own_column() {
        let hide = Flag::Hidden.edit(true);
        assert_eq!(hide.columns(), vec!["hidden"]);
        let mute = Flag::Notify.edit(false);
        assert_eq!(mute.columns(), vec!["notify"]);
    }

    #[test]
    fn the_flag_wording_follows_the_state_it_is_leaving() {
        assert_eq!(Flag::Hidden.wording(true).0, "Unhide device");
        assert_eq!(Flag::Hidden.wording(false).0, "Hide device");
        assert_eq!(Flag::Notify.wording(true).0, "Mute alerts");
        assert_eq!(Flag::Notify.wording(false).0, "Unmute alerts");
    }
}
