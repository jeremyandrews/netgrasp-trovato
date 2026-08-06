#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The plugin's real statements, against the daemon's real schema.
//!
//! This is the test that was missing, and its absence is the whole reason the
//! plugin shipped a set of queries that could not run. Everything else on this
//! side — the unit tests here, the wasm integration test in the kernel — met
//! only the plugin's **own** migration, which was written from a design record
//! rather than from the daemon's landed DDL and disagreed with it about the
//! primary key type, every timestamp, and four column names. A test that seeds
//! its own schema and queries it will agree with itself whatever it says.
//!
//! So this file:
//!
//! 1. applies `tests/fixtures/daemon_schema.sql` — the daemon's DDL, unguarded —
//!    into a scratch Postgres schema of its own;
//! 2. seeds rows the way the daemon writes them;
//! 3. runs the statements from [`netgrasp_core::queries`], the same constants
//!    the plugin executes, bound and decoded through a **mirror of the `db`
//!    host's own JSON conversion** (`crates/kernel/src/host/db.rs`), so a column
//!    the host cannot decode reads here exactly as it would in production: as
//!    `null`.
//!
//! That last point is what makes the timestamp assertions mean anything. The
//! host decodes a fixed list of Postgres types and falls through to a `String`
//! decode for the rest; `timestamptz` fails that decode and arrives as `null`.
//! Asserting "the timeline came back with non-null times" through this harness
//! is therefore the same assertion as "the device page is not blank".
//!
//! Requires Postgres. `DATABASE_URL`, or
//! `postgres://trovato:trovato@localhost:5432/trovato`.

use std::collections::BTreeMap;

use netgrasp_core::columns::{DAEMON_OWNED, LINK_OWNED, USER_OWNED};
use netgrasp_core::model::{DeviceRow, DeviceState, EventRow, Span, SpanRow};
use netgrasp_core::queries;
use netgrasp_core::writeback::{Statement, build_person_upsert, build_update, overlay_from_item};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::postgres::{PgArguments, PgConnection, PgRow};
use sqlx::{Column, Connection, Executor, Row, TypeInfo};
use uuid::Uuid;

/// The daemon's DDL, shipped as a fixture so this test never meets the plugin's
/// copy of it.
const DAEMON_SCHEMA: &str = include_str!("fixtures/daemon_schema.sql");

/// The plugin's guarded copy, for the drift check.
const PLUGIN_MIGRATION: &str =
    include_str!("../../../plugins/netgrasp/migrations/001_netgrasp_schema.sql");

const ITEM_A: &str = "11111111-1111-4111-8111-111111111111";
const ITEM_B: &str = "22222222-2222-4222-8222-222222222222";
const PERSON: &str = "33333333-3333-4333-8333-333333333333";

// ===========================================================================
// The `db` host, mirrored
// ===========================================================================

/// Bind JSON parameters the way the `db` host does.
///
/// Mirrors `bind_json_params` in `crates/kernel/src/host/db.rs`. It matters that
/// this is a mirror and not a convenience: the plugin hands the host
/// `serde_json::Value` parameters, so a device id reaches Postgres as a bound
/// `i64` and an Item id as a bound `String`, and whether `$1::bigint` accepts
/// what it is given is part of what is under test.
fn bind_json<'q>(
    params: &'q [Value],
    mut query: sqlx::query::Query<'q, sqlx::Postgres, PgArguments>,
) -> sqlx::query::Query<'q, sqlx::Postgres, PgArguments> {
    for param in params {
        match param {
            Value::String(s) => query = query.bind(s.clone()),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    query = query.bind(i);
                } else if let Some(f) = n.as_f64() {
                    query = query.bind(f);
                }
            }
            Value::Bool(b) => query = query.bind(*b),
            Value::Null => query = query.bind(Option::<String>::None),
            other => {
                if let Ok(s) = serde_json::to_string(other) {
                    query = query.bind(s);
                }
            }
        }
    }
    query
}

/// Serialize a row to JSON the way the `db` host does.
///
/// Mirrors `row_to_json` in `crates/kernel/src/host/db.rs`, including its
/// fall-through: any type not on the list is decoded as a `String`, and a
/// `timestamptz` fails that decode and becomes `null`. Reproducing the defect is
/// the point — it is what the plugin's queries have to survive.
fn host_row_to_json(row: &PgRow) -> Value {
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        let value = match col.type_info().name() {
            "BOOL" => row
                .try_get::<bool, _>(name)
                .ok()
                .map_or(Value::Null, Value::Bool),
            "INT2" => row
                .try_get::<i16, _>(name)
                .ok()
                .map_or(Value::Null, |v| Value::Number(v.into())),
            "INT4" => row
                .try_get::<i32, _>(name)
                .ok()
                .map_or(Value::Null, |v| Value::Number(v.into())),
            "INT8" => row
                .try_get::<i64, _>(name)
                .ok()
                .map_or(Value::Null, |v| Value::Number(v.into())),
            "FLOAT4" => row
                .try_get::<f32, _>(name)
                .ok()
                .and_then(|v| serde_json::Number::from_f64(f64::from(v)))
                .map_or(Value::Null, Value::Number),
            "FLOAT8" => row
                .try_get::<f64, _>(name)
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::Null, Value::Number),
            "UUID" => row
                .try_get::<Uuid, _>(name)
                .ok()
                .map_or(Value::Null, |v| Value::String(v.to_string())),
            "JSON" | "JSONB" => row.try_get::<Value, _>(name).ok().unwrap_or(Value::Null),
            _ => row
                .try_get::<String, _>(name)
                .ok()
                .map_or(Value::Null, Value::String),
        };
        map.insert(name.to_string(), value);
    }
    Value::Object(map)
}

/// The plugin's `db::query_rows`, over a real connection.
async fn query_rows<T: DeserializeOwned>(
    conn: &mut PgConnection,
    sql: &str,
    params: &[Value],
) -> Vec<T> {
    let rows = bind_json(params, sqlx::query(sql))
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_else(|e| panic!("query failed: {e}\n{sql}"));
    let json = Value::Array(rows.iter().map(host_row_to_json).collect());
    serde_json::from_value(json.clone())
        .unwrap_or_else(|e| panic!("row decode failed: {e}\nrows: {json}\n{sql}"))
}

/// The plugin's `db::exec`, over a real connection.
async fn exec(conn: &mut PgConnection, sql: &str, params: &[Value]) -> u64 {
    bind_json(params, sqlx::query(sql))
        .execute(&mut *conn)
        .await
        .unwrap_or_else(|e| panic!("statement failed: {e}\n{sql}"))
        .rows_affected()
}

// ===========================================================================
// The scratch schema
// ===========================================================================

/// Open a connection with a private schema holding the daemon's DDL.
///
/// A schema rather than a database: the test user is not guaranteed `CREATEDB`,
/// and the schema is per-test-function so two of these can run against one
/// database without meeting. The name carries the process id because CI runs
/// several test binaries against the same Postgres.
async fn daemon_db(test: &str) -> PgConnection {
    scratch(test, DAEMON_SCHEMA).await
}

async fn scratch(test: &str, ddl: &str) -> PgConnection {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let mut conn = PgConnection::connect(&url).await.expect("connect test DB");

    let schema = format!("ng_fx_{}_{test}", std::process::id());
    conn.execute(format!("DROP SCHEMA IF EXISTS {schema} CASCADE").as_str())
        .await
        .unwrap();
    conn.execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    // Unqualified table names in the plugin's statements resolve here. `public`
    // stays on the path for extensions only.
    conn.execute(format!("SET search_path TO {schema}, public").as_str())
        .await
        .unwrap();
    sqlx::raw_sql(ddl)
        .execute(&mut conn)
        .await
        .unwrap_or_else(|e| panic!("apply schema for {test}: {e}"));
    conn
}

/// Insert a device the way the daemon does: timestamps, no Item link, dirty.
async fn seed_device(conn: &mut PgConnection, mac: &str, hostname: Option<&str>, seen: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO ng_devices \
             (mac, hostname, vendor, device_type, os_family, state, last_ip, current_location, \
              resolved_name, identity_source, identity_confidence, first_seen_at, last_seen_at) \
         VALUES ($1, $2, 'Apple', 'phone', 'iOS', 'online', '192.168.1.10', 'living-room-ap', \
                 $2, 'mdns', 0.9, to_timestamp($3), to_timestamp($4)) \
         RETURNING id",
    )
    .bind(mac)
    .bind(hostname)
    .bind(seen as f64 - 900_000.0)
    .bind(seen as f64)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn device_item(item_id: &str, title: &str) -> Value {
    json!({
        "id": item_id,
        "type": "ng_device",
        "title": title,
        "fields": {
            "field_owner": PERSON,
            "field_notes": "work phone",
            "field_hidden": false,
            "field_notify": true,
        }
    })
}

// ===========================================================================
// The device rows
// ===========================================================================

/// The sync pass's own query: a dirty row, decoded into the struct the plugin
/// decodes it into. `id` must arrive as a number and both timestamps as
/// integers — reading `first_seen_at` instead of its epoch twin would put a
/// `null` in an `Option<i64>` and this would pass with `None`, which is why the
/// assertion is on the values and not on the decode succeeding.
#[tokio::test]
async fn the_sync_pass_reads_a_dirty_device_row_with_both_timestamps() {
    let mut conn = daemon_db("dirty_devices").await;
    let seen = now();
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:01", Some("nas"), seen).await;

    let rows: Vec<DeviceRow> =
        query_rows(&mut conn, queries::SELECT_DIRTY_DEVICES, &[json!(10)]).await;

    assert_eq!(rows.len(), 1, "the dirty set did not come back");
    let row = &rows[0];
    assert_eq!(row.id, id, "the device id did not decode as a bigint");
    assert_eq!(row.mac, "aa:bb:cc:00:00:01");
    assert_eq!(row.hostname.as_deref(), Some("nas"));
    assert_eq!(
        row.last_seen,
        Some(seen),
        "last_seen came back null — the query read the timestamptz, not its epoch twin"
    );
    assert_eq!(row.first_seen, Some(seen - 900_000));
    assert!(row.trovato_item_id.is_none());
}

/// The two statements that address a device row by its primary key. A `::uuid`
/// cast here is the error that made every one of them fail against a real
/// daemon database, and it fails loudly rather than returning zero rows:
/// `invalid input syntax for type uuid`.
#[tokio::test]
async fn the_link_and_clean_statements_address_a_device_by_its_bigint_id() {
    let mut conn = daemon_db("link_and_clean").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:02", None, now()).await;

    assert_eq!(
        exec(
            &mut conn,
            queries::UPDATE_LINK_ITEM,
            &[json!(ITEM_A), json!(id)]
        )
        .await,
        1
    );
    assert_eq!(
        exec(&mut conn, queries::UPDATE_MARK_CLEAN, &[json!(id)]).await,
        1
    );

    let (link, sync_state): (Option<Uuid>, String) =
        sqlx::query_as("SELECT trovato_item_id, sync_state FROM ng_devices WHERE id = $1")
            .bind(id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(link, Some(Uuid::parse_str(ITEM_A).unwrap()));
    assert_eq!(sync_state, "clean");

    // And the row falls out of the dirty set, which is what makes the pass
    // idempotent.
    let rows: Vec<DeviceRow> =
        query_rows(&mut conn, queries::SELECT_DIRTY_DEVICES, &[json!(10)]).await;
    assert!(rows.is_empty());
}

/// The device page's own read.
#[tokio::test]
async fn the_device_page_finds_its_row_by_the_item_link() {
    let mut conn = daemon_db("device_state").await;
    let seen = now();
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:03", Some("phone"), seen).await;
    exec(
        &mut conn,
        queries::UPDATE_LINK_ITEM,
        &[json!(ITEM_A), json!(id)],
    )
    .await;

    let rows: Vec<DeviceState> =
        query_rows(&mut conn, queries::SELECT_DEVICE_STATE, &[json!(ITEM_A)]).await;
    let state = rows.first().expect("the device page found no daemon row");
    assert_eq!(state.id, id);
    assert_eq!(state.mac, "aa:bb:cc:00:00:03");
    assert_eq!(state.state.as_deref(), Some("online"));
    assert_eq!(state.current_location.as_deref(), Some("living-room-ap"));
    assert_eq!(
        state.last_seen,
        Some(seen),
        "the identity block would render 'never'"
    );
    assert_eq!(state.first_seen, Some(seen - 900_000));

    // An Item with no row behind it matches nothing rather than raising.
    let none: Vec<DeviceState> =
        query_rows(&mut conn, queries::SELECT_DEVICE_STATE, &[json!(ITEM_B)]).await;
    assert!(none.is_empty());
}

/// The write-back's naming probe, which is the other statement keyed on the
/// Item link.
#[tokio::test]
async fn the_write_back_reads_the_daemons_naming_inputs() {
    let mut conn = daemon_db("daemon_title").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:04", Some("roku"), now()).await;
    exec(
        &mut conn,
        queries::UPDATE_LINK_ITEM,
        &[json!(ITEM_A), json!(id)],
    )
    .await;

    #[derive(serde::Deserialize)]
    struct FallbackRow {
        mac: String,
        hostname: Option<String>,
        vendor: Option<String>,
    }
    let rows: Vec<FallbackRow> = query_rows(
        &mut conn,
        queries::SELECT_DAEMON_TITLE_FIELDS,
        &[json!(ITEM_A)],
    )
    .await;
    let row = rows.first().expect("no naming inputs");
    assert_eq!(row.mac, "aa:bb:cc:00:00:04");
    assert_eq!(row.hostname.as_deref(), Some("roku"));
    assert_eq!(row.vendor.as_deref(), Some("Apple"));
}

// ===========================================================================
// The timelines
// ===========================================================================

/// The three timeline queries, which is where the schema mismatch was total:
/// every one of them named a column the daemon does not have, and bound the
/// device id as a uuid besides.
///
/// The seeded history is one closed session, one open session and one compacted
/// summary row per timeline that has summaries, so the assertions cover the
/// open-span case (`end` is null and only null for the open one) and the
/// summary-exclusion decision at the same time.
#[tokio::test]
async fn every_timeline_returns_its_spans_with_non_null_times() {
    let mut conn = daemon_db("timelines").await;
    let now = now();
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:05", Some("laptop"), now).await;

    sqlx::query(
        "INSERT INTO ng_presence (device_id, ip, started_at, ended_at, is_summary) VALUES \
           ($1, '192.168.1.10', to_timestamp($2), to_timestamp($3), FALSE), \
           ($1, '192.168.1.10', to_timestamp($4), NULL, FALSE), \
           ($1, NULL, to_timestamp($5), to_timestamp($6), TRUE)",
    )
    .bind(id)
    .bind((now - 8_000) as f64)
    .bind((now - 4_000) as f64)
    .bind((now - 600) as f64)
    .bind((now - 200_000) as f64)
    .bind((now - 190_000) as f64)
    .execute(&mut conn)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ng_location_history \
             (device_id, ap_name, location, started_at, ended_at, is_summary) VALUES \
           ($1, 'ap-1', 'living-room-ap', to_timestamp($2), to_timestamp($3), FALSE), \
           ($1, 'ap-2', 'kitchen-ap', to_timestamp($4), NULL, FALSE), \
           ($1, NULL, 'compacted', to_timestamp($5), to_timestamp($6), TRUE)",
    )
    .bind(id)
    .bind((now - 8_000) as f64)
    .bind((now - 4_000) as f64)
    .bind((now - 600) as f64)
    .bind((now - 200_000) as f64)
    .bind((now - 190_000) as f64)
    .execute(&mut conn)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ng_ip_history (device_id, ip, interface, first_seen, last_seen) \
         VALUES ($1, '192.168.1.42', 'eth0', to_timestamp($2), to_timestamp($3))",
    )
    .bind(id)
    .bind((now - 8_000) as f64)
    .bind((now - 600) as f64)
    .execute(&mut conn)
    .await
    .unwrap();

    let limit = json!(100);

    let presence: Vec<SpanRow> = query_rows(
        &mut conn,
        queries::SELECT_PRESENCE_SPANS,
        &[json!(id), limit.clone()],
    )
    .await;
    assert_eq!(
        presence.len(),
        2,
        "presence returned {} rows; the compacted summary row is not a session",
        presence.len()
    );
    assert_eq!(presence[0].start, now - 600, "not newest first");
    assert!(
        presence[0].end.is_none(),
        "the open session came back closed"
    );
    assert_eq!(presence[1].start, now - 8_000);
    assert_eq!(presence[1].end, Some(now - 4_000));

    let locations: Vec<SpanRow> = query_rows(
        &mut conn,
        queries::SELECT_LOCATION_SPANS,
        &[json!(id), limit.clone()],
    )
    .await;
    assert_eq!(locations.len(), 2);
    assert_eq!(locations[0].label.as_deref(), Some("kitchen-ap"));
    assert_eq!(locations[0].start, now - 600);
    assert!(locations[0].end.is_none());
    assert_eq!(locations[1].label.as_deref(), Some("living-room-ap"));

    let addresses: Vec<SpanRow> = query_rows(
        &mut conn,
        queries::SELECT_ADDRESS_SPANS,
        &[json!(id), limit],
    )
    .await;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].label.as_deref(), Some("192.168.1.42"));
    assert_eq!(addresses[0].start, now - 8_000);
    assert_eq!(
        addresses[0].end,
        Some(now - 600),
        "an address holding has a NOT NULL last_seen and is never open"
    );

    // What the page actually renders from: the spans, through the conversion
    // the plugin uses. A null time would have failed the decode above; this
    // asserts the arithmetic on the far side is not garbage either.
    let spans: Vec<Span> = presence.into_iter().map(Span::from).collect();
    assert!(spans[0].is_open());
    assert_eq!(spans[1].duration_secs(now), 4_000);
}

/// A device with no history renders empty timelines rather than raising — the
/// state an install with a fresh daemon is in.
#[tokio::test]
async fn a_device_with_no_history_returns_no_spans() {
    let mut conn = daemon_db("empty_timelines").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:06", None, now()).await;
    for sql in [
        queries::SELECT_PRESENCE_SPANS,
        queries::SELECT_LOCATION_SPANS,
        queries::SELECT_ADDRESS_SPANS,
    ] {
        let spans: Vec<SpanRow> = query_rows(&mut conn, sql, &[json!(id), json!(100)]).await;
        assert!(spans.is_empty(), "{sql}");
    }
}

// ===========================================================================
// The write-back
// ===========================================================================

/// The write-back, executed rather than inspected: the statement
/// [`build_update`] generates has to run against the daemon's actual column
/// types, and its `owner_item_id` is the one uuid in a set of text and boolean.
///
/// The daemon-owned snapshot is taken over **every** column in
/// [`DAEMON_OWNED`], which is now the daemon's whole schema rather than the
/// nine columns the plugin happened to read.
#[tokio::test]
async fn the_write_back_writes_every_user_column_and_disturbs_no_daemon_column() {
    let mut conn = daemon_db("write_back").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:07", Some("phone-1"), now()).await;
    exec(
        &mut conn,
        queries::UPDATE_LINK_ITEM,
        &[json!(ITEM_A), json!(id)],
    )
    .await;
    sqlx::query("INSERT INTO ng_people (item_id, name) VALUES ($1, 'Jeremy')")
        .bind(Uuid::parse_str(PERSON).unwrap())
        .execute(&mut conn)
        .await
        .unwrap();

    let before = daemon_snapshot(&mut conn, id).await;

    let item = device_item(ITEM_A, "Jeremy's iPhone");
    let overlay = overlay_from_item(&item).unwrap();
    let Statement {
        sql,
        params,
        columns,
    } = build_update(ITEM_A, &overlay, None).unwrap();
    assert_eq!(exec(&mut conn, &sql, &params).await, 1);

    // The statement named the user's columns and nobody else's.
    for column in &columns {
        assert!(
            USER_OWNED.contains(&column.as_str()),
            "{column} is not user-owned"
        );
        assert!(!DAEMON_OWNED.contains(&column.as_str()));
        assert!(!LINK_OWNED.contains(&column.as_str()));
    }

    let row = sqlx::query(
        "SELECT display_name, owner_item_id, notes, hidden, notify FROM ng_devices WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        row.get::<Option<String>, _>(0).as_deref(),
        Some("Jeremy's iPhone")
    );
    assert_eq!(
        row.get::<Option<Uuid>, _>(1),
        Some(Uuid::parse_str(PERSON).unwrap())
    );
    assert_eq!(
        row.get::<Option<String>, _>(2).as_deref(),
        Some("work phone")
    );
    assert!(!row.get::<bool, _>(3));
    assert!(row.get::<bool, _>(4));

    assert_eq!(
        before,
        daemon_snapshot(&mut conn, id).await,
        "the write-back moved a daemon-owned column"
    );
}

/// Every daemon-owned column as text, so a comparison proves none moved.
///
/// The generated epoch twins are in the set and are read here too: they are
/// derived from `first_seen_at` / `last_seen_at`, so a write-back that somehow
/// touched a timestamp would show up twice.
async fn daemon_snapshot(conn: &mut PgConnection, id: i64) -> BTreeMap<String, Option<String>> {
    let projection = DAEMON_OWNED
        .iter()
        .map(|c| format!("{c}::text AS {c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(&format!(
        "SELECT {projection} FROM ng_devices WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .unwrap_or_else(|e| panic!("a column in DAEMON_OWNED does not exist: {e}"));
    DAEMON_OWNED
        .iter()
        .map(|c| {
            (
                (*c).to_string(),
                row.try_get::<Option<String>, _>(*c).unwrap(),
            )
        })
        .collect()
}

/// The person mirror and the two retirement statements, which are the only
/// other places the plugin writes.
#[tokio::test]
async fn the_person_mirror_upserts_and_retires_without_touching_the_daemons_columns() {
    let mut conn = daemon_db("person_mirror").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:08", None, now()).await;

    let person = json!({
        "id": PERSON,
        "type": "ng_person",
        "title": "Jeremy",
        "fields": {"field_notes": "household", "field_notify_arrive": true}
    });
    let Statement { sql, params, .. } = build_person_upsert(&person).unwrap();
    exec(&mut conn, &sql, &params).await;
    // Twice, because a re-fired tap and a human clicking save twice are the
    // same event as far as this row is concerned.
    exec(&mut conn, &sql, &params).await;

    let (name, notes, notify, state): (String, Option<String>, bool, String) = sqlx::query_as(
        "SELECT name, notes, notify_arrive, state FROM ng_people WHERE item_id = $1",
    )
    .bind(Uuid::parse_str(PERSON).unwrap())
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(name, "Jeremy");
    assert_eq!(notes.as_deref(), Some("household"));
    assert!(notify);
    assert_eq!(
        state, "away",
        "the mirror upsert overwrote a daemon-owned column of ng_people"
    );

    sqlx::query("UPDATE ng_devices SET owner_item_id = $1 WHERE id = $2")
        .bind(Uuid::parse_str(PERSON).unwrap())
        .bind(id)
        .execute(&mut conn)
        .await
        .unwrap();

    assert_eq!(
        exec(&mut conn, queries::UPDATE_CLEAR_OWNER, &[json!(PERSON)]).await,
        1
    );
    assert_eq!(
        exec(&mut conn, queries::DELETE_PERSON_MIRROR, &[json!(PERSON)]).await,
        1
    );

    // The device outlives its owner: it is still on the network.
    let (owner, still_there): (Option<Uuid>, i64) =
        sqlx::query_as("SELECT owner_item_id, count(*) OVER () FROM ng_devices WHERE id = $1")
            .bind(id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(owner, None);
    assert_eq!(still_there, 1);
}

/// Deleting a device Item unlinks the row and queues it for a fresh one.
#[tokio::test]
async fn unlinking_a_deleted_items_row_re_dirties_it() {
    let mut conn = daemon_db("unlink").await;
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:09", None, now()).await;
    exec(
        &mut conn,
        queries::UPDATE_LINK_ITEM,
        &[json!(ITEM_A), json!(id)],
    )
    .await;
    exec(&mut conn, queries::UPDATE_MARK_CLEAN, &[json!(id)]).await;

    assert_eq!(
        exec(&mut conn, queries::UPDATE_UNLINK_DEVICE, &[json!(ITEM_A)]).await,
        1
    );
    let rows: Vec<DeviceRow> =
        query_rows(&mut conn, queries::SELECT_DIRTY_DEVICES, &[json!(10)]).await;
    assert_eq!(
        rows.len(),
        1,
        "the unlinked row was not queued for a new Item"
    );
    assert!(rows[0].trovato_item_id.is_none());
}

// ===========================================================================
// Events
// ===========================================================================

/// Retention, over `timestamp_epoch`. Comparing the cutoff against the
/// `timestamptz` itself would not have compiled in Postgres' eyes — `bigint`
/// against `timestamptz` has no operator — so this is the one place the old
/// schema mismatch would have failed loudly rather than silently.
#[tokio::test]
async fn expired_events_prune_and_current_ones_do_not() {
    let mut conn = daemon_db("retention").await;
    let now = now();
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:0a", None, now).await;

    for (event_type, age_days) in [
        ("device_seen", 1),
        ("device_seen", 89),
        ("device_seen", 91),
        ("mac_spoof", 200),
    ] {
        sqlx::query(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details) \
             VALUES ($1, $2, to_timestamp($3), $4::jsonb)",
        )
        .bind(id)
        .bind(event_type)
        .bind((now - age_days * 86_400) as f64)
        .bind(json!({"claimed_mac": "aa:bb:cc:00:00:0a"}))
        .execute(&mut conn)
        .await
        .unwrap();
    }

    let cutoff = netgrasp_core::retention::cutoff(now, 90);
    let pruned = exec(
        &mut conn,
        queries::DELETE_EXPIRED_EVENTS,
        &[json!(cutoff), json!(netgrasp_core::retention::PRUNE_BATCH)],
    )
    .await;
    assert_eq!(pruned, 2);
    // Idempotent: a second pass over a pruned log deletes nothing.
    assert_eq!(
        exec(
            &mut conn,
            queries::DELETE_EXPIRED_EVENTS,
            &[json!(cutoff), json!(netgrasp_core::retention::PRUNE_BATCH)],
        )
        .await,
        0
    );
}

/// `details` is JSONB and the host decodes JSONB, so an event row's detail
/// arrives as an object. The plugin used to declare it a string, which would
/// have failed this decode.
#[tokio::test]
async fn an_event_rows_details_decode_as_an_object() {
    let mut conn = daemon_db("event_details").await;
    let now = now();
    let id = seed_device(&mut conn, "aa:bb:cc:00:00:0b", None, now).await;
    sqlx::query(
        "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details) \
         VALUES ($1, 'mac_spoof', to_timestamp($2), $3::jsonb)",
    )
    .bind(id)
    .bind(now as f64)
    .bind(json!({"claimed_mac": "aa:bb:cc:00:00:0b", "observations": 3}))
    .execute(&mut conn)
    .await
    .unwrap();

    let rows: Vec<EventRow> = query_rows(
        &mut conn,
        "SELECT event_type, timestamp_epoch AS timestamp, details FROM ng_events \
         WHERE device_id = $1::bigint",
        &[json!(id)],
    )
    .await;
    let event = rows.first().expect("no event");
    assert_eq!(event.timestamp, now);
    assert_eq!(
        event.detail("claimed_mac").as_deref(),
        Some("aa:bb:cc:00:00:0b")
    );
    assert_eq!(event.detail("observations").as_deref(), Some("3"));
}

// ===========================================================================
// The host's type coverage, stated directly
// ===========================================================================

/// The finding underneath the whole reconciliation, asserted rather than
/// described: a `timestamptz` selected through the `db` host arrives as `null`,
/// and its generated `bigint` twin arrives as the integer the plugin renders
/// from. If the host ever grows a `TIMESTAMPTZ` arm, this fails and the epoch
/// companions can go.
#[tokio::test]
async fn a_timestamptz_is_null_through_the_db_host_and_its_epoch_twin_is_not() {
    let mut conn = daemon_db("host_type_coverage").await;
    let seen = now();
    seed_device(&mut conn, "aa:bb:cc:00:00:0c", None, seen).await;

    let rows: Vec<Value> = query_rows(
        &mut conn,
        "SELECT last_seen_at, last_seen_at_epoch FROM ng_devices LIMIT 1",
        &[],
    )
    .await;
    let row = &rows[0];
    assert_eq!(
        row["last_seen_at"],
        Value::Null,
        "the db host decoded a timestamptz — G-DB-HOST-TYPE-COVERAGE may be fixed, and the \
         epoch companion columns are no longer needed"
    );
    assert_eq!(row["last_seen_at_epoch"], json!(seen));
}

// ===========================================================================
// The plugin's copy of the schema
// ===========================================================================

/// The plugin ships a guarded copy of the daemon's DDL so an install with no
/// daemon can still enable it. A copy that has drifted is exactly the defect
/// this whole change repaired, and `CREATE TABLE IF NOT EXISTS` will never
/// report it — so the two are compared here, column by column.
#[tokio::test]
async fn the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema() {
    let mut daemon = scratch("copy_daemon", DAEMON_SCHEMA).await;
    let mut plugin = scratch("copy_plugin", PLUGIN_MIGRATION).await;

    for table in [
        "ng_devices",
        "ng_presence",
        "ng_events",
        "ng_ip_history",
        "ng_location_history",
        "ng_people",
    ] {
        let expected = columns_of(&mut daemon, "copy_daemon", table).await;
        let actual = columns_of(&mut plugin, "copy_plugin", table).await;
        assert!(!expected.is_empty(), "the fixture has no {table}");
        assert_eq!(
            expected, actual,
            "the plugin's migration has drifted from the daemon's schema on {table}"
        );
    }

    // ng_state is the plugin's own scratch table and is deliberately not the
    // daemon's, so the copy has one table the canonical schema does not.
    let plugin_state = columns_of(&mut plugin, "copy_plugin", "ng_state").await;
    assert!(!plugin_state.is_empty(), "the plugin lost its own ng_state");
    assert!(
        columns_of(&mut daemon, "copy_daemon", "ng_state")
            .await
            .is_empty(),
        "ng_state reached the canonical schema, which the daemon does not know about"
    );
}

/// One table's columns as `(name, type, nullable, generated)`, ordered.
async fn columns_of(
    conn: &mut PgConnection,
    test: &str,
    table: &str,
) -> Vec<(String, String, String, String)> {
    sqlx::query_as(
        "SELECT column_name, data_type, is_nullable, is_generated \
         FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 \
         ORDER BY ordinal_position",
    )
    .bind(format!("ng_fx_{}_{test}", std::process::id()))
    .bind(table)
    .fetch_all(&mut *conn)
    .await
    .unwrap()
}
