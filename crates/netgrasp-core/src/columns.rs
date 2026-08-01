//! Who owns which column of `ng_devices`.
//!
//! The scope's requirement is that "the user-owned columns are a fixed disjoint
//! set from the daemon-owned columns so the two writers never collide". This
//! module is that requirement written down once, in a form the write-back
//! statement builder is forced to consult and a test can check.
//!
//! Three writers, three sets, no overlap:
//!
//! | Set | Writer | Contents |
//! |---|---|---|
//! | [`DAEMON_OWNED`] | the native netgrasp daemon | what it observes on the wire |
//! | [`USER_OWNED`] | an admin, via `tap_item_update` on the device Item | what a human decides |
//! | [`LINK_OWNED`] | this plugin's cron sync | the Item linkage |
//!
//! `sync_state` is deliberately in [`DAEMON_OWNED`] and **not** in
//! [`LINK_OWNED`], even though the plugin does write it. That is not an
//! inconsistency: `sync_state` is the daemon's signal *to* the plugin, and
//! putting it in the daemon's set is what makes
//! [`crate::writeback::build_update`] structurally unable to emit it — which is
//! the whole loop-termination argument (`DESIGN.md` Decision 4).
//!
//! Exactly two statements in the plugin write it, both outside the write-back
//! and both in `sync_host.rs`: the sync pass lowers it after handling a row, and
//! deleting a device Item raises it so the next pass mints a replacement. The
//! second is safe for the reason the write-back is not — it fires on a delete,
//! which cannot recur, so it cannot close a cycle.

/// Columns of `ng_devices` the daemon writes and nothing else may.
///
/// Sorted, so the disjointness test and any diff of this list read cleanly.
pub const DAEMON_OWNED: &[&str] = &[
    "current_location",
    "device_type",
    "first_seen",
    "hostname",
    "last_ip",
    "last_seen",
    "mac",
    "os_family",
    "state",
    "sync_state",
    "vendor",
];

/// Columns of `ng_devices` a human owns, written back from the device Item.
///
/// `display_name` carries the Item's **title**, not a field: the title is what
/// an admin actually edits on the content form, and duplicating it into a field
/// would give one value two editable homes.
pub const USER_OWNED: &[&str] = &["display_name", "hidden", "notes", "notify", "owner_item_id"];

/// Columns this plugin owns: the link from a device row to its Item.
pub const LINK_OWNED: &[&str] = &["trovato_item_id"];

/// Whether `column` is one an admin edit is allowed to write.
#[must_use]
pub fn is_user_owned(column: &str) -> bool {
    USER_OWNED.contains(&column)
}

/// Whether `column` belongs to the daemon.
#[must_use]
pub fn is_daemon_owned(column: &str) -> bool {
    DAEMON_OWNED.contains(&column)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(cols: &[&str]) -> HashSet<String> {
        cols.iter().map(|c| (*c).to_string()).collect()
    }

    #[test]
    fn the_three_ownership_sets_are_pairwise_disjoint() {
        let daemon = set(DAEMON_OWNED);
        let user = set(USER_OWNED);
        let link = set(LINK_OWNED);

        let daemon_user: Vec<_> = daemon.intersection(&user).collect();
        assert!(
            daemon_user.is_empty(),
            "daemon and user sets overlap: {daemon_user:?} — the two writers would collide"
        );
        let daemon_link: Vec<_> = daemon.intersection(&link).collect();
        assert!(
            daemon_link.is_empty(),
            "daemon and link overlap: {daemon_link:?}"
        );
        let user_link: Vec<_> = user.intersection(&link).collect();
        assert!(user_link.is_empty(), "user and link overlap: {user_link:?}");
    }

    #[test]
    fn no_set_repeats_a_column() {
        for (name, cols) in [
            ("daemon", DAEMON_OWNED),
            ("user", USER_OWNED),
            ("link", LINK_OWNED),
        ] {
            assert_eq!(
                set(cols).len(),
                cols.len(),
                "{name} set has a duplicate entry"
            );
        }
    }

    #[test]
    fn each_set_is_sorted_so_a_diff_of_this_file_reads_cleanly() {
        for (name, cols) in [
            ("daemon", DAEMON_OWNED),
            ("user", USER_OWNED),
            ("link", LINK_OWNED),
        ] {
            let mut sorted = cols.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, cols.to_vec(), "{name} set is not sorted");
        }
    }

    /// `sync_state` is the signal the daemon raises and the sync pass lowers. It
    /// must sit in the daemon's set so the write-back builder cannot emit it —
    /// that is what stops an admin edit from re-triggering a sync pass.
    #[test]
    fn sync_state_is_daemon_owned_so_the_write_back_cannot_raise_it() {
        assert!(is_daemon_owned("sync_state"));
        assert!(!is_user_owned("sync_state"));
    }

    #[test]
    fn predicates_agree_with_the_tables() {
        assert!(is_user_owned("display_name"));
        assert!(is_user_owned("owner_item_id"));
        assert!(!is_user_owned("mac"));
        assert!(!is_user_owned("trovato_item_id"));
        assert!(is_daemon_owned("last_seen"));
        assert!(!is_daemon_owned("notes"));
    }
}
