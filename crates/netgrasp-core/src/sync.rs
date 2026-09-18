//! The daemon→kernel direction: what a sync pass does with a dirty row.
//!
//! The daemon marks a `ng_devices` row `sync_state = 'dirty'` when it creates or
//! changes it. A cron pass reads the dirty set and, for each row, has to decide
//! among four outcomes. That decision is pure, so it lives here and is tested
//! exhaustively without a database; the plugin's host port only executes the
//! plan it is handed.
//!
//! # Why the plan is so small
//!
//! Everything an `ng_device` Item holds is user-owned except its title
//! (`DESIGN.md` Decision 1), so there is almost nothing for the daemon side to
//! push. The whole daemon→kernel payload is a **derived title**. That is the
//! point rather than a shortcut: a sync that carried volatile state onto the
//! Item would write an `item_revision` row every time a device flipped online,
//! and would have to read-modify-write the Item's fields to avoid clobbering the
//! admin's edits (`Item::update` replaces the whole `fields` object —
//! `G-ITEM-NO-MERGE`) with no transaction to make that safe (`G-DB-NO-TX`).
//!
//! [`SyncAction::Refresh`] sends the title **and no `fields` key at all**, which
//! the kernel reads as "leave the fields alone"
//! (`Item::update`: `input.fields.unwrap_or(current.fields)`). So the sync pass
//! physically cannot clobber a user-owned value, whatever races it loses.

use crate::model::DeviceRow;

/// What a sync pass should do with one dirty device row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// No linked Item: create one, then write its id back onto the row.
    Create {
        /// Title for the new Item.
        title: String,
    },
    /// The row names an Item that no longer exists (an operator deleted it).
    /// Create a replacement and repoint the row.
    Relink {
        /// The dangling id, carried so the caller can log what it repaired.
        stale_item_id: String,
        /// Title for the replacement Item.
        title: String,
    },
    /// The Item exists and its derived title has moved: update the title only.
    Refresh {
        /// The linked Item.
        item_id: String,
        /// The title it should now carry.
        title: String,
    },
    /// The Item exists and is already correct. Nothing to write but the
    /// `sync_state` clear.
    Skip {
        /// The linked Item.
        item_id: String,
    },
}

impl SyncAction {
    /// The title this action would put on the Item, if any.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        match self {
            SyncAction::Create { title }
            | SyncAction::Relink { title, .. }
            | SyncAction::Refresh { title, .. } => Some(title),
            SyncAction::Skip { .. } => None,
        }
    }

    /// Whether this action writes anything through `item-api`.
    ///
    /// [`SyncAction::Skip`] does not, which is what makes a repeated pass over
    /// an unchanged dirty set free.
    #[must_use]
    pub fn writes_item(&self) -> bool {
        !matches!(self, SyncAction::Skip { .. })
    }
}

/// Longest title the sync will derive.
///
/// A hostname is attacker-influenceable on an open LAN (DHCP option 12 is
/// whatever the client says it is), and the title lands in an `item` row and on
/// every page that lists devices. Truncation is on the way in.
pub const MAX_TITLE_LEN: usize = 128;

/// Everything a device's name can be derived from.
///
/// One struct, so that "what to call this device" is one question asked of one
/// set of inputs. Before it there were two ladders over two row shapes — the
/// sync derived a title from a [`DeviceRow`] and the assistant labelled a
/// `DeviceFacts` — and they disagreed: the sync's skipped `resolved_name` and
/// `mdns_name`, which the daemon's identity resolution had already settled and
/// every page already showed. So a Brother printer's Item was titled
/// "CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD. device" while the proposal
/// card for the same row said "Brother HL-L8360CDW series"
/// (`docs/JOINT-RUN.md`, plugin finding 2). The better name was in the
/// database; nothing was reading it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TitleInputs<'a> {
    /// The name a human typed, from `ng_devices.display_name`.
    pub display_name: Option<&'a str>,
    /// The daemon's resolved identity, from `ng_devices.resolved_name`.
    pub resolved_name: Option<&'a str>,
    /// Reverse-DNS or DHCP hostname.
    pub hostname: Option<&'a str>,
    /// mDNS name.
    pub mdns_name: Option<&'a str>,
    /// OUI vendor.
    pub vendor: Option<&'a str>,
    /// Hardware address. Always present, so a title always exists.
    pub mac: &'a str,
}

impl<'a> From<&'a DeviceRow> for TitleInputs<'a> {
    fn from(row: &'a DeviceRow) -> Self {
        Self {
            display_name: row.display_name.as_deref(),
            resolved_name: row.resolved_name.as_deref(),
            hostname: row.hostname.as_deref(),
            mdns_name: row.mdns_name.as_deref(),
            vendor: row.vendor.as_deref(),
            mac: &row.mac,
        }
    }
}

/// The name somebody actually gave this device, or that something on the wire
/// did. `None` when nothing has named it.
///
/// The ladder, and it is the only one: the name a human typed, then the name
/// the daemon resolved, then the hostname it was given, then the name it
/// advertised over mDNS. A vendor is deliberately **not** here — a vendor is
/// not a name, and the two callers that fall back to one say so differently
/// (a title says "Apple device", a proposal card says "Apple tablet").
#[must_use]
pub fn observed_name<'a>(inputs: &TitleInputs<'a>) -> Option<&'a str> {
    [
        inputs.display_name,
        inputs.resolved_name,
        inputs.hostname,
        inputs.mdns_name,
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|name| !name.is_empty())
}

/// The title a device's Item carries.
///
/// One function for the sync's mint, the sync's title refresh and the
/// assistant's context alike, so a device is called the same thing wherever it
/// is named. `display_name` first, then whatever the daemon resolved, then the
/// vendor qualified as a device, then the MAC, which always exists.
///
/// The user-first precedence is also half of the loop-termination argument:
/// after one write-back, `display_name` is the title, so re-deriving the title
/// yields the title. See [`is_fixed_point`].
#[must_use]
pub fn device_title(inputs: &TitleInputs) -> String {
    let title = match observed_name(inputs) {
        Some(name) => name.to_string(),
        None => match non_blank(inputs.vendor) {
            // A vendor is qualified, because a vendor is not a name: an OUI
            // holder called "Apple, Inc." is not what anybody calls the thing
            // on their desk.
            Some(vendor) => format!("{vendor} device"),
            None => inputs.mac.trim().to_string(),
        },
    };
    truncate_chars(&title, MAX_TITLE_LEN)
}

/// The label a device should carry, given what a row says about it.
#[must_use]
pub fn derive_title(row: &DeviceRow) -> String {
    device_title(&row.into())
}

/// The title the **daemon's** observations alone imply, ignoring any name a
/// human gave the device.
///
/// This is the value the write-back compares an admin's title against: if the
/// admin saved the Item without changing its name, the title still equals this,
/// and storing it as `display_name` would pin the device's name forever —
/// freezing it against every name the daemon later resolves, as a side effect
/// of editing an unrelated field. [`crate::writeback::build_partial_update`]
/// stores `NULL` in that case instead.
///
/// Keeping it a function rather than a `CASE` expression in the write-back's SQL
/// is what stops the two derivations from drifting: there is one definition of
/// "what the daemon would call this device", and both callers use it.
#[must_use]
pub fn daemon_title(inputs: &TitleInputs) -> String {
    device_title(&TitleInputs {
        display_name: None,
        ..*inputs
    })
}

/// The value, trimmed, if it is present and not blank.
fn non_blank(candidate: Option<&str>) -> Option<&str> {
    candidate.map(str::trim).filter(|c| !c.is_empty())
}

/// Truncate on a character boundary, never mid-`char`.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Decide what to do with one dirty row.
///
/// `item_exists` answers "does the Item this row names still exist" and is the
/// caller's job because it costs a host call; it is ignored when the row names
/// no Item at all.
///
/// `current_title` is the linked Item's present title, so an unchanged device
/// costs no write.
#[must_use]
pub fn plan(row: &DeviceRow, item_exists: bool, current_title: Option<&str>) -> SyncAction {
    let title = derive_title(row);

    let linked = row
        .trovato_item_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty() && !is_nil_uuid(id));

    match linked {
        None => SyncAction::Create { title },
        Some(id) if !item_exists => SyncAction::Relink {
            stale_item_id: id.to_string(),
            title,
        },
        Some(id) if current_title == Some(title.as_str()) => SyncAction::Skip {
            item_id: id.to_string(),
        },
        Some(id) => SyncAction::Refresh {
            item_id: id.to_string(),
            title,
        },
    }
}

/// Whether a row is at the sync/write-back fixed point.
///
/// True when re-running the whole cycle — derive a title, write it to the Item,
/// write the Item's title back as `display_name`, derive a title again — changes
/// nothing. This is the property that makes the loop terminate *by discipline*,
/// independently of the fact that it currently cannot even start
/// (`DESIGN.md` Decision 4).
#[must_use]
pub fn is_fixed_point(row: &DeviceRow) -> bool {
    let title = derive_title(row);
    // Simulate the write-back: display_name := title.
    let mut after = row.clone();
    after.display_name = Some(title.clone());
    derive_title(&after) == title
}

/// Whether a uuid string is the nil uuid, which the kernel treats as "no id".
fn is_nil_uuid(id: &str) -> bool {
    id.chars().all(|c| c == '0' || c == '-')
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const ITEM: &str = "22222222-2222-4222-8222-222222222222";

    fn row() -> DeviceRow {
        DeviceRow::new(11, "aa:bb:cc:dd:ee:ff")
    }

    // --- derive_title -----------------------------------------------------

    #[test]
    fn a_bare_device_is_titled_by_its_mac() {
        assert_eq!(derive_title(&row()), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn a_vendor_alone_is_qualified_because_a_vendor_is_not_a_name() {
        let mut r = row();
        r.vendor = Some("Apple".into());
        assert_eq!(derive_title(&r), "Apple device");
    }

    #[test]
    fn a_hostname_beats_a_vendor_and_is_used_bare() {
        let mut r = row();
        r.vendor = Some("Apple".into());
        r.hostname = Some("jeremys-phone".into());
        assert_eq!(derive_title(&r), "jeremys-phone");
    }

    /// The defect this ladder was widened for: the daemon had resolved
    /// "Brother HL-L8360CDW series" and the title read the OUI holder instead.
    #[test]
    fn the_daemons_resolved_name_beats_the_vendor_it_bought_its_oui_from() {
        let mut r = row();
        r.vendor = Some("CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD.".into());
        r.resolved_name = Some("Brother HL-L8360CDW series".into());
        assert_eq!(derive_title(&r), "Brother HL-L8360CDW series");
    }

    #[test]
    fn a_resolved_name_beats_a_hostname_and_an_mdns_name() {
        let mut r = row();
        r.mdns_name = Some("Filbert-3".into());
        r.hostname = Some("dhcp-claimed".into());
        r.resolved_name = Some("studio-nas".into());
        assert_eq!(derive_title(&r), "studio-nas");
    }

    /// Every step of the ladder, in order, each one falling through to the next
    /// as its input is removed. One test rather than five, because what is
    /// being asserted is the *order* and not the individual answers.
    #[test]
    fn each_step_of_the_ladder_falls_through_to_the_next() {
        let mut r = row();
        r.display_name = Some("Office printer".into());
        r.resolved_name = Some("Brother HL-L8360CDW series".into());
        r.hostname = Some("brother-printer".into());
        r.mdns_name = Some("Brother._ipp".into());
        r.vendor = Some("Brother Industries".into());

        assert_eq!(derive_title(&r), "Office printer");
        r.display_name = None;
        assert_eq!(derive_title(&r), "Brother HL-L8360CDW series");
        r.resolved_name = None;
        assert_eq!(derive_title(&r), "brother-printer");
        r.hostname = None;
        assert_eq!(derive_title(&r), "Brother._ipp");
        r.mdns_name = None;
        assert_eq!(derive_title(&r), "Brother Industries device");
        r.vendor = None;
        assert_eq!(derive_title(&r), "aa:bb:cc:dd:ee:ff");
    }

    /// A name a human typed is not overtaken by anything the daemon learns
    /// afterwards — including an mDNS name, which on a real LAN is attributed
    /// to the wrong device constantly (`docs/JOINT-RUN.md`, daemon finding 11).
    #[test]
    fn a_hand_set_display_name_wins_over_a_later_mdns_name() {
        let mut r = row();
        r.display_name = Some("Office printer".into());
        assert_eq!(derive_title(&r), "Office printer");

        r.mdns_name = Some("Jeremy's MacBook Pro (2)".into());
        r.resolved_name = Some("Jeremy's MacBook Pro (2)".into());
        assert_eq!(
            derive_title(&r),
            "Office printer",
            "a misattributed mDNS name took a device's typed name"
        );
    }

    /// `daemon_title` is the same ladder with the human's name taken out — the
    /// comparison the write-back makes to decide whether a title was typed or
    /// merely left alone.
    #[test]
    fn the_daemon_title_ignores_the_name_a_human_typed() {
        let mut r = row();
        r.display_name = Some("Office printer".into());
        r.resolved_name = Some("Brother HL-L8360CDW series".into());
        assert_eq!(
            daemon_title(&(&r).into()),
            "Brother HL-L8360CDW series",
            "the daemon's own view must not include the human's name"
        );
        assert_eq!(derive_title(&r), "Office printer");
    }

    /// A vendor is not a name, so it is not on the named ladder at all: a
    /// caller that wants to fall back to one does it itself, and says so in its
    /// own words.
    #[test]
    fn the_named_ladder_stops_before_the_vendor() {
        let mut r = row();
        r.vendor = Some("Apple".into());
        assert_eq!(observed_name(&(&r).into()), None);
        r.mdns_name = Some("  ".into());
        assert_eq!(observed_name(&(&r).into()), None, "a blank name is no name");
        r.mdns_name = Some(" living-room-tv ".into());
        assert_eq!(observed_name(&(&r).into()), Some("living-room-tv"));
    }

    #[test]
    fn the_users_display_name_beats_everything_the_daemon_observed() {
        let mut r = row();
        r.vendor = Some("Apple".into());
        r.hostname = Some("jeremys-phone".into());
        r.display_name = Some("Jeremy's iPhone".into());
        assert_eq!(derive_title(&r), "Jeremy's iPhone");
    }

    #[test]
    fn blank_and_whitespace_candidates_are_skipped_not_used() {
        let mut r = row();
        r.display_name = Some("   ".into());
        r.hostname = Some(String::new());
        r.vendor = Some("\t".into());
        assert_eq!(derive_title(&r), "aa:bb:cc:dd:ee:ff");
    }

    /// A hostname is whatever a DHCP client claims, so it is bounded before it
    /// reaches an `item` row.
    #[test]
    fn an_absurd_hostname_is_truncated_on_a_char_boundary() {
        let mut r = row();
        r.hostname = Some("é".repeat(500));
        let title = derive_title(&r);
        assert_eq!(title.chars().count(), MAX_TITLE_LEN);
        // Truncating by bytes would have split a two-byte char and produced
        // invalid UTF-8; that this is a String at all is the assertion.
        assert!(title.chars().all(|c| c == 'é'));
    }

    // --- plan -------------------------------------------------------------

    #[test]
    fn an_unlinked_row_is_created() {
        let action = plan(&row(), false, None);
        assert_eq!(
            action,
            SyncAction::Create {
                title: "aa:bb:cc:dd:ee:ff".into()
            }
        );
        assert!(action.writes_item());
    }

    #[test]
    fn a_row_linked_to_a_missing_item_is_relinked_and_reports_the_stale_id() {
        let mut r = row();
        r.trovato_item_id = Some(ITEM.into());
        assert_eq!(
            plan(&r, false, None),
            SyncAction::Relink {
                stale_item_id: ITEM.into(),
                title: "aa:bb:cc:dd:ee:ff".into()
            }
        );
    }

    #[test]
    fn a_linked_row_whose_title_already_matches_is_skipped_and_writes_nothing() {
        let mut r = row();
        r.trovato_item_id = Some(ITEM.into());
        r.hostname = Some("nas".into());
        let action = plan(&r, true, Some("nas"));
        assert_eq!(
            action,
            SyncAction::Skip {
                item_id: ITEM.into()
            }
        );
        assert!(!action.writes_item());
    }

    #[test]
    fn a_linked_row_whose_derived_title_moved_is_refreshed() {
        let mut r = row();
        r.trovato_item_id = Some(ITEM.into());
        r.hostname = Some("nas".into());
        assert_eq!(
            plan(&r, true, Some("aa:bb:cc:dd:ee:ff")),
            SyncAction::Refresh {
                item_id: ITEM.into(),
                title: "nas".into()
            }
        );
    }

    /// An empty string and the nil uuid both mean "not linked". The kernel reads
    /// a nil `id` in a `save-item` payload as a create, so treating it as a link
    /// would produce a create that looked like an update.
    #[test]
    fn an_empty_or_nil_link_is_treated_as_unlinked() {
        for id in ["", "   ", "00000000-0000-0000-0000-000000000000"] {
            let mut r = row();
            r.trovato_item_id = Some(id.into());
            assert!(
                matches!(plan(&r, true, Some("whatever")), SyncAction::Create { .. }),
                "link {id:?} should not count as a link"
            );
        }
    }

    // --- idempotency and termination --------------------------------------

    /// The scope's "idempotent" requirement, stated at the level that decides
    /// it: running the plan again over the state the first run produced must ask
    /// for no further writes.
    #[test]
    fn replanning_after_a_refresh_asks_for_no_further_write() {
        let mut r = row();
        r.trovato_item_id = Some(ITEM.into());
        r.hostname = Some("nas".into());

        let first = plan(&r, true, Some("old-name"));
        let title = first.title().unwrap().to_string();
        assert!(first.writes_item());

        // Second pass over the same row, with the Item now carrying that title.
        let second = plan(&r, true, Some(&title));
        assert!(
            !second.writes_item(),
            "a second pass over an unchanged row must not write"
        );
    }

    /// The loop-termination property, by discipline rather than by contract:
    /// after the write-back has copied the title into `display_name`, deriving
    /// the title again yields the same string, so the cycle has a fixed point
    /// after exactly one pass.
    #[test]
    fn the_sync_write_back_cycle_reaches_a_fixed_point_after_one_pass() {
        let cases = [
            row(),
            {
                let mut r = row();
                r.hostname = Some("nas".into());
                r
            },
            {
                let mut r = row();
                r.vendor = Some("Apple".into());
                r
            },
            {
                let mut r = row();
                r.display_name = Some("Jeremy's iPhone".into());
                r.hostname = Some("jeremys-phone".into());
                r.vendor = Some("Apple".into());
                r
            },
        ];
        for r in cases {
            assert!(
                is_fixed_point(&r),
                "row {r:?} does not reach a fixed point — the loop would not terminate"
            );
        }
    }

    /// The dangerous case specifically: a vendor-derived title is *qualified*
    /// ("Apple device"), so if the write-back stored it as `display_name` and
    /// the next derivation re-qualified it, titles would grow without bound
    /// ("Apple device device device"). They do not, because `display_name` wins
    /// outright and is used bare.
    #[test]
    fn a_qualified_vendor_title_does_not_re_qualify_on_the_next_pass() {
        let mut r = row();
        r.vendor = Some("Apple".into());
        assert_eq!(derive_title(&r), "Apple device");

        r.display_name = Some("Apple device".into());
        assert_eq!(derive_title(&r), "Apple device");

        r.display_name = Some(derive_title(&r));
        assert_eq!(derive_title(&r), "Apple device");
    }
}
