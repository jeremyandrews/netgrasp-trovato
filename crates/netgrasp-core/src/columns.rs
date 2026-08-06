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
/// This is the daemon's landed schema as of its milestone 3, not the subset the
/// plugin happens to read: a column the plugin has never heard of is still a
/// column the write-back must be unable to name, and the list is what makes that
/// checkable.
///
/// Three of these are unwritable by **anyone**. `first_seen_at_epoch`,
/// `last_seen_at_epoch` and the timeline tables' twins are
/// `GENERATED ALWAYS … STORED`, so Postgres refuses a write to them whoever
/// sends it. They are listed here anyway, because "the daemon owns it" is the
/// reason a reader may treat them as authoritative and the reason no writer
/// should try.
///
/// Sorted, so the disjointness test and any diff of this list read cleanly.
pub const DAEMON_OWNED: &[&str] = &[
    "baseline",
    "current_ap",
    "current_location",
    "device_type",
    "device_type_confidence",
    "first_seen_at",
    "first_seen_at_epoch",
    "hostname",
    "identity_confidence",
    "identity_source",
    "last_interface",
    "last_ip",
    "last_ipv6",
    "last_seen_at",
    "last_seen_at_epoch",
    "mac",
    "mdns_name",
    "os_family",
    "resolved_name",
    "state",
    "sync_state",
    "vendor",
];

/// Columns of `ng_devices` a human owns, written back from the device Item.
///
/// `display_name` carries the Item's **title**, not a field: the title is what
/// an admin actually edits on the content form, and duplicating it into a field
/// would give one value two editable homes.
///
/// `owner_item_id` is a `UUID` — it holds an `ng_person` **Item** id, and the
/// kernel's `item.id` is a uuid. It is the one column in this set that is not
/// text or boolean, and [`crate::writeback::build_update`] casts its placeholder
/// `::uuid` for exactly that reason. Device ids, by contrast, are `bigint`;
/// nothing in this set is one.
pub const USER_OWNED: &[&str] = &["display_name", "hidden", "notes", "notify", "owner_item_id"];

/// Columns this plugin owns: the link from a device row to its Item.
///
/// `UUID`, for the same reason `owner_item_id` is.
pub const LINK_OWNED: &[&str] = &["trovato_item_id"];

/// Columns of `ng_devices` that hold a kernel Item id and are therefore `UUID`.
///
/// Named as a set because getting one of them wrong is a cast error two layers
/// from where it was written: every statement binding one casts `::uuid`, and
/// every statement binding a device id casts `::bigint`.
pub const UUID_TYPED: &[&str] = &["owner_item_id", "trovato_item_id"];

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
        assert!(is_daemon_owned("last_seen_at"));
        assert!(!is_daemon_owned("notes"));
    }

    /// The columns the plugin was written against before it was reconciled with
    /// the daemon's landed schema. Naming one here would put a column that does
    /// not exist into a list whose whole job is to be exhaustive.
    #[test]
    fn no_set_names_a_column_the_daemon_dropped_or_never_had() {
        for absent in [
            "first_seen",
            "last_seen",
            "start_time",
            "end_time",
            "ip_address",
        ] {
            for (name, cols) in [
                ("daemon", DAEMON_OWNED),
                ("user", USER_OWNED),
                ("link", LINK_OWNED),
            ] {
                assert!(
                    !cols.contains(&absent),
                    "{name} set names '{absent}', which is not a column of ng_devices"
                );
            }
        }
    }

    /// The generated twins are the daemon's, so the write-back cannot name one.
    /// Postgres would refuse the write anyway; this is the layer that says why.
    #[test]
    fn the_generated_epoch_twins_are_daemon_owned() {
        for twin in ["first_seen_at_epoch", "last_seen_at_epoch"] {
            assert!(is_daemon_owned(twin), "{twin} is not in the daemon's set");
            assert!(!is_user_owned(twin));
        }
    }

    /// A device id is a `bigint` and every Item link is a `uuid`. Confusing the
    /// two is a cast error at runtime and nothing at compile time.
    #[test]
    fn the_uuid_typed_columns_are_exactly_the_item_links() {
        assert_eq!(UUID_TYPED, ["owner_item_id", "trovato_item_id"]);
        for column in UUID_TYPED {
            assert!(
                is_user_owned(column) || LINK_OWNED.contains(column),
                "{column} is uuid-typed but owned by nobody"
            );
        }
        assert!(!UUID_TYPED.contains(&"id"), "a device id is a bigint");
    }
}
