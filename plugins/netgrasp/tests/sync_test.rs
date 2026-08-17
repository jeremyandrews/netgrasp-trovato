#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Netgrasp integration: the bidirectional device sync, the write-back's column
//! discipline, event retention, and the record gathers.
//!
//! Drives the **real** `plugins/netgrasp` wasm through the real `TapDispatcher`,
//! the real `ItemService`, the real `GatherService` and a real Postgres. The
//! parts that can be settled without a database are settled in `netgrasp-core`;
//! what is asserted here is everything that only shows up when a host is
//! involved:
//!
//! - a dirty daemon row becomes a device Item, once, however many times the pass
//!   runs;
//! - an admin's edit reaches the daemon's user-owned columns and **no others**;
//! - the sync/write-back loop terminates, and the kernel behaviour it currently
//!   terminates *because of* is pinned so the day it changes, a test says so;
//! - a device Item the sync writes leaves the admin's fields untouched;
//! - events prune on the retention window;
//! - the record gathers return rows.
//!
//! Build the wasm first:
//!
//! ```text
//! cargo build -p netgrasp --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/netgrasp.wasm plugins/netgrasp/
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

use trovato_kernel::content::{ContentTypeRegistry, ItemService, RecordTypeRegistry};
use trovato_kernel::gather::{
    CategoryService, GatherExtensionRegistry, GatherService, QueryContext,
};
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const PLUGIN: &str = "netgrasp";
const DEVICE_TYPE: &str = "ng_device";
const PERSON_TYPE: &str = "ng_person";
const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";

/// The daemon-owned columns, as `netgrasp_core::columns::DAEMON_OWNED` names
/// them. Restated here rather than imported: the point of the column-discipline
/// test is to check the plugin's *behaviour* against an independently written
/// list, and importing the same constant it is built from would make the
/// assertion circular.
///
/// These are the daemon's names, which are not the plugin's old ones: the two
/// observation timestamps are `first_seen_at` / `last_seen_at`, and each carries
/// a generated `_epoch` twin that the plugin reads instead (a `timestamptz`
/// decodes as `null` through the `db` host).
const DAEMON_COLUMNS: &[&str] = &[
    "resolved_name",
    "identity_source",
    "hostname",
    "mdns_name",
    "vendor",
    "device_type",
    "os_family",
    "state",
    "last_ip",
    "last_ipv6",
    "last_interface",
    "current_ap",
    "current_location",
    "first_seen_at",
    "last_seen_at",
    "first_seen_at_epoch",
    "last_seen_at_epoch",
];

static SERIAL: Mutex<()> = Mutex::new(());

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

/// The plugin's source directory: this crate's own root, holding the manifest
/// and `migrations/`.
///
/// In the Trovato monorepo this test lived in the kernel's `tests/` and had to
/// climb two levels to reach `plugins/`. Here it lives in the plugin it tests,
/// so `CARGO_MANIFEST_DIR` **is** that directory.
fn plugin_source_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The directory the kernel can LOAD the plugin from: the built overlay.
///
/// Two directories, not one, and this is the difference that matters in a
/// standalone repo. The kernel wants the module and the manifest side by side;
/// the module is a build artifact under `target/` and the manifest is source, so
/// something has to put them together. In the monorepo that was a `cp` in a
/// doc comment that everybody forgot, and the committed `netgrasp.wasm` it
/// produced meant tests ran against whatever had last been copied in. Here
/// `scripts/build-overlay.sh` assembles it, and the panic below names that
/// script rather than a `cp`.
fn loadable_plugin_dir() -> PathBuf {
    plugin_source_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("overlay/plugins")
        .join(PLUGIN)
}

/// `.env` loading, once.
///
/// `dotenvy::dotenv` mutates the process environment and the tests here run
/// concurrently, so it must not be called per test function. In the monorepo this
/// went through `trovato_test_utils::env`, which exists because that workspace has
/// many test binaries contending for one environment; here there is one, so a
/// `OnceLock` is the whole requirement.
fn load_dotenv() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        dotenvy::dotenv().ok();
    });
}

/// Every migration the manifest ships, in the order it ships them.
///
/// Read out of the manifest rather than listed here. The monorepo's copy carried
/// a hand-written list with a comment asking the next person to keep it in step,
/// and by the time this moved it was two migrations behind — which matters more
/// than it sounds: 006 creates the view the `ng_device_state` record type is
/// declared over, so a test that skips it exercises a record type whose backing
/// relation does not exist.
fn manifest_migrations() -> Vec<String> {
    let manifest = include_str!("../netgrasp.info.toml");
    manifest
        .split("[migrations]")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
        })
        .collect()
}

fn dispatcher() -> Arc<TapDispatcher> {
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            runtime
                .load_plugin(&loadable_plugin_dir())
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load '{PLUGIN}' from {}: {e:#}\n\
                         build the overlay first: scripts/build-overlay.sh",
                        loadable_plugin_dir().display()
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

async fn fresh_pool() -> PgPool {
    load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run kernel migrations");
    let migrations = manifest_migrations();
    assert!(
        migrations.len() >= 6,
        "the manifest's migration list failed to parse: {migrations:?}"
    );
    for migration in migrations {
        let sql = std::fs::read_to_string(plugin_source_dir().join(&migration))
            .unwrap_or_else(|e| panic!("read {migration}: {e}"));
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply {migration}: {e}"));
    }
    ContentTypeRegistry::new(pool.clone(), Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("register netgrasp content types");
    pool
}

async fn reset(pool: &PgPool) {
    for stmt in [
        "DELETE FROM item WHERE type IN ('ng_device', 'ng_person')",
        // One statement, because the timeline and event tables carry foreign
        // keys onto ng_devices now: truncating it alone is refused.
        "TRUNCATE ng_devices, ng_presence, ng_location_history, ng_ip_history, ng_events CASCADE",
        "TRUNCATE ng_people",
        "TRUNCATE ng_state",
    ] {
        sqlx::query(stmt).execute(pool).await.unwrap();
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs() as i64
}

fn background(pool: &PgPool) -> RequestState {
    let disp = dispatcher();
    RequestState::new(
        UserContext::background(),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    )
}

/// An `ItemService` wired to the real dispatcher, so `update` fires
/// `tap_item_update` exactly as the admin content route does.
fn items(pool: &PgPool) -> Arc<ItemService> {
    let disp = dispatcher();
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone());
    Arc::new(ItemService::new(
        pool.clone(),
        disp,
        services,
        Duration::from_secs(60),
        None,
        None,
    ))
}

/// Run one cron cycle and return the plugin's report.
async fn run_cron(pool: &PgPool) -> serde_json::Value {
    let input = serde_json::json!({ "timestamp": now() }).to_string();
    let results = dispatcher()
        .dispatch("tap_cron", &input, background(pool))
        .await;
    assert_eq!(results.len(), 1, "expected exactly one tap_cron result");
    serde_json::from_str(&results[0].output).expect("tap_cron returned non-JSON")
}

/// Any user id, for a column that only needs to satisfy a foreign key.
async fn any_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Insert a device row the way the daemon would: observation columns filled in,
/// `sync_state = 'dirty'`, no Item link.
///
/// The id is the table's own identity sequence — `ng_devices.id` is
/// `BIGINT GENERATED ALWAYS AS IDENTITY`, so a caller cannot supply one — and
/// the two observation timestamps are `timestamptz`.
async fn seed_device(pool: &PgPool, mac: &str, hostname: Option<&str>, state: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO ng_devices \
             (mac, hostname, vendor, device_type, os_family, state, last_ip, \
              current_location, first_seen_at, last_seen_at, sync_state) \
         VALUES ($1, $2, 'Apple', 'phone', 'iOS', $3, '192.168.1.10', \
                 'living-room-ap', to_timestamp($4), to_timestamp($4), 'dirty') \
         RETURNING id",
    )
    .bind(mac)
    .bind(hostname)
    .bind(state)
    .bind(now() as f64)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Snapshot the daemon-owned columns of a device row as text, so a later
/// comparison proves none of them moved.
async fn daemon_snapshot(pool: &PgPool, device_id: i64) -> Vec<(String, Option<String>)> {
    let cols = DAEMON_COLUMNS
        .iter()
        .map(|c| format!("{c}::text AS {c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(&format!("SELECT {cols} FROM ng_devices WHERE id = $1"))
        .bind(device_id)
        .fetch_one(pool)
        .await
        .unwrap();
    DAEMON_COLUMNS
        .iter()
        .map(|c| {
            (
                (*c).to_string(),
                row.try_get::<Option<String>, _>(*c).unwrap(),
            )
        })
        .collect()
}

/// The linked Item id and sync state of a device row.
async fn link_of(pool: &PgPool, device_id: i64) -> (Option<Uuid>, String) {
    let row = sqlx::query("SELECT trovato_item_id, sync_state FROM ng_devices WHERE id = $1")
        .bind(device_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

async fn device_item_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item WHERE type = $1")
        .bind(DEVICE_TYPE)
        .fetch_one(pool)
        .await
        .unwrap()
}

// ===========================================================================
// Declarations
// ===========================================================================

/// The plugin's record types must be admitted, and their names must not collide
/// with its content types — the registry rejects a record type whose name is
/// also a content type, and the skeleton declared six Items with two of these
/// names.
#[test]
fn the_record_types_are_admitted_and_do_not_collide_with_the_item_types() {
    serial(async {
        let pool = fresh_pool().await;
        let disp = dispatcher();
        let compiled = disp.runtime().get_plugin(PLUGIN).expect("plugin loaded");

        let content_names: HashSet<String> =
            [DEVICE_TYPE.to_string(), PERSON_TYPE.to_string()].into();
        let (registry, errors) = RecordTypeRegistry::build(
            [(
                PLUGIN,
                compiled.db_policy().as_ref(),
                compiled.info.record_types.as_slice(),
            )],
            &content_names,
        );
        assert!(errors.is_empty(), "record types rejected: {errors:?}");
        for name in [
            "ng_device_state",
            "ng_event",
            "ng_presence",
            "ng_location",
            "ng_ip_history",
            "ng_person_mirror",
        ] {
            assert!(registry.contains(name), "{name} was not admitted");
        }
        // The two Item types must NOT be record types.
        assert!(!registry.contains(DEVICE_TYPE));
        assert!(!registry.contains(PERSON_TYPE));
        drop(pool);
    });
}

/// Every table the plugin's SQL touches must be inside its effective allowlist,
/// or a structured call is denied at runtime with `table-not-declared`.
#[test]
fn every_ng_table_is_inside_the_plugins_effective_db_allowlist() {
    serial(async {
        let _pool = fresh_pool().await;
        let compiled = dispatcher().runtime().get_plugin(PLUGIN).unwrap();
        let policy = compiled.db_policy();
        for table in [
            "ng_devices",
            "ng_people",
            "ng_events",
            "ng_presence",
            "ng_location_history",
            "ng_ip_history",
            "ng_state",
        ] {
            assert!(
                policy.check_table(table).is_ok(),
                "{table} is outside the effective allowlist"
            );
        }
        // And the fence still holds for something it does not own.
        assert!(policy.check_table("users").is_err());
    });
}

// ===========================================================================
// daemon → kernel
// ===========================================================================

#[test]
fn a_dirty_daemon_row_becomes_a_device_item_and_the_row_is_marked_clean() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:01", Some("nas"), "online").await;

        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["created"], 1, "report: {report}");

        let (item_id, sync_state) = link_of(&pool, device).await;
        let item_id = item_id.expect("device row was not linked to an Item");
        assert_eq!(sync_state, "clean");

        let (item_type, title): (String, String) =
            sqlx::query_as("SELECT type, title FROM item WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(item_type, DEVICE_TYPE);
        // Derived from the hostname, since the admin has not named it yet.
        assert_eq!(title, "nas");
    });
}

/// The idempotency requirement, at the level that decides it: the second pass
/// must create nothing, and the third must not either.
#[test]
fn re_running_the_sync_creates_no_second_item_however_many_times_it_runs() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_device(&pool, "aa:bb:cc:00:00:02", Some("printer"), "online").await;

        let first = run_cron(&pool).await;
        assert_eq!(first["sync"]["created"], 1);
        assert_eq!(device_item_count(&pool).await, 1);

        // Nothing is dirty any more, so the next passes examine nothing.
        for _ in 0..2 {
            let again = run_cron(&pool).await;
            assert_eq!(again["sync"]["examined"], 0, "report: {again}");
            assert_eq!(device_item_count(&pool).await, 1);
        }

        // Even if the daemon re-dirties the row without changing anything, the
        // pass must recognise the Item as already correct.
        sqlx::query("UPDATE ng_devices SET sync_state = 'dirty'")
            .execute(&pool)
            .await
            .unwrap();
        let redirtied = run_cron(&pool).await;
        assert_eq!(redirtied["sync"]["examined"], 1);
        assert_eq!(redirtied["sync"]["skipped"], 1, "report: {redirtied}");
        assert_eq!(redirtied["sync"]["created"], 0);
        assert_eq!(device_item_count(&pool).await, 1);
    });
}

/// A device row whose Item an operator deleted is relinked, not duplicated and
/// not left dangling.
#[test]
fn a_device_row_pointing_at_a_deleted_item_is_relinked_to_a_fresh_one() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:03", Some("tv"), "online").await;
        run_cron(&pool).await;
        let (first_item, _) = link_of(&pool, device).await;
        let first_item = first_item.unwrap();

        // Delete the Item behind the plugin's back and re-dirty the row, as
        // tap_item_delete would.
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(first_item)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE ng_devices SET sync_state = 'dirty' WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();

        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["relinked"], 1, "report: {report}");

        let (second_item, sync_state) = link_of(&pool, device).await;
        let second_item = second_item.expect("row was not relinked");
        assert_ne!(second_item, first_item);
        assert_eq!(sync_state, "clean");
        assert_eq!(device_item_count(&pool).await, 1);
    });
}

/// The sync drains a backlog over successive ticks rather than trying to do it
/// all in one, and says so in its report. `MAX_DEVICES_PER_TICK` is 200, so a
/// 201-row backlog is the smallest case that proves the bound is real.
#[test]
fn a_backlog_larger_than_one_tick_drains_over_successive_ticks() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        const TOTAL: usize = 205;
        const PER_TICK: usize = 200;

        // One statement: 205 individual inserts through sqlx is slower than the
        // thing being tested.
        sqlx::query(
            "INSERT INTO ng_devices (mac, state, first_seen_at, last_seen_at, sync_state) \
             SELECT 'aa:bb:cc:' || lpad(to_hex(i), 6, '0'), \
                    'online', to_timestamp($2::bigint - i), to_timestamp($2::bigint - i), 'dirty' \
             FROM generate_series(1, $1) AS i",
        )
        .bind(TOTAL as i32)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        let first = run_cron(&pool).await;
        assert_eq!(first["sync"]["examined"], PER_TICK, "report: {first}");
        assert_eq!(first["sync"]["created"], PER_TICK);
        assert_eq!(
            first["sync"]["more_pending"], true,
            "a full page must report that more remain"
        );

        let second = run_cron(&pool).await;
        assert_eq!(second["sync"]["examined"], TOTAL - PER_TICK);
        assert_eq!(second["sync"]["more_pending"], false);

        assert_eq!(device_item_count(&pool).await, TOTAL as i64);
        let dirty: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ng_devices WHERE sync_state = 'dirty'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(dirty, 0, "backlog did not drain");
    });
}

// ===========================================================================
// kernel → daemon: the write-back
// ===========================================================================

/// The write-back itself: an admin's edit through the same `ItemService::update`
/// the admin content route calls must reach the daemon's user-owned columns.
#[test]
fn an_admin_edit_reaches_the_daemons_user_owned_columns() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:04", Some("phone-1"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item_id = item_id.unwrap();

        let author = any_user(&pool).await;
        let person = Uuid::now_v7();
        sqlx::query("INSERT INTO ng_people (item_id, name) VALUES ($1, 'Jeremy')")
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();

        items(&pool)
            .update(
                item_id,
                UpdateItem {
                    title: Some("Jeremy's iPhone".into()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_mac": "aa:bb:cc:00:00:04",
                        "field_owner": person.to_string(),
                        "field_notes": "work phone",
                        "field_hidden": false,
                        "field_notify": true,
                    })),
                    log: None,
                },
                &UserContext::authenticated(author, vec!["edit ng_device content".into()]),
            )
            .await
            .expect("admin edit")
            .expect("item exists");

        let row = sqlx::query(
            "SELECT display_name, owner_item_id, notes, hidden, notify \
             FROM ng_devices WHERE id = $1",
        )
        .bind(device)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row.get::<Option<String>, _>(0).as_deref(),
            Some("Jeremy's iPhone")
        );
        assert_eq!(row.get::<Option<Uuid>, _>(1), Some(person));
        assert_eq!(
            row.get::<Option<String>, _>(2).as_deref(),
            Some("work phone")
        );
        assert!(!row.get::<bool, _>(3));
        assert!(row.get::<bool, _>(4));
    });
}

/// Column discipline, direction one: the write-back must not disturb a single
/// daemon-owned column. Asserted against a full before/after snapshot rather
/// than a sampled column, so a future edit that widens the SET list is caught.
#[test]
fn the_write_back_leaves_every_daemon_owned_column_untouched() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:05", Some("laptop"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let before = daemon_snapshot(&pool, device).await;

        let author = any_user(&pool).await;
        items(&pool)
            .update(
                item_id.unwrap(),
                UpdateItem {
                    title: Some("Renamed by an admin".into()),
                    status: None,
                    promote: None,
                    sticky: None,
                    // Deliberately hostile: fields named after daemon columns.
                    // The write-back builds its SET list from its own column
                    // constant, so these cannot become assignments.
                    fields: Some(serde_json::json!({
                        "field_mac": "aa:bb:cc:00:00:05",
                        "field_notes": "renamed",
                        "hostname": "attacker-supplied",
                        "state": "offline",
                        "last_ip": "10.0.0.1",
                        "sync_state": "dirty",
                    })),
                    log: None,
                },
                &UserContext::authenticated(author, vec!["edit ng_device content".into()]),
            )
            .await
            .unwrap()
            .unwrap();

        let after = daemon_snapshot(&pool, device).await;
        assert_eq!(before, after, "the write-back moved a daemon-owned column");
    });
}

/// **Loop termination.** The write-back must not raise `sync_state`, so the
/// admin's edit cannot cause a sync pass, so the sync pass cannot cause another
/// write-back. Asserted end to end: edit, then run cron and see it examine
/// nothing.
#[test]
fn an_admin_edit_does_not_re_trigger_the_sync_so_the_loop_terminates() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:06", Some("watch"), "online").await;
        run_cron(&pool).await;
        let (item_id, state) = link_of(&pool, device).await;
        assert_eq!(state, "clean");

        let author = any_user(&pool).await;
        items(&pool)
            .update(
                item_id.unwrap(),
                UpdateItem {
                    title: Some("Jeremy's watch".into()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({"field_mac": "aa:bb:cc:00:00:06"})),
                    log: None,
                },
                &UserContext::authenticated(author, vec!["edit ng_device content".into()]),
            )
            .await
            .unwrap()
            .unwrap();

        // The row absorbed the edit and stayed clean.
        let (_, after_edit) = link_of(&pool, device).await;
        assert_eq!(
            after_edit, "clean",
            "the write-back marked the row dirty — the sync loop would not terminate"
        );

        // So the next pass has nothing to do, and the one after that still does
        // not: the cycle is closed after zero iterations, not merely convergent.
        for _ in 0..2 {
            let report = run_cron(&pool).await;
            assert_eq!(report["sync"]["examined"], 0, "report: {report}");
        }
    });
}

/// The same property one level down, and the reason it holds *today*: a
/// plugin's own `save-item` goes through `Item::update` rather than
/// `ItemService::update`, so it fires no taps. The loop has no edge to traverse.
///
/// This is a **pin on kernel behaviour**, not an endorsement of it: routing
/// `save-item` through `ItemService` is the obvious fix for the fact that plugin-
/// written Items are never embedded, and the day someone makes it, this test
/// fails and points at `DESIGN.md` Drift 3. The write-back's own discipline is
/// what keeps the loop terminating after that; the test above proves that half.
#[test]
fn the_plugins_own_save_item_fires_no_tap_which_is_why_the_sync_cannot_start_the_loop() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:07", Some("old-name"), "online").await;
        run_cron(&pool).await;

        // The daemon learns a better hostname, so the next pass *does* call
        // save-item with a new title.
        sqlx::query(
            "UPDATE ng_devices SET hostname = 'new-name', sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["refreshed"], 1, "report: {report}");

        // If save-item fired tap_item_update, the write-back would have run and
        // copied the new title into display_name. It did not.
        let display_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM ng_devices WHERE id = $1")
                .bind(device)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            display_name, None,
            "save-item dispatched tap_item_update — the kernel behaviour \
             DESIGN.md Drift 3 records has changed; the loop now has an edge, and \
             termination rests entirely on the write-back not raising sync_state"
        );
    });
}

/// Column discipline, direction two: a sync pass must not clobber the admin's
/// edits. It sends a title and no `fields` key, which `Item::update` reads as
/// "leave the fields alone" — the reason the sync needs no read-modify-write and
/// therefore no transaction it cannot have (`G-ITEM-NO-MERGE`, `G-DB-NO-TX`).
#[test]
fn a_sync_pass_does_not_clobber_the_admins_fields() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:08", Some("tablet"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item_id = item_id.unwrap();

        let author = any_user(&pool).await;
        items(&pool)
            .update(
                item_id,
                UpdateItem {
                    title: Some("Aurora's tablet".into()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_mac": "aa:bb:cc:00:00:08",
                        "field_notes": "bedtime device",
                        "field_notify": true,
                    })),
                    log: None,
                },
                &UserContext::authenticated(author, vec!["edit ng_device content".into()]),
            )
            .await
            .unwrap()
            .unwrap();

        // The daemon re-dirties the row. Because display_name now holds the
        // admin's title, the derived title is that title, so the pass skips.
        sqlx::query(
            "UPDATE ng_devices SET hostname = 'tablet-2', sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
        let report = run_cron(&pool).await;
        assert_eq!(
            report["sync"]["skipped"], 1,
            "a named device must not be re-titled from its hostname: {report}"
        );

        let (title, fields): (String, serde_json::Value) =
            sqlx::query_as("SELECT title, fields FROM item WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "Aurora's tablet");
        assert_eq!(fields["field_notes"], "bedtime device");
        assert_eq!(fields["field_notify"], true);
    });
}

/// The other half of the same guarantee, on the path that *does* write: a title
/// refresh must leave the fields alone too.
#[test]
fn a_title_refresh_leaves_the_items_fields_intact() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:09", None, "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item_id = item_id.unwrap();

        // An admin fills in notes but does not name the device.
        let author = any_user(&pool).await;
        items(&pool)
            .update(
                item_id,
                UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_mac": "aa:bb:cc:00:00:09",
                        "field_notes": "unidentified, watch this one",
                    })),
                    log: None,
                },
                &UserContext::authenticated(author, vec!["edit ng_device content".into()]),
            )
            .await
            .unwrap()
            .unwrap();

        // The daemon then resolves a hostname, so the title genuinely changes.
        sqlx::query("UPDATE ng_devices SET hostname = 'roku', sync_state = 'dirty' WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["refreshed"], 1, "report: {report}");

        let (title, fields): (String, serde_json::Value) =
            sqlx::query_as("SELECT title, fields FROM item WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "roku", "the derived title did not move");
        assert_eq!(
            fields["field_notes"], "unidentified, watch this one",
            "the refresh clobbered the admin's notes"
        );
    });
}

// ===========================================================================
// People
// ===========================================================================

#[test]
fn a_person_item_is_mirrored_into_ng_people_and_retired_on_delete() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let author = any_user(&pool).await;
        let user = UserContext::authenticated(
            author,
            vec![
                "create ng_person content".into(),
                "edit ng_person content".into(),
                "delete ng_person content".into(),
            ],
        );

        let person = items(&pool)
            .create(
                CreateItem {
                    item_type: PERSON_TYPE.into(),
                    title: "Jeremy".into(),
                    status: Some(1),
                    author_id: author,
                    fields: Some(serde_json::json!({
                        "field_notes": "household",
                        "field_notify_arrive": true,
                        "field_notify_depart": false,
                    })),
                    promote: Some(0),
                    sticky: Some(0),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &user,
            )
            .await
            .expect("create person");

        let row =
            sqlx::query("SELECT name, notes, notify_arrive FROM ng_people WHERE item_id = $1")
                .bind(person.id)
                .fetch_one(&pool)
                .await
                .expect("person was not mirrored");
        assert_eq!(row.get::<String, _>(0), "Jeremy");
        assert_eq!(
            row.get::<Option<String>, _>(1).as_deref(),
            Some("household")
        );
        assert!(row.get::<bool, _>(2));

        // A device owned by them.
        let device = seed_device(&pool, "aa:bb:cc:00:00:0a", Some("phone"), "online").await;
        sqlx::query("UPDATE ng_devices SET owner_item_id = $1 WHERE id = $2")
            .bind(person.id)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();

        items(&pool).delete(person.id, &user).await.expect("delete");

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ng_people WHERE item_id = $1")
                .bind(person.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0, "the mirror row outlived its Item");

        // The device is unassigned, not deleted: it is still on the network.
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT owner_item_id FROM ng_devices WHERE id = $1")
                .bind(device)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            owner, None,
            "a deleted person left a dangling owner, which the by-owner gather would surface"
        );
        let still_there: i64 = sqlx::query_scalar("SELECT count(*) FROM ng_devices WHERE id = $1")
            .bind(device)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(still_there, 1);
    });
}

/// Deleting a device Item means "forget my edits and start over", not "stop
/// tracking this device" — the device is on the network either way.
#[test]
fn deleting_a_device_item_unlinks_the_row_and_the_next_pass_mints_a_replacement() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:0b", Some("speaker"), "online").await;
        run_cron(&pool).await;
        let (first_item, _) = link_of(&pool, device).await;

        let author = any_user(&pool).await;
        items(&pool)
            .delete(
                first_item.unwrap(),
                &UserContext::authenticated(author, vec!["delete ng_device content".into()]),
            )
            .await
            .expect("delete device item");

        let (link, sync_state) = link_of(&pool, device).await;
        assert_eq!(link, None, "the row still points at a deleted Item");
        assert_eq!(
            sync_state, "dirty",
            "the row was not queued for a fresh Item"
        );

        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["created"], 1, "report: {report}");
        let (relinked, _) = link_of(&pool, device).await;
        assert!(relinked.is_some());
        assert_ne!(relinked, first_item);
    });
}

// ===========================================================================
// Retention
// ===========================================================================

#[test]
fn events_older_than_the_retention_window_are_pruned_and_newer_ones_are_not() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:0c", Some("nvr"), "online").await;
        let now = now();

        for (event_type, age_days) in [
            ("device_seen", 1),
            ("device_seen", 89),
            // Just past the 90-day default.
            ("device_seen", 91),
            ("mac_spoof", 200),
        ] {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details) \
                 VALUES ($1, $2, to_timestamp($3), '{\"note\": \"x\"}'::jsonb)",
            )
            .bind(device)
            .bind(event_type)
            .bind((now - age_days * 86_400) as f64)
            .execute(&pool)
            .await
            .unwrap();
        }

        let report = run_cron(&pool).await;
        assert_eq!(report["pruned"], 2, "report: {report}");

        let remaining: Vec<i64> = sqlx::query_scalar(
            "SELECT timestamp_epoch FROM ng_events ORDER BY timestamp_epoch DESC",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(remaining.len(), 2);
        for ts in remaining {
            assert!(
                now - ts < 90 * 86_400,
                "an event older than the window survived"
            );
        }
    });
}

/// Pruning must not be a function of how many events happen to be old: a second
/// pass over an already-pruned log deletes nothing.
#[test]
fn a_second_retention_pass_over_a_pruned_log_deletes_nothing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:0d", None, "online").await;
        sqlx::query(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
             VALUES ($1, 'device_seen', to_timestamp($2))",
        )
        .bind(device)
        .bind((now() - 200 * 86_400) as f64)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(run_cron(&pool).await["pruned"], 1);
        assert_eq!(run_cron(&pool).await["pruned"], 0);
    });
}

// ===========================================================================
// Gathers
// ===========================================================================

/// A gather over the device record type, through the real `GatherService`.
/// The online list is the front page and the tile whose pager count is "how many
/// devices are online", so it has to actually filter.
#[test]
fn the_online_device_gather_returns_only_online_unhidden_devices() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_device(&pool, "aa:bb:cc:00:00:10", Some("on-1"), "online").await;
        seed_device(&pool, "aa:bb:cc:00:00:11", Some("on-2"), "online").await;
        seed_device(&pool, "aa:bb:cc:00:00:12", Some("off-1"), "offline").await;
        let hidden = seed_device(&pool, "aa:bb:cc:00:00:13", Some("hidden-1"), "online").await;
        sqlx::query("UPDATE ng_devices SET hidden = true WHERE id = $1")
            .bind(hidden)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_device_online", HashMap::new()).await;
        assert_eq!(
            rows.len(),
            2,
            "online gather returned {} rows, expected 2",
            rows.len()
        );

        // That the gather ran at all is the sort assertion: its definition sorts
        // on the logical field `last_seen`, which the record field map resolves
        // to `last_seen_at_epoch`. A field map naming a column that does not
        // exist fails here rather than returning unsorted rows.
        //
        // What a row carries is the backing relation's **physical** columns —
        // the gather wraps the query in Postgres' own `row_to_json`.
        let row = &rows[0];
        assert!(
            row["last_seen_at_epoch"].is_i64(),
            "the epoch twin did not render as an integer: {row}"
        );
        assert!(row["first_seen_at_epoch"].is_i64(), "{row}");
        assert!(row["mac"].is_string());

        // And it carries ONLY the twins, never the `timestamptz` columns they are
        // generated from. That relation is `ng_devices_with_owner`, the view the
        // `ng_device_state` record type is declared over, which selects the record
        // type's mapped columns and no others. Deliberate: a column that renders
        // as an ISO 8601 string on this path and as `null` through the structured
        // `db` host is a trap for a template, and keeping it off the row is the
        // strongest available form of "do not reach for this".
        assert!(
            row["last_seen_at"].is_null(),
            "the owner view started exposing the raw timestamptz again: {row}"
        );

        // The kernel behaviour that made the twins necessary in the first place is
        // still pinned, just against the daemon's table rather than the view over
        // it: `row_to_json` renders a `timestamptz` as an ISO 8601 string, while
        // the structured `db` host decodes the same column as `null`
        // (`G-DB-HOST-TYPE-COVERAGE`). The day that stops being true, this fails
        // and the twins are worth reconsidering.
        let raw: serde_json::Value = sqlx::query_scalar(
            "SELECT row_to_json(t) FROM (SELECT last_seen_at FROM ng_devices LIMIT 1) t",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            raw["last_seen_at"].is_string(),
            "a timestamptz stopped rendering as an ISO string through row_to_json: {raw}"
        );
    });
}

/// The event gathers are the retention-bounded, high-volume path, and the
/// security view is the one the tile points at.
#[test]
fn the_event_gathers_return_the_log_and_the_security_subset() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:14", None, "online").await;
        // Daemon event-type strings, from `EventType::as_str`. Two routine
        // (`name_updated`, `new_device` — the latter is a real event the daemon
        // does not class as security relevant) and two security ones.
        //
        // This seed used to read ["device_seen", "device_seen", "mac_spoof",
        // "device_new"], and only one of those four is a name the daemon can
        // write. The test still passed: it seeded the gather's own stale
        // vocabulary, so the filter matched the fixture exactly while matching
        // nothing on a real database.
        for event_type in ["name_updated", "new_device", "arp_spoof", "ip_conflict"] {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
                 VALUES ($1, $2, to_timestamp($3))",
            )
            .bind(device)
            .bind(event_type)
            .bind(now() as f64)
            .execute(&pool)
            .await
            .unwrap();
        }

        let gather = wire_gather(&pool).await;
        assert_eq!(
            run_gather(&gather, &pool, "ng_event_log", HashMap::new()).await,
            4
        );
        assert_eq!(
            run_gather(&gather, &pool, "ng_event_security", HashMap::new()).await,
            2,
            "the security gather did not select exactly the security event types"
        );
    });
}

/// The facet routes carry their value in a URL argument, because an exposed
/// filter left blank binds `''` against a uuid column and raises
/// (`G-EXPOSED-FILTER-NO-MATCH-ALL`). This asserts the by-owner route works with
/// its argument supplied — which is the only way it is ever reached.
#[test]
fn the_by_owner_facet_route_filters_on_its_url_argument() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO ng_people (item_id, name) VALUES ($1, 'Jeremy')")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let mine = seed_device(&pool, "aa:bb:cc:00:00:15", Some("mine"), "online").await;
        seed_device(&pool, "aa:bb:cc:00:00:16", Some("theirs"), "online").await;
        sqlx::query("UPDATE ng_devices SET owner_item_id = $1 WHERE id = $2")
            .bind(owner)
            .bind(mine)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let args = HashMap::from([("owner".to_string(), owner.to_string())]);
        assert_eq!(
            run_gather(&gather, &pool, "ng_device_by_owner", args).await,
            1
        );
    });
}

/// Wire a standalone `GatherService` with the plugin's record types admitted and
/// the migration-seeded queries loaded, the way the running kernel wires it.
async fn wire_gather(pool: &PgPool) -> Arc<GatherService> {
    let disp = dispatcher();
    let compiled = disp.runtime().get_plugin(PLUGIN).expect("plugin loaded");
    let content_names: HashSet<String> = [DEVICE_TYPE.to_string(), PERSON_TYPE.to_string()].into();
    let (registry, errors) = RecordTypeRegistry::build(
        [(
            PLUGIN,
            compiled.db_policy().as_ref(),
            compiled.info.record_types.as_slice(),
        )],
        &content_names,
    );
    assert!(errors.is_empty(), "record types rejected: {errors:?}");

    let categories = CategoryService::new(pool.clone(), Duration::from_secs(60));
    let gather = GatherService::new(
        pool.clone(),
        categories,
        Arc::new(GatherExtensionRegistry::new()),
        trovato_kernel::gather::GatherConfig {
            ttl: Duration::from_secs(60),
            max_page_size: 100,
            access: trovato_kernel::gather::GatherAccessConfig::default(),
        },
        None,
        None,
    );
    gather.set_item_service(items(pool));
    gather.set_record_types(Arc::new(registry));
    gather.load_queries().await.expect("load gather queries");
    gather
}

/// Execute a seeded gather by id and return its row count.
///
/// No exposed filters are ever passed, because the plugin seeds none — every
/// facet is a URL argument instead (`G-EXPOSED-FILTER-NO-MATCH-ALL`,
/// `DESIGN.md` Decision 6).
async fn run_gather(
    gather: &GatherService,
    _pool: &PgPool,
    query_id: &str,
    url_args: HashMap<String, String>,
) -> usize {
    gather_items(gather, query_id, url_args).await.len()
}

/// The rows a seeded gather returns, for the assertions that are about what a
/// row contains rather than how many there are.
async fn gather_items(
    gather: &GatherService,
    query_id: &str,
    url_args: HashMap<String, String>,
) -> Vec<serde_json::Value> {
    let context = QueryContext {
        url_args,
        ..QueryContext::default()
    };
    gather
        .execute(
            query_id,
            1,
            HashMap::new(),
            Uuid::parse_str(LIVE_STAGE).unwrap(),
            &context,
        )
        .await
        .unwrap_or_else(|e| panic!("gather {query_id} failed: {e:#}"))
        .items
}

// ===========================================================================
// Permissions
// ===========================================================================

/// The read-only role must actually be read-only. `network_viewer` is seeded with
/// `view` and no `edit`, and `ItemService::update` refuses without it — so a
/// viewer cannot reach the write-back at all, and the daemon's row is safe from
/// them by the same check that guards the Item.
#[test]
fn a_viewer_cannot_edit_a_device_and_therefore_cannot_reach_the_write_back() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:20", Some("shared-nas"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item_id = item_id.unwrap();
        let before = daemon_snapshot(&pool, device).await;

        // The permissions the seeded network_viewer role actually holds.
        let viewer_perms = role_permissions(&pool, "network_viewer").await;
        assert!(
            viewer_perms.contains("view ng_device content"),
            "network_viewer cannot even read: {viewer_perms:?}"
        );
        assert!(
            !viewer_perms.contains("edit ng_device content"),
            "network_viewer holds an edit permission and is not read-only"
        );

        let viewer = UserContext::authenticated(
            any_user(&pool).await,
            viewer_perms.iter().cloned().collect(),
        );
        let refused = items(&pool)
            .update(
                item_id,
                UpdateItem {
                    title: Some("viewer tried to rename this".into()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: None,
                },
                &viewer,
            )
            .await;
        assert!(refused.is_err(), "a viewer was allowed to edit a device");

        // Nothing reached the daemon's table: not the user columns, not the
        // daemon columns.
        assert_eq!(daemon_snapshot(&pool, device).await, before);
        let display_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM ng_devices WHERE id = $1")
                .bind(device)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            display_name, None,
            "a refused edit still reached the write-back"
        );
    });
}

/// The admin role must hold exactly the permission strings the kernel checks —
/// a seeded permission the kernel never looks at grants nothing.
#[test]
fn the_network_admin_role_holds_the_permission_strings_tap_perm_declares() {
    serial(async {
        let pool = fresh_pool().await;
        let granted = role_permissions(&pool, "network_admin").await;
        for expected in [
            "administer netgrasp",
            "view netgrasp devices",
            "edit ng_device content",
            "delete ng_device content",
            "create ng_person content",
            "edit ng_person content",
        ] {
            assert!(
                granted.contains(expected),
                "network_admin lacks '{expected}'"
            );
        }

        // And every string it holds for an ng_ type is one tap_perm declares, so
        // a typo in the migration cannot ship as a silently inert grant.
        let declared: HashSet<String> = declared_permissions(&pool).await;
        for held in &granted {
            if held.contains("ng_device") || held.contains("ng_person") {
                assert!(
                    declared.contains(held),
                    "the migration grants '{held}', which tap_perm does not declare"
                );
            }
        }
    });
}

/// Permission strings held by a seeded role.
async fn role_permissions(pool: &PgPool, role: &str) -> HashSet<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT rp.permission FROM role_permissions rp \
         JOIN roles r ON r.id = rp.role_id WHERE r.name = $1",
    )
    .bind(role)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .collect()
}

/// The permission names `tap_perm` declares, read back from the live plugin.
async fn declared_permissions(pool: &PgPool) -> HashSet<String> {
    let results = dispatcher()
        .dispatch("tap_perm", "{}", background(pool))
        .await;
    let raw: serde_json::Value = serde_json::from_str(&results[0].output).unwrap();
    raw.as_array()
        .expect("tap_perm returns an array")
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect()
}

// ===========================================================================
// The device page
// ===========================================================================

/// The plugin's real UI work: presence, location and address timelines rendered
/// over three daemon tables by `tap_item_view`, since no other surface exists
/// for it (`G-NO-PLUGIN-HTTP`).
#[test]
fn the_device_page_renders_the_daemons_timelines() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:21", Some("jeremys-phone"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item_id = item_id.unwrap();
        let now = now();

        // A closed presence session, an open one, and a compacted summary row
        // the page must leave out: a summary is a day, not a session.
        for (start, end, is_summary) in [
            (now - 8_000, Some(now - 4_000), false),
            (now - 600, None, false),
            (now - 200_000, Some(now - 190_000), true),
        ] {
            sqlx::query(
                "INSERT INTO ng_presence (device_id, started_at, ended_at, is_summary) \
                 VALUES ($1, to_timestamp($2), to_timestamp($3), $4)",
            )
            .bind(device)
            .bind(start as f64)
            .bind(end.map(|e| e as f64))
            .bind(is_summary)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO ng_location_history (device_id, ap_name, location, started_at, ended_at) \
             VALUES ($1, 'ap-1', 'living-room-ap', to_timestamp($2), NULL)",
        )
        .bind(device)
        .bind((now - 600) as f64)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ng_ip_history (device_id, ip, interface, first_seen, last_seen) \
             VALUES ($1, '192.168.1.42', 'eth0', to_timestamp($2), to_timestamp($3))",
        )
        .bind(device)
        .bind((now - 8_000) as f64)
        .bind((now - 600) as f64)
        .execute(&pool)
        .await
        .unwrap();

        let item: serde_json::Value = item_json(&pool, item_id).await;
        let results = dispatcher()
            .dispatch("tap_item_view", &item.to_string(), background(&pool))
            .await;
        assert_eq!(results.len(), 1);

        // Decode, because the kernel appends the JSON-serialized form to the page
        // verbatim (`G-VIEW-OUTPUT-JSON-ENCODED`). The pin below asserts that.
        let html: String = serde_json::from_str(&results[0].output)
            .unwrap_or_else(|e| panic!("view output was not a JSON string ({e})"));

        for expected in [
            "aa:bb:cc:00:00:21",
            "jeremys-phone",
            "living-room-ap",
            "192.168.1.42",
            "ng-device__timeline--presence",
            "ng-device__timeline--location",
            "ng-device__timeline--address",
            // The open session, marked.
            "(ongoing)",
            // Two sessions across the presence table, and only two: the third
            // row is a compacted summary, which is a day rather than a session
            // and would also render as a permanent "(ongoing)" if it were let
            // through (its ended_at is outside the daemon's open-row index).
            "2 sessions",
            &format!("/events/device?device={device}"),
        ] {
            assert!(
                html.contains(expected),
                "device page is missing {expected:?}"
            );
        }
    });
}

/// The pin on `G-VIEW-OUTPUT-JSON-ENCODED`, inherited from Argus M3: the kernel
/// appends a view tap's **JSON-serialized** return value to the page without
/// decoding it, so a fragment containing a `"` would reach a browser with
/// backslashes in it. The plugin's mitigation is to emit no such character; this
/// asserts both halves, so the day the kernel decodes, a test says so.
#[test]
fn the_device_pages_view_output_is_json_encoded_by_the_contract() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:22", Some("printer"), "online").await;
        run_cron(&pool).await;
        let (item_id, _) = link_of(&pool, device).await;
        let item = item_json(&pool, item_id.unwrap()).await;

        let results = dispatcher()
            .dispatch("tap_item_view", &item.to_string(), background(&pool))
            .await;
        let raw = &results[0].output;

        // The contract as it stands: the output is a JSON string literal.
        assert!(
            raw.starts_with('"') && raw.ends_with('"'),
            "view output is no longer JSON-encoded — G-VIEW-OUTPUT-JSON-ENCODED may be fixed"
        );
        // The mitigation: the fragment inside carries no escape, so the round
        // trip damages nothing but the wrapping quotes.
        assert!(
            !raw[1..raw.len() - 1].contains('\\'),
            "the fragment picked up a serde escape, which reaches the page as literal text"
        );
    });
}

/// A device Item with no daemon row behind it must say so rather than render
/// empty timelines that read as "never seen".
#[test]
fn a_device_item_with_no_daemon_row_says_so() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let author = any_user(&pool).await;
        let orphan = items(&pool)
            .create(
                CreateItem {
                    item_type: DEVICE_TYPE.into(),
                    title: "hand-made".into(),
                    status: Some(1),
                    author_id: author,
                    fields: Some(serde_json::json!({"field_mac": "aa:bb:cc:00:00:23"})),
                    promote: Some(0),
                    sticky: Some(0),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &UserContext::authenticated(author, vec!["create ng_device content".into()]),
            )
            .await
            .unwrap();

        let item = item_json(&pool, orphan.id).await;
        let results = dispatcher()
            .dispatch("tap_item_view", &item.to_string(), background(&pool))
            .await;
        let html: String = serde_json::from_str(&results[0].output).unwrap();
        assert!(html.contains("ng-device--unlinked"));
        assert!(!html.contains("ng-device__timeline"));
    });
}

/// The Item as the kernel serializes it for a view tap.
async fn item_json(pool: &PgPool, item_id: Uuid) -> serde_json::Value {
    let (item_type, title, fields): (String, String, serde_json::Value) =
        sqlx::query_as("SELECT type, title, fields FROM item WHERE id = $1")
            .bind(item_id)
            .fetch_one(pool)
            .await
            .unwrap();
    serde_json::json!({
        "id": item_id.to_string(),
        "type": item_type,
        "title": title,
        "fields": fields,
    })
}
