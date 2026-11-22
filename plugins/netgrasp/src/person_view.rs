//! The person page: `/item/<uuid>` for an `ng_person`.
//!
//! Where they are, which devices are theirs, their last comings and goings, and
//! their alerts in words, with the five things a person does to a person as
//! links: Rename, Alerts, Merge into…, Delete…, and Add a device. Each link
//! opens a `tap_api` form, for the reason `forms.rs` gives: this fragment is a
//! view, and a view cannot carry the kernel's form token.
//!
//! Rendering is pure ([`render`]) and the reads are the tap's, so the markup is
//! tested without a host.

use netgrasp_core::assist::{DeviceFacts, MovementFacts, PersonFacts, device_label};
use netgrasp_core::timeline;

use crate::device_view::escape;

/// How many comings and goings the page lists.
pub const MOVEMENT_ROWS: i64 = 20;

/// One person's presence, as `SELECT_PERSON_PRESENCE` decodes it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Presence {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub current_location: Option<String>,
    #[serde(default)]
    pub arrived: Option<i64>,
    #[serde(default)]
    pub departed: Option<i64>,
    #[serde(default)]
    pub devices_online: i64,
    /// The person's own switch (netgraspd V7).
    #[serde(default)]
    pub track_presence: Option<bool>,
    /// The install's switch: `None` when no daemon has published it.
    #[serde(default)]
    pub presence_tracking: Option<bool>,
}

/// Why somebody is not tracked, in one sentence, most fundamental reason first.
#[must_use]
pub fn untracked_reason(presence: &Presence) -> &'static str {
    if presence.presence_tracking == Some(false) {
        "Presence tracking is off for everybody: set presence_tracking = true in the daemon's netgrasp.toml."
    } else if presence.track_presence != Some(true) {
        "Tracking whether they are home is off for this person."
    } else {
        "None of their devices counts: give them a phone or a watch, or choose a device that counts."
    }
}

/// The page fragment.
#[must_use]
pub fn render(
    person: &PersonFacts,
    presence: &Presence,
    devices: &[DeviceFacts],
    movements: &[MovementFacts],
    now: i64,
) -> String {
    let id = escape(&person.item_id);
    let back = format!("/item/{}", person.item_id);
    let back = url_encode_path(&back);
    let mut out = String::with_capacity(4096);
    out.push_str("<link rel=\"stylesheet\" href=\"/static/css/netgrasp.css\">");
    out.push_str("<section class='ng-person-page'>");

    // Where they are. Three states, and only "away" reads as away: a person
    // nobody is tracking is neither in nor out.
    let state = presence.state.as_deref();
    out.push_str("<p class='ng-person-page__state'>");
    if state == Some("home") {
        out.push_str("<strong>Home</strong>");
        if let Some(t) = presence.arrived {
            out.push_str(&format!(
                " since {}",
                escape(&timeline::humanize_ago(Some(t), now))
            ));
        }
        if let Some(place) = present(presence.current_location.as_deref()) {
            out.push_str(&format!(", {}", escape(place)));
        }
        if presence.devices_online > 0 {
            out.push_str(&format!(
                " ({} device{} online)",
                presence.devices_online,
                if presence.devices_online == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
    } else if state == Some("away") {
        out.push_str("<strong>Away</strong>");
        if let Some(t) = presence.departed {
            out.push_str(&format!(
                ", left {}",
                escape(&timeline::humanize_ago(Some(t), now))
            ));
        }
    } else {
        out.push_str("<strong>Not tracked</strong>. ");
        out.push_str(&escape(untracked_reason(presence)));
    }
    out.push_str("</p>");

    // The actions.
    out.push_str("<p class='ng-person-page__actions'>");
    for (href, label) in [
        (
            format!("/netgrasp/person/rename?target={id}&amp;back={back}"),
            "Rename",
        ),
        (
            format!("/netgrasp/person/notify?target={id}&amp;back={back}"),
            "Alerts",
        ),
        (
            format!("/netgrasp/person/presence?target={id}&amp;back={back}"),
            "Presence",
        ),
        (
            format!("/netgrasp/person/merge?target={id}&amp;back={back}"),
            "Merge into…",
        ),
        (
            format!("/netgrasp/person/delete?target={id}&amp;back={back}"),
            "Delete…",
        ),
        (
            format!("/devices/assign?person={id}&amp;back={back}"),
            "Add a device",
        ),
    ] {
        out.push_str(&format!("<a class='button' href='{href}'>{label}</a> "));
    }
    out.push_str("</p>");

    // Alerts, in words.
    out.push_str("<p class='ng-person-page__alerts'>");
    out.push_str(&escape(&alerts_sentence(person)));
    out.push_str("</p>");

    // Their devices.
    out.push_str("<h2>Devices</h2>");
    if devices.is_empty() {
        out.push_str(&format!(
            "<p class='ng-empty'>No devices yet. A tracked person is home when a phone or watch of theirs is online. \
             <a href='/devices/assign?person={id}&amp;back={back}'>Give them a device</a>.</p>"
        ));
    } else {
        out.push_str("<ul class='ng-person-page__devices'>");
        for device in devices {
            let label = escape(&device.label());
            let name = match device.trovato_item_id.as_deref().filter(|i| !i.is_empty()) {
                Some(item) => format!("<a href='/item/{}'>{label}</a>", escape(item)),
                None => label,
            };
            let state = match device.state.as_deref() {
                Some("online") => "online",
                Some("offline") => "offline",
                _ => "unknown",
            };
            out.push_str(&format!(
                "<li>{name} <span class='ng-person-page__meta'>{mac} · {state} · last seen {seen}</span></li>",
                mac = escape(&device.mac),
                seen = escape(&timeline::humanize_ago(device.last_seen, now)),
            ));
        }
        out.push_str("</ul>");
    }

    // Comings and goings.
    out.push_str("<h2>Comings and goings</h2>");
    if movements.is_empty() {
        out.push_str("<p class='ng-empty'>None recorded yet.</p>");
    } else {
        out.push_str("<ul class='ng-person-page__movements'>");
        for m in movements {
            let what = match m.event_type.as_str() {
                "person_arrived" => "Arrived",
                "person_departed" => "Left",
                _ => "Seen",
            };
            let mut line = format!("{what} {}", escape(&timeline::humanize_ago(m.ts, now)));
            if let Some(place) = present(m.location.as_deref()) {
                line.push_str(&format!(", at {}", escape(place)));
            }
            if let Some(mac) = present(m.device_mac.as_deref()) {
                let label = device_label(
                    m.device_display_name.as_deref(),
                    m.device_resolved_name.as_deref(),
                    m.device_hostname.as_deref(),
                    None,
                    mac,
                );
                line.push_str(&format!(", on {}", escape(&label)));
            }
            out.push_str(&format!("<li>{line}</li>"));
        }
        out.push_str("</ul>");
    }
    out.push_str("<p><a href='/people/movements'>Everybody's comings and goings</a></p>");
    out.push_str("</section>");
    out
}

/// Their alerts, as a sentence.
fn alerts_sentence(person: &PersonFacts) -> String {
    let name = &person.name;
    match (person.notify_arrive, person.notify_depart) {
        (true, true) => {
            format!("Alerts: you are told when {name} gets home and when {name} leaves.")
        }
        (true, false) => format!("Alerts: you are told when {name} gets home."),
        (false, true) => format!("Alerts: you are told when {name} leaves."),
        (false, false) => format!("Alerts: off. You are not told when {name} comes or goes."),
    }
}

/// A value if it is present and not blank.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Encode a local path for a query value. Only what a `/item/<uuid>` path can
/// hold needs it: the slashes.
fn url_encode_path(path: &str) -> String {
    path.replace('/', "%2F")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const ID: &str = "5eed0000-0000-4000-8000-000000000001";

    fn jamie() -> PersonFacts {
        PersonFacts {
            item_id: ID.to_string(),
            name: "Jamie".to_string(),
            notify_arrive: true,
            ..PersonFacts::default()
        }
    }

    #[test]
    fn the_page_offers_every_action_and_says_the_alerts_in_words() {
        let html = render(&jamie(), &Presence::default(), &[], &[], 1_000);
        for action in [
            "/netgrasp/person/rename?target=",
            "/netgrasp/person/notify?target=",
            "/netgrasp/person/merge?target=",
            "/netgrasp/person/delete?target=",
            "/devices/assign?person=",
        ] {
            assert!(html.contains(action), "{action} missing: {html}");
        }
        assert!(
            html.contains("you are told when Jamie gets home."),
            "{html}"
        );
        assert!(html.contains("/netgrasp/person/presence?target="), "{html}");
        // No state at all is not away: nobody is tracking them.
        assert!(html.contains("<strong>Not tracked</strong>"), "{html}");
        assert!(!html.contains("Away"), "{html}");
        assert!(html.contains("No devices yet"), "{html}");
        assert!(html.contains("back=%2Fitem%2F5eed"), "{html}");
    }

    #[test]
    fn devices_and_movements_are_listed_and_escaped() {
        let devices = vec![DeviceFacts {
            mac: "02:00:5e:00:00:01".into(),
            display_name: Some("<b>Phone</b>".into()),
            state: Some("online".into()),
            trovato_item_id: Some("abc".into()),
            ..DeviceFacts::default()
        }];
        let movements = vec![MovementFacts {
            event_type: "person_arrived".into(),
            ts: Some(900),
            location: Some("Kitchen".into()),
            device_mac: Some("02:00:5e:00:00:01".into()),
            ..MovementFacts::default()
        }];
        let presence = Presence {
            state: Some("home".into()),
            arrived: Some(900),
            devices_online: 1,
            ..Presence::default()
        };
        let html = render(&jamie(), &presence, &devices, &movements, 1_000);
        assert!(
            html.contains("&lt;b&gt;Phone&lt;&#x2F;b&gt;") || html.contains("&lt;b&gt;Phone"),
            "{html}"
        );
        assert!(!html.contains("<b>Phone"), "{html}");
        assert!(html.contains("<a href='/item/abc'>"), "{html}");
        assert!(html.contains("Arrived"), "{html}");
        assert!(html.contains("at Kitchen"), "{html}");
        assert!(html.contains("<strong>Home</strong>"), "{html}");
        assert!(html.contains("(1 device online)"), "{html}");
    }

    #[test]
    fn an_untracked_person_is_not_tracked_never_away_and_the_page_says_why() {
        let base = Presence {
            state: Some("untracked".into()),
            departed: Some(500),
            ..Presence::default()
        };
        let off_everywhere = Presence {
            presence_tracking: Some(false),
            track_presence: Some(true),
            ..base.clone()
        };
        let html = render(&jamie(), &off_everywhere, &[], &[], 1_000);
        assert!(html.contains("<strong>Not tracked</strong>"), "{html}");
        assert!(!html.contains("Away"), "{html}");
        assert!(!html.contains("left "), "{html}");
        assert!(html.contains("presence_tracking = true"), "{html}");

        let off_for_them = Presence {
            presence_tracking: Some(true),
            track_presence: Some(false),
            ..base.clone()
        };
        let html = render(&jamie(), &off_for_them, &[], &[], 1_000);
        assert!(html.contains("off for this person"), "{html}");

        let nothing_counts = Presence {
            presence_tracking: Some(true),
            track_presence: Some(true),
            ..base
        };
        let html = render(&jamie(), &nothing_counts, &[], &[], 1_000);
        assert!(html.contains("None of their devices counts"), "{html}");

        let away = Presence {
            state: Some("away".into()),
            departed: Some(500),
            ..Presence::default()
        };
        let html = render(&jamie(), &away, &[], &[], 1_000);
        assert!(html.contains("<strong>Away</strong>"), "{html}");
    }
}
