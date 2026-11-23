//! Every raw statement the plugin sends passes the kernel's raw SQL gate.
//!
//! Kernel 0.107 parses each `query-raw` and `execute-raw` statement before it
//! runs (`trovato_kernel::host::sql_guard`): `query-raw` takes one read and
//! nothing that writes, `execute-raw` takes exactly one INSERT, UPDATE or
//! DELETE, and neither may name a kernel credential table or call a denied
//! function. A refused statement is a host error at runtime (-11, or -16 for a
//! protected table) on whatever page or tool sent it, and nothing else notices.
//!
//! So the gate itself is run here, from the pinned kernel, over every statement
//! in `netgrasp_core::queries::ALL` and every statement the write-back builders
//! generate. No database is needed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use netgrasp_core::model::{DeviceEdit, DeviceOverlay};
use netgrasp_core::queries;
use netgrasp_core::writeback::{build_partial_update, build_person_upsert, build_update};
use serde_json::json;
use trovato_kernel::host::sql_guard::{check_execute_raw, check_query_raw};

const ITEM: &str = "019a4730-0000-7000-8000-000000000001";

/// Which host call a statement goes through: a read goes to `query-raw`,
/// anything else to `execute-raw`. That is how `crate::db::query_rows` and
/// `crate::db::exec` are used throughout the plugin.
fn is_read(sql: &str) -> bool {
    let head = sql.trim_start().to_ascii_uppercase();
    head.starts_with("SELECT") || head.starts_with("WITH")
}

#[test]
fn every_statement_in_queries_all_passes_the_kernels_gate() {
    let mut refused = Vec::new();
    for (name, sql) in queries::ALL {
        let verdict = if is_read(sql) {
            check_query_raw(sql)
        } else {
            check_execute_raw(sql)
        };
        if let Err(why) = verdict {
            refused.push(format!("{name}: {why}"));
        }
    }
    assert!(
        refused.is_empty(),
        "the kernel's raw SQL gate refuses:\n{}",
        refused.join("\n")
    );
    assert!(
        queries::ALL.len() > 40,
        "queries::ALL lists only {} statements; the gate check is not seeing them",
        queries::ALL.len()
    );
}

#[test]
fn every_generated_write_back_passes_the_kernels_gate() {
    let overlay = DeviceOverlay {
        display_name: "A device".into(),
        owner_item_id: Some(ITEM.into()),
        notes: Some("notes".into()),
        hidden: true,
        notify: true,
        counts_for_presence: Some(false),
    };
    let whole = build_update(ITEM, &overlay, Some("a fallback")).unwrap();
    let rename = build_partial_update(
        ITEM,
        &DeviceEdit {
            display_name: Some("A new name".into()),
            ..DeviceEdit::default()
        },
        None,
    )
    .unwrap();
    let unassign = build_partial_update(
        ITEM,
        &DeviceEdit {
            owner_item_id: Some(None),
            ..DeviceEdit::default()
        },
        None,
    )
    .unwrap();
    let person = build_person_upsert(&json!({
        "id": ITEM,
        "title": "A person",
        "fields": {"field_notes": "", "field_notify_arrive": true},
    }))
    .unwrap();

    for (what, statement) in [
        ("whole device write-back", whole),
        ("sparse rename", rename),
        ("sparse unassign", unassign),
        ("person mirror", person),
    ] {
        check_execute_raw(&statement.sql)
            .unwrap_or_else(|why| panic!("{what} is refused: {why}\n{}", statement.sql));
    }
}

/// The two statements kernel 0.107 refused, kept as a guard so the shape does
/// not come back: a writable `WITH` is neither a read nor one row change.
#[test]
fn a_writable_cte_is_what_the_gate_refuses() {
    let writable_cte = "WITH gone AS (DELETE FROM ng_people WHERE item_id = $1::uuid \
                        RETURNING item_id) SELECT count(*) FROM gone";
    assert!(check_query_raw(writable_cte).is_err());
    assert!(check_execute_raw(writable_cte).is_err());
}
