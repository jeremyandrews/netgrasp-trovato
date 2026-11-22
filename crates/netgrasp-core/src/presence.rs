//! Who counts for presence, as the plugin shows and edits it.
//!
//! The daemon decides who is home (netgraspd, `src/people/mod.rs`). Since its
//! schema V7 that decision has two inputs a person sets here:
//!
//! - `ng_devices.counts_for_presence`, per device, three states: `NULL` decides
//!   by device type, `TRUE` counts, `FALSE` does not. By type, a device counts
//!   when the daemon classified it as a `phone` or a `wearable`.
//! - `ng_people.track_presence`, per person, off by default.
//!
//! and one the operator sets in the daemon's config, `presence_tracking`,
//! published as `ng_daemon_status.presence_tracking`. A person is "not tracked"
//! unless all three say yes and they own a device that counts.
//!
//! [`counts_for_presence`] repeats the daemon's rule so a page can say what a
//! device's effective value is and why. The daemon's copy is the one that acts;
//! this one only explains, and a test holds the type list equal to the
//! daemon's documented one.

/// The device types that count when nobody chose: the ones a person carries.
/// The same list as netgraspd's `people::PRESENCE_DEVICE_TYPES`.
pub const PRESENCE_DEVICE_TYPES: [&str; 2] = ["phone", "wearable"];

/// The config line that turns presence tracking on for the whole install.
pub const GLOBAL_SWITCH: &str = "presence_tracking = true";

/// Whether a device decides if its owner is home: the person's choice when
/// there is one, otherwise whether its type is one a person carries.
#[must_use]
pub fn counts_for_presence(counts_override: Option<bool>, device_type: Option<&str>) -> bool {
    counts_override
        .unwrap_or_else(|| device_type.is_some_and(|t| PRESENCE_DEVICE_TYPES.contains(&t.trim())))
}

/// The three choices a person has for one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceChoice {
    /// `NULL`: decide by device type.
    ByType,
    /// `TRUE`: this device says whether its owner is home.
    Counts,
    /// `FALSE`: it never does.
    DoesNotCount,
}

impl PresenceChoice {
    /// Every choice, in the order a form offers them.
    pub const ALL: [PresenceChoice; 3] = [
        PresenceChoice::ByType,
        PresenceChoice::Counts,
        PresenceChoice::DoesNotCount,
    ];

    /// The value stored in the `ng_device` Item's `field_counts_for_presence`
    /// and posted by the form.
    #[must_use]
    pub const fn as_value(self) -> &'static str {
        match self {
            PresenceChoice::ByType => "by_type",
            PresenceChoice::Counts => "counts",
            PresenceChoice::DoesNotCount => "does_not_count",
        }
    }

    /// What a form calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PresenceChoice::ByType => "By device type",
            PresenceChoice::Counts => "Counts",
            PresenceChoice::DoesNotCount => "Does not count",
        }
    }

    /// The column value.
    #[must_use]
    pub const fn to_override(self) -> Option<bool> {
        match self {
            PresenceChoice::ByType => None,
            PresenceChoice::Counts => Some(true),
            PresenceChoice::DoesNotCount => Some(false),
        }
    }

    /// The choice a column value is.
    #[must_use]
    pub const fn from_override(value: Option<bool>) -> Self {
        match value {
            None => PresenceChoice::ByType,
            Some(true) => PresenceChoice::Counts,
            Some(false) => PresenceChoice::DoesNotCount,
        }
    }

    /// Reads a stored or posted value. Tolerates the spellings an admin might
    /// type into the content form's text box (`yes`, `true`, `no`, …); anything
    /// else, blank included, is "by device type", which is the default.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "counts" | "count" | "true" | "yes" | "on" | "1" => PresenceChoice::Counts,
            "does_not_count" | "doesnt_count" | "not" | "no" | "false" | "off" | "0" => {
                PresenceChoice::DoesNotCount
            }
            _ => PresenceChoice::ByType,
        }
    }
}

/// The effective value of one device and the reason, as the edit form says it:
/// "Counts: phone", "Does not count: computer", "Counts: chosen for this
/// device", "Does not count: type not known yet".
#[must_use]
pub fn effective_description(counts_override: Option<bool>, device_type: Option<&str>) -> String {
    let counts = counts_for_presence(counts_override, device_type);
    let verdict = if counts { "Counts" } else { "Does not count" };
    let why = match (
        counts_override,
        device_type.map(str::trim).filter(|t| !t.is_empty()),
    ) {
        (Some(_), _) => "chosen for this device".to_string(),
        (None, Some(t)) => t.to_string(),
        (None, None) => "type not known yet".to_string(),
    };
    format!("{verdict}: {why}")
}

/// How a person's presence reads on a page. `state` is `ng_people.state`.
///
/// Anything other than `home` or `away` reads as not tracked, never as away:
/// `untracked` is the daemon's word for it, and a value this build does not
/// know is not evidence that somebody left.
#[must_use]
pub fn state_label(state: Option<&str>) -> &'static str {
    match state {
        Some("home") => "Home",
        Some("away") => "Away",
        _ => "Not tracked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_phones_and_wearables_count_by_type_and_a_choice_wins() {
        assert!(counts_for_presence(None, Some("phone")));
        assert!(counts_for_presence(None, Some("wearable")));
        for t in ["computer", "tablet", "tv", "printer"] {
            assert!(!counts_for_presence(None, Some(t)), "{t}");
            assert!(counts_for_presence(Some(true), Some(t)), "{t}");
        }
        assert!(!counts_for_presence(Some(false), Some("phone")));
        assert!(!counts_for_presence(None, None));
    }

    #[test]
    fn the_three_choices_round_trip_through_the_field_and_the_column() {
        for choice in PresenceChoice::ALL {
            assert_eq!(PresenceChoice::parse(choice.as_value()), choice);
            assert_eq!(PresenceChoice::from_override(choice.to_override()), choice);
        }
        assert_eq!(PresenceChoice::parse(""), PresenceChoice::ByType);
        assert_eq!(
            PresenceChoice::parse("Does not count"),
            PresenceChoice::DoesNotCount
        );
        assert_eq!(PresenceChoice::parse("yes"), PresenceChoice::Counts);
    }

    #[test]
    fn the_effective_value_says_why() {
        assert_eq!(effective_description(None, Some("phone")), "Counts: phone");
        assert_eq!(
            effective_description(None, Some("computer")),
            "Does not count: computer"
        );
        assert_eq!(
            effective_description(Some(true), Some("computer")),
            "Counts: chosen for this device"
        );
        assert_eq!(
            effective_description(None, None),
            "Does not count: type not known yet"
        );
    }

    #[test]
    fn untracked_and_unknown_states_are_never_away() {
        assert_eq!(state_label(Some("home")), "Home");
        assert_eq!(state_label(Some("away")), "Away");
        assert_eq!(state_label(Some("untracked")), "Not tracked");
        assert_eq!(state_label(Some("something new")), "Not tracked");
        assert_eq!(state_label(None), "Not tracked");
    }
}
