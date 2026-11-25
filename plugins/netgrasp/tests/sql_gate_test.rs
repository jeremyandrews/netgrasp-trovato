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
use std::path::{Path, PathBuf};
use trovato_kernel::host::sql_guard::{check_execute_raw, check_query_raw};

const ITEM: &str = "019a4730-0000-7000-8000-000000000001";

/// Which host call a statement belongs on: a read goes to `query-raw`, anything
/// else to `execute-raw`. `every_named_statement_is_sent_through_the_host_call_the_gate_checks_it_for`
/// holds the plugin's call sites to the same rule.
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

/// A call that sends a named statement: (file, `query_rows` or `exec`, name).
type CallSite = (String, &'static str, String);

/// Every call in the plugin's source that sends a `queries::` constant, as
/// (file, function, constant): `query_rows` sends through `query-raw`, `exec`
/// through `execute-raw` (`src/db.rs`).
/// Also every `queries::` constant the source names anywhere, so a statement
/// sent some other way (through a variable, say) is noticed rather than
/// skipped.
fn named_call_sites() -> (Vec<CallSite>, Vec<(String, String)>) {
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut sites = Vec::new();
    let mut named = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (at, _) in text.match_indices("queries::") {
            let name: String = text[at + "queries::".len()..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if !name.is_empty() {
                named.push((file.display().to_string(), name));
            }
        }
        for call in ["query_rows", "exec"] {
            for (at, _) in text.match_indices(call) {
                // A whole identifier: `exec`, not `my_exec` or `execute`.
                let before = text[..at].chars().next_back();
                let mut rest = &text[at + call.len()..];
                let after = rest.chars().next();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    || after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    continue;
                }
                // A turbofish, `query_rows::<Row>(`.
                if let Some(generic) = rest.strip_prefix("::<") {
                    let Some(close) = generic.find('>') else {
                        continue;
                    };
                    rest = &generic[close + 1..];
                }
                let Some(rest) = rest.strip_prefix('(') else {
                    continue;
                };
                let rest = rest.trim_start();
                let rest = rest.strip_prefix("netgrasp_core::").unwrap_or(rest);
                let Some(rest) = rest.strip_prefix("queries::") else {
                    continue;
                };
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect();
                sites.push((file.display().to_string(), call, name));
            }
        }
    }
    (sites, named)
}

/// The gate check above picks `query-raw` or `execute-raw` from the statement,
/// so it would pass a read that a call site sends through `exec` (which the
/// kernel refuses at run time). This holds each call site to the same choice.
#[test]
fn every_named_statement_is_sent_through_the_host_call_the_gate_checks_it_for() {
    let (sites, named) = named_call_sites();
    assert!(
        sites.len() > 40,
        "found only {} call sites sending a queries:: constant; the scan is not seeing them",
        sites.len()
    );
    let mut wrong = Vec::new();
    for (file, call, name) in &sites {
        let Some((_, sql)) = queries::ALL.iter().find(|(n, _)| n == name) else {
            wrong.push(format!(
                "{file}: {call}(queries::{name}) is not in queries::ALL"
            ));
            continue;
        };
        let wants = if is_read(sql) { "query_rows" } else { "exec" };
        if *call != wants {
            wrong.push(format!(
                "{file}: queries::{name} is sent with {call}, and the gate wants {wants}"
            ));
        }
    }
    for (file, name) in &named {
        if !sites.iter().any(|(_, _, sent)| sent == name) {
            wrong.push(format!(
                "{file}: queries::{name} is named but never sent directly through query_rows or exec, so its host call is not checked"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
