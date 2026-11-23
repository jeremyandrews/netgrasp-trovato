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
    CategoryService, FilterValue, GatherExtensionRegistry, GatherService, QueryContext,
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
    KEEP_ITEMS.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.as_str()))
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply {migration}: {e}"));
    }
    ContentTypeRegistry::new(pool.clone(), Duration::from_secs(60), metrics())
        .sync_from_plugins(&dispatcher())
        .await
        .expect("register netgrasp content types");
    pool
}

async fn reset(pool: &PgPool) {
    for stmt in [
        // The people tests inject failures with triggers. Disarmed first, so a
        // test that panicked cannot make this reset, or the next test, fail.
        "DROP TRIGGER IF EXISTS u2_fail_device ON ng_devices",
        "DROP TRIGGER IF EXISTS u2_fail_item ON item",
        "DELETE FROM item WHERE type IN ('ng_device', 'ng_person')",
        // One statement, because the timeline and event tables carry foreign
        // keys onto ng_devices now: truncating it alone is refused.
        "TRUNCATE ng_devices, ng_presence, ng_location_history, ng_ip_history, ng_events CASCADE",
        "TRUNCATE ng_people",
        "TRUNCATE ng_state",
        "TRUNCATE ng_person_merge",
        "TRUNCATE ng_notify_status, ng_notify_requests RESTART IDENTITY",
        // The presence tests stand a daemon status row up; nothing else has one.
        "DROP TABLE IF EXISTS ng_daemon_status",
    ] {
        sqlx::query(stmt).execute(pool).await.unwrap();
    }
}

/// The kernel's metrics registry, which its services now take. A fresh one per
/// service is what the kernel's own tests do; nothing here reads it.
fn metrics() -> Arc<trovato_kernel::metrics::Metrics> {
    Arc::new(trovato_kernel::metrics::Metrics::default())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs() as i64
}

fn background(pool: &PgPool) -> RequestState {
    RequestState::new(UserContext::background(), services(pool))
}

/// Request services wired the way `AppState` wires them: the plugin runtime,
/// and an `ItemService` bound late for the `item-api` host calls. Without that
/// binding every `save-item` answers `ERR_NO_SERVICES` (-10).
///
/// The services hold the item service weakly (it holds them in turn), so the
/// strong reference is kept in [`KEEP_ITEMS`] for the life of the test binary.
fn services(pool: &PgPool) -> RequestServices {
    let disp = dispatcher();
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone());
    let items = Arc::new(ItemService::new(
        pool.clone(),
        disp,
        services.clone(),
        Duration::from_secs(60),
        None,
        None,
        metrics(),
    ));
    services.set_item_service(&items);
    KEEP_ITEMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(items);
    services
}

/// The strong ends of the item services [`services`] binds. Emptied by
/// [`fresh_pool`], which every test calls first and which runs one test at a
/// time, so each test's pool is released when the next test starts rather than
/// held open for the life of the binary.
static KEEP_ITEMS: LazyLock<Mutex<Vec<Arc<ItemService>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// An `ItemService` wired to the real dispatcher, so `update` fires
/// `tap_item_update` exactly as the admin content route does.
fn items(pool: &PgPool) -> Arc<ItemService> {
    let disp = dispatcher();
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone());
    let items = Arc::new(ItemService::new(
        pool.clone(),
        disp,
        services.clone(),
        Duration::from_secs(60),
        None,
        None,
        metrics(),
    ));
    services.set_item_service(&items);
    items
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
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {cols} FROM ng_devices WHERE id = $1"
    )))
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
            "ng_person_presence",
            "ng_person_movement",
            "ng_device_new",
            "ng_overview",
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
            "ng_people_presence",
            "ng_person_movements",
            "ng_devices_new",
            "ng_overview",
            "ng_devices_listed",
            "ng_devices_new_listed",
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                "publish content".into(),
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

// ===========================================================================
// The overview
// ===========================================================================

/// Seed the house the overview tests read: three people, two of them home; a
/// day of arrivals and departures with yesterday's behind it; devices old, new
/// and hidden; and security events inside and outside the last day.
///
/// "Today" is the database's calendar day, so today's rows are placed just after
/// its midnight rather than a few minutes before now: a test that ran at 00:02
/// would otherwise seed half of "today" into yesterday.
async fn seed_house(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let jamie = Uuid::now_v7();
    let aurora = Uuid::now_v7();
    let arlo = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ng_people (item_id, name, state, current_location, last_arrived_at, last_departed_at) VALUES \
            ($1, 'Jamie',  'home', 'Studio', date_trunc('day', now()) + interval '10 seconds', NULL), \
            ($2, 'Aurora', 'home', NULL,     date_trunc('day', now()) + interval '5 seconds',  NULL), \
            ($3, 'Arlo',   'away', NULL,     now() - interval '3 days', date_trunc('day', now()) + interval '20 seconds')",
    )
    .bind(jamie)
    .bind(aurora)
    .bind(arlo)
    .execute(pool)
    .await
    .unwrap();

    // Jamie's phone: old, online, owned. The device the movements name.
    let phone = seed_device(pool, "aa:bb:cc:00:01:01", Some("jamie-phone"), "online").await;
    // A device first seen a year ago, and one first seen yesterday that is
    // hidden: neither is "new" on the page.
    let old = seed_device(pool, "aa:bb:cc:00:01:02", Some("old-nas"), "online").await;
    let hidden = seed_device(pool, "aa:bb:cc:00:01:03", Some("hidden-new"), "online").await;
    // Two genuinely new ones, one of which the daemon has identified.
    let fresh = seed_device(pool, "aa:bb:cc:00:01:04", None, "online").await;
    let unknown = seed_device(pool, "aa:bb:cc:00:01:05", None, "offline").await;
    sqlx::query(
        "UPDATE ng_devices SET owner_item_id = $1, first_seen_at = now() - interval '300 days' WHERE id = $2",
    )
    .bind(jamie)
    .bind(phone)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ng_devices SET first_seen_at = now() - interval '400 days' WHERE id = $1")
        .bind(old)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ng_devices SET hidden = true, first_seen_at = now() - interval '1 day' WHERE id = $1",
    )
    .bind(hidden)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ng_devices SET device_type = 'phone', device_type_confidence = 0.92, \
             first_seen_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(fresh)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ng_devices SET device_type = NULL, device_type_confidence = NULL, os_family = NULL, \
             vendor = NULL, first_seen_at = now() - interval '6 days' WHERE id = $1",
    )
    .bind(unknown)
    .execute(pool)
    .await
    .unwrap();

    // Movements, written the way the daemon writes them (src/people/mod.rs):
    // the person's name and item id and the device in `details`.
    for (event_type, person, name, at, details) in [
        // Yesterday's departure: not on today's list.
        (
            "person_departed",
            jamie,
            "Jamie",
            "date_trunc('day', now()) - interval '2 hours'",
            r#"{"via": "Driveway"}"#,
        ),
        // Today, deliberately inserted out of order.
        (
            "person_arrived",
            jamie,
            "Jamie",
            "date_trunc('day', now()) + interval '10 seconds'",
            r#"{"location": "Studio", "via": "Driveway"}"#,
        ),
        (
            "person_arrived",
            aurora,
            "Aurora",
            "date_trunc('day', now()) + interval '5 seconds'",
            r#"{"location": null, "via": null}"#,
        ),
        (
            "person_departed",
            arlo,
            "Arlo",
            "date_trunc('day', now()) + interval '20 seconds'",
            r#"{"via": "Gate"}"#,
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details, person_item_id) \
             VALUES ($1, $2, {at}, $3::jsonb || jsonb_build_object('person', $4::text, 'person_item_id', $5::text, 'device', 'aa:bb:cc:00:01:01'), $5::uuid)"
        )))
        .bind(phone)
        .bind(event_type)
        .bind(details)
        .bind(name)
        .bind(person.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    // Security: two in the last day, one a week ago, and an ordinary event that
    // is not security at all.
    for (event_type, ago) in [
        ("arp_spoof", "1 hour"),
        ("ip_conflict", "2 hours"),
        ("arp_scan", "7 days"),
        ("name_updated", "1 hour"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
             VALUES ($1, $2, now() - interval '{ago}')"
        )))
        .bind(fresh)
        .bind(event_type)
        .execute(pool)
        .await
        .unwrap();
    }
    (jamie, aurora, arlo)
}

/// Render a gather's page template the way the kernel's gather route does:
/// the template the query id suggests, in the context
/// `render_gather_with_theme` builds (`crates/kernel/src/routes/gather.rs`),
/// from a result the real `GatherService` returned.
///
/// The kernel's own render falls back to a dump of every column when the
/// template raises, and the page still returns 200; so a template error is only
/// ever visible to something that renders the template itself.
fn render_page(query_id: &str, label: &str, base_path: &str, rows: &[serde_json::Value]) -> String {
    let dir = plugin_source_dir().join("../../templates/**/*.html");
    let tera = tera::Tera::new(&dir.to_string_lossy()).expect("the templates parse");
    let mut context = tera::Context::new();
    context.insert(
        "query",
        &serde_json::json!({"query_id": query_id, "label": label}),
    );
    context.insert("rows", rows);
    context.insert("total", &rows.len());
    context.insert("page", &1);
    context.insert("per_page", &50);
    context.insert("total_pages", &1);
    context.insert("has_next", &false);
    context.insert("has_prev", &false);
    context.insert("base_path", base_path);
    context.insert("exposed_filters", &serde_json::json!([]));
    context.insert("filter_values", &serde_json::json!({}));
    tera.render(&format!("gather/query--{query_id}.html"), &context)
        .unwrap_or_else(|e| panic!("{query_id} failed to render: {e:#?}"))
}

/// Every view the overview's record types are declared over carries every
/// column the record type maps.
///
/// Asked of Postgres rather than of the migration's text: a field map naming a
/// column the view does not have is admitted by the kernel and then fails the
/// first gather that filters or sorts on it, with a SQL error on a page that
/// looked fine in review.
#[test]
fn every_overview_view_carries_every_column_its_record_type_maps() {
    serial(async {
        let pool = fresh_pool().await;
        let compiled = dispatcher().runtime().get_plugin(PLUGIN).unwrap();
        let mut checked = 0;
        for record_type in &compiled.info.record_types {
            if ![
                "ng_person_presence",
                "ng_person_movement",
                "ng_device_new",
                "ng_overview",
                "ng_device_state",
            ]
            .contains(&record_type.name.as_str())
            {
                continue;
            }
            let columns: HashSet<String> = sqlx::query_scalar(
                "SELECT column_name::text FROM information_schema.columns WHERE table_name = $1",
            )
            .bind(&record_type.table)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .collect();
            assert!(!columns.is_empty(), "{} does not exist", record_type.table);
            for column in record_type.fields.values().chain([
                &record_type.id_column,
                &record_type.title_column,
                &record_type.created_column,
                &record_type.changed_column,
            ]) {
                assert!(
                    columns.contains(column.as_str()),
                    "{} maps {column}, which {} does not have",
                    record_type.name,
                    record_type.table
                );
            }
            checked += 1;
        }
        assert_eq!(
            checked, 5,
            "not every overview or device record type was checked"
        );
    });
}

/// An include's name is the key its rows land under on the parent row, and the
/// kernel inserts it over whatever was there. An include named after one of the
/// overview's columns would replace a count with a list.
#[test]
fn no_overview_include_is_named_after_a_column_it_would_overwrite() {
    serial(async {
        let pool = fresh_pool().await;
        let includes: serde_json::Value = sqlx::query_scalar(
            "SELECT definition -> 'includes' FROM gather_query WHERE query_id = 'ng_overview'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        // Both relations: the overview view and Home's view over it, which is
        // what the record type now reads and so what the includes land on.
        let columns: HashSet<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_name IN ('ng_overview', 'ng_home', 'ng_home_presence')",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
        let names: Vec<&String> = includes.as_object().unwrap().keys().collect();
        // 023 added `untracked`, the people nobody is tracking.
        assert_eq!(names.len(), 6, "{includes}");
        for name in names {
            assert!(
                !columns.contains(name),
                "the include {name} would overwrite the overview's {name} column"
            );
        }
    });
}

/// Each list on the overview is also a page of its own, and the two copies of
/// its definition must agree: same record type, and the same filters apart from
/// the one the include's join supplies. The sorts are allowed to differ, and
/// only where 008 says they do.
#[test]
fn each_overview_list_agrees_with_the_page_it_links_to() {
    serial(async {
        let pool = fresh_pool().await;
        let definition = |query_id: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, serde_json::Value>(
                    "SELECT definition FROM gather_query WHERE query_id = $1",
                )
                .bind(query_id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let overview = definition("ng_overview").await;
        for (include, standalone, same_sort) in [
            ("home", "ng_people_home", true),
            ("movements", "ng_person_movements", false),
            ("new_devices", "ng_devices_new", true),
        ] {
            let inc = &overview["includes"][include];
            let child = &inc["definition"];
            let page = definition(standalone).await;
            assert_eq!(child["record_type"], page["record_type"], "{include}");

            let join = inc["child_field"].as_str().unwrap();
            let without_join = |filters: &serde_json::Value| -> Vec<serde_json::Value> {
                filters
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|f| f["field"] != join)
                    .cloned()
                    .collect()
            };
            assert_eq!(
                without_join(&child["filters"]),
                without_join(&page["filters"]),
                "the overview's {include} and {standalone} filter differently"
            );
            if same_sort {
                assert_eq!(child["sorts"], page["sorts"], "{include}");
            } else {
                assert_ne!(child["sorts"], page["sorts"], "{include}");
            }
        }

        // Home's "Needs attention" is /devices/new's definition plus exactly
        // the To sort tab's two conditions, and at most six rows.
        let attention = &overview["includes"]["attention"];
        let new = definition("ng_devices_new").await;
        let todo = definition("ng_devices_todo").await;
        assert_eq!(attention["definition"]["record_type"], new["record_type"]);
        assert_eq!(attention["definition"]["sorts"], new["sorts"]);
        let mut expected: Vec<serde_json::Value> = new["filters"].as_array().unwrap().clone();
        expected.extend(
            todo["filters"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| f["operator"] == "is_null")
                .cloned(),
        );
        assert_eq!(
            attention["definition"]["filters"].as_array().unwrap(),
            &expected,
            "Needs attention is not New this week and To sort"
        );
        assert_eq!(attention["display"]["items_per_page"], 6);
        // Who is away reads the same people, by the other state.
        assert_eq!(
            overview["includes"]["away"]["definition"]["record_type"],
            overview["includes"]["home"]["definition"]["record_type"]
        );
    });
}

/// **The overview gather, end to end**: one row, the right counts, each list
/// holding exactly its rows in its order, and the page rendering all of it.
#[test]
fn the_overview_counts_and_lists_what_the_house_is_doing_today() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;

        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        assert_eq!(rows.len(), 1, "the overview is one row: {rows:?}");
        let ov = &rows[0];

        assert_eq!(ov["people_home"], 2, "{ov}");
        assert_eq!(ov["people_total"], 3, "{ov}");
        assert_eq!(
            ov["devices_new"], 2,
            "hidden and year-old devices are not new: {ov}"
        );
        assert_eq!(ov["movements_today"], 3, "{ov}");
        assert_eq!(
            ov["security_events"], 3,
            "a non-security event was counted: {ov}"
        );
        assert_eq!(ov["security_events_24h"], 2, "{ov}");

        // The includes: attached, filled, and in the order the page promises.
        let names = |list: &serde_json::Value, key: &str| -> Vec<String> {
            list.as_array()
                .unwrap_or_else(|| panic!("the include is not a list: {list}"))
                .iter()
                .map(|r| r[key].as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(
            names(&ov["home"], "name"),
            ["Aurora", "Jamie"],
            "home is everyone home, earliest arrival first"
        );
        assert_eq!(
            names(&ov["movements"], "person_name"),
            ["Aurora", "Jamie", "Arlo"],
            "today's movements, oldest first, and not yesterday's"
        );
        assert_eq!(
            names(&ov["new_devices"], "mac"),
            ["aa:bb:cc:00:01:04", "aa:bb:cc:00:01:05"],
            "new devices, newest first, without the hidden one"
        );
        // The arrival time reached the row as an integer, which is the one
        // thing ng_people could not give it.
        assert!(ov["home"][0]["last_arrived_at_epoch"].is_i64(), "{ov}");

        // Home's own counts and lists (013): who is out, what is unsorted (the
        // year-old NAS too, but not the hidden device nor the owned phone),
        // what of that is new, and the actionable security events by type.
        assert_eq!(names(&ov["away"], "name"), ["Arlo"], "{ov}");
        assert_eq!(ov["devices_unsorted"], 3, "{ov}");
        assert_eq!(
            names(&ov["attention"], "mac"),
            ["aa:bb:cc:00:01:04", "aa:bb:cc:00:01:05"],
            "new and unsorted, newest first"
        );
        assert_eq!(ov["arp_spoof_24h"], 1, "{ov}");
        assert_eq!(ov["ip_conflict_24h"], 1, "{ov}");
        assert_eq!(ov["rogue_dhcp_24h"], 0, "{ov}");

        let html = render_page("ng_overview", "Overview", "/overview", &rows);
        for expected in [
            "Aurora",
            "home since",
            "Away:",
            "1 ARP spoofing warning today",
            "1 address conflict today",
            "1 more device has no owner.",
            "via Driveway",
            "via Gate",
            "Left",
            "92%",
            "Not yet identified",
            "aa:bb:cc:00:01:05",
            "ng-menu__button",
            "href=\"/events/security\"",
            "ng-attention__alert",
        ] {
            assert!(
                html.contains(expected),
                "the overview lost {expected:?}: {html}"
            );
        }
        assert!(!html.contains("rogue DHCP"), "{html}");
    });
}

/// The overview's three listings as pages of their own, and each page rendered.
#[test]
fn the_overviews_listings_are_pages_that_filter_and_render_on_their_own() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let gather = wire_gather(&pool).await;

        // Since 023 the page lists everybody: home first, then away, then
        // anybody nobody is tracking, who is never counted as either.
        let home = gather_items(&gather, "ng_people_home", HashMap::new()).await;
        let states: Vec<(&str, &str)> = home
            .iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap_or_default(),
                    r["state"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            states,
            [("Aurora", "home"), ("Jamie", "home"), ("Arlo", "away")],
            "home first, earliest arrival first, then away: {home:?}"
        );
        let page = render_page("ng_people_home", "Home now", "/people/home", &home);
        assert!(page.contains("Jamie"), "{page}");
        assert!(page.contains("Away"), "{page}");
        assert!(!page.contains("Not tracked"), "{page}");

        // The whole log, newest first, and one day of it by URL argument.
        let all = gather_items(&gather, "ng_person_movements", HashMap::new()).await;
        assert_eq!(
            all.len(),
            4,
            "every movement, yesterday's included: {all:?}"
        );
        assert_eq!(all[0]["person_name"], "Arlo", "newest first");
        let today: String = sqlx::query_scalar("SELECT to_char(now(), 'YYYY-MM-DD')")
            .fetch_one(&pool)
            .await
            .unwrap();
        let one_day = gather_items(
            &gather,
            "ng_person_movements",
            HashMap::from([("day".to_string(), today)]),
        )
        .await;
        assert_eq!(one_day.len(), 3, "?day= keeps only that day: {one_day:?}");
        let html = render_page(
            "ng_person_movements",
            "Arrivals and departures",
            "/people/movements",
            &all,
        );
        assert!(html.contains("Arrived") && html.contains("Left"), "{html}");

        let new = gather_items(&gather, "ng_devices_new", HashMap::new()).await;
        assert_eq!(new.len(), 2, "{new:?}");
        assert!(
            render_page("ng_devices_new", "New this week", "/devices/new", &new).contains("92%")
        );
    });
}

/// A quiet house: nobody, nothing, no events. The overview is still one row,
/// with zeroes and three empty lists rather than missing keys, and the page
/// renders its empty states.
#[test]
fn an_empty_house_still_has_an_overview() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        assert_eq!(rows.len(), 1);
        for list in ["home", "away", "movements", "new_devices", "attention"] {
            assert_eq!(rows[0][list], serde_json::json!([]), "{list}: {}", rows[0]);
        }
        let html = render_page("ng_overview", "Overview", "/overview", &rows);
        // Nobody added yet: the one sentence that says so, not "0 of 0".
        assert!(
            html.contains("Netgrasp doesn't know whose devices are whose yet."),
            "{html}"
        );
        assert!(html.contains("Nothing needs you."), "{html}");
        assert!(!html.contains("ng-attention__alert"), "{html}");
    });
}

/// The front page moves to the overview from the old default, and from nothing,
/// and from nowhere else.
#[test]
fn the_front_page_moves_to_the_overview_only_from_the_old_default() {
    serial(async {
        let pool = fresh_pool().await;
        let migration = std::fs::read_to_string(
            plugin_source_dir().join("migrations/008_netgrasp_overview.sql"),
        )
        .unwrap();
        let front = |pool: PgPool| async move {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT value FROM site_config WHERE key = 'site_front_page'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap()
        };

        for (before, after) in [
            (Some("/devices/online"), "/overview"),
            (None, "/overview"),
            (
                Some("/somewhere-the-operator-chose"),
                "/somewhere-the-operator-chose",
            ),
        ] {
            sqlx::query("DELETE FROM site_config WHERE key = 'site_front_page'")
                .execute(&pool)
                .await
                .unwrap();
            if let Some(path) = before {
                sqlx::query(
                    "INSERT INTO site_config (key, value, updated) VALUES ('site_front_page', $1, NOW())",
                )
                .bind(serde_json::json!(path))
                .execute(&pool)
                .await
                .unwrap();
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(migration.as_str()))
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(
                front(pool.clone()).await,
                Some(serde_json::json!(after)),
                "front page {before:?} became the wrong thing"
            );
        }
    });
}

// ===========================================================================
// Location
// ===========================================================================

/// Place a device the way the daemon's UniFi enrichment does: an access point
/// and the place it maps to, either of which may be absent.
async fn place(pool: &PgPool, device: i64, location: Option<&str>, ap: Option<&str>) {
    sqlx::query("UPDATE ng_devices SET current_location = $1, current_ap = $2 WHERE id = $3")
        .bind(location)
        .bind(ap)
        .bind(device)
        .execute(pool)
        .await
        .unwrap();
}

/// /devices/location through the real `GatherService`: only placed, unhidden
/// devices, sorted so a place's devices are adjacent, and the page rendering one
/// section per place with the device table inside it.
#[test]
fn the_location_page_groups_every_placed_device_under_its_place() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let a = seed_device(&pool, "aa:bb:cc:00:02:01", Some("tv"), "online").await;
        let b = seed_device(&pool, "aa:bb:cc:00:02:02", Some("laptop"), "online").await;
        let c = seed_device(&pool, "aa:bb:cc:00:02:03", Some("phone"), "online").await;
        let ap_only = seed_device(&pool, "aa:bb:cc:00:02:04", Some("ap-only"), "online").await;
        let wired = seed_device(&pool, "aa:bb:cc:00:02:05", Some("wired"), "online").await;
        let hidden = seed_device(&pool, "aa:bb:cc:00:02:06", Some("hidden"), "online").await;
        place(&pool, a, Some("Living room"), Some("Living room AP")).await;
        place(&pool, b, Some("Studio"), Some("Studio AP")).await;
        place(&pool, c, Some("Living room"), Some("Living room AP")).await;
        place(&pool, ap_only, None, Some("Garage AP")).await;
        place(&pool, wired, None, None).await;
        place(&pool, hidden, Some("Studio"), Some("Studio AP")).await;
        sqlx::query("UPDATE ng_devices SET hidden = true WHERE id = $1")
            .bind(hidden)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_devices_by_location", HashMap::new()).await;
        let places: Vec<&str> = rows
            .iter()
            .map(|r| r["current_location"].as_str().unwrap())
            .collect();
        assert_eq!(
            places,
            ["Living room", "Living room", "Studio"],
            "placed and unhidden only, sorted by place"
        );
        assert!(rows[0]["current_ap"].is_string(), "{}", rows[0]);

        let html = render_page(
            "ng_devices_by_location",
            "Devices by location",
            "/devices/location",
            &rows,
        );
        assert_eq!(html.matches("class=\"ng-section\"").count(), 2, "{html}");
        assert!(html.contains("id=\"loc-living-room\""), "{html}");
        assert!(html.contains("id=\"loc-studio\""), "{html}");
        assert!(html.contains("Studio AP"), "{html}");
        assert!(
            !html.contains("aa:bb:cc:00:02:06"),
            "a hidden device was listed: {html}"
        );
    });
}

/// **UniFi enrichment off**: every device's place and access point are null.
/// The device list renders no Where column, the location page is its empty
/// state saying why, and nothing errors.
#[test]
fn with_enrichment_off_the_device_pages_read_cleanly_and_say_why_location_is_empty() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        for mac in ["aa:bb:cc:00:03:01", "aa:bb:cc:00:03:02"] {
            let id = seed_device(&pool, mac, Some("dev"), "online").await;
            place(&pool, id, None, None).await;
        }
        let gather = wire_gather(&pool).await;

        let devices = gather_items(&gather, "ng_device_list", HashMap::new()).await;
        assert_eq!(devices.len(), 2);
        assert!(devices[0]["current_location"].is_null() && devices[0]["current_ap"].is_null());
        let html = render_page("ng_device_list", "Devices", "/devices", &devices);
        assert!(!html.contains("<th>Where</th>"), "{html}");

        let placed = gather_items(&gather, "ng_devices_by_location", HashMap::new()).await;
        assert!(placed.is_empty());
        let html = render_page(
            "ng_devices_by_location",
            "Devices by location",
            "/devices/location",
            &placed,
        );
        assert!(html.contains("UniFi enrichment"), "{html}");

        // The same device list, once one device is placed, grows the column.
        let id = seed_device(&pool, "aa:bb:cc:00:03:03", Some("placed"), "online").await;
        place(&pool, id, Some("Studio"), Some("Studio AP")).await;
        let devices = gather_items(&gather, "ng_device_list", HashMap::new()).await;
        let html = render_page("ng_device_list", "Devices", "/devices", &devices);
        assert!(html.contains("<th>Where</th>"), "{html}");
        assert!(html.contains("/devices/location#loc-studio"), "{html}");
    });
}

/// The device Item page, through the real `tap_item_view`: the place and the
/// access point when the daemon has them, and an explanation rather than an
/// apparent gap when it has neither.
#[test]
fn the_device_page_shows_the_access_point_and_explains_a_missing_location() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let placed = seed_device(&pool, "aa:bb:cc:00:04:01", Some("placed"), "online").await;
        let bare = seed_device(&pool, "aa:bb:cc:00:04:02", Some("bare"), "online").await;
        place(&pool, placed, Some("Studio"), Some("Studio AP")).await;
        place(&pool, bare, None, None).await;
        run_cron(&pool).await;

        let view = |device: i64| {
            let pool = pool.clone();
            async move {
                let (item_id, _) = link_of(&pool, device).await;
                let item = item_json(&pool, item_id.unwrap()).await;
                let results = dispatcher()
                    .dispatch("tap_item_view", &item.to_string(), background(&pool))
                    .await;
                serde_json::from_str::<String>(&results[0].output).unwrap()
            }
        };

        let html = view(placed).await;
        assert!(html.contains("<dt>Location</dt><dd>Studio</dd>"), "{html}");
        assert!(
            html.contains("<dt>Access point</dt><dd>Studio AP</dd>"),
            "{html}"
        );

        let html = view(bare).await;
        assert!(!html.contains("<dt>Location</dt>"), "{html}");
        assert!(!html.contains("<dt>Access point</dt>"), "{html}");
        assert!(html.contains("UniFi enrichment"), "{html}");
    });
}

// ===========================================================================
// New devices: the event record and the to-do
// ===========================================================================

/// /events/new-devices is the daemon's `new_device` events and nothing else,
/// rendered in the ordinary event table with a menu on each row.
#[test]
fn the_new_device_event_page_lists_only_new_device_events() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:05:01", None, "online").await;
        for event_type in [
            "new_device",
            "returned",
            "arp_scan",
            "new_device",
            "went_offline",
        ] {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
                 VALUES ($1, $2, now())",
            )
            .bind(device)
            .bind(event_type)
            .execute(&pool)
            .await
            .unwrap();
        }
        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_event_new_devices", HashMap::new()).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().all(|r| r["event_type"] == "new_device"));

        let html = render_page(
            "ng_event_new_devices",
            "New device events",
            "/events/new-devices",
            &rows,
        );
        assert!(html.contains("ng-menu__button"), "{html}");
        assert!(html.contains("href=\"/devices/todo\""), "{html}");
    });
}

/// **The to-do is the operator's task after a new device appears**: it lists
/// every unhidden device with neither a typed name nor an owner, and a device
/// leaves it the moment either is given. The rename goes through the row menu's
/// real form, the way a person on the page would do it.
#[test]
fn the_todo_lists_unnamed_unowned_devices_until_somebody_names_or_assigns_them() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let owner = seed_person_item(&pool, "Jamie").await;

        // Two to do: one the daemon knows nothing about, one it has guessed a
        // name for (a guess is not a name a person gave it).
        let bare = seed_clean_device(&pool, "02:00:5e:00:05:01", None, "online").await;
        let guessed = seed_clean_device(&pool, "02:00:5e:00:05:02", None, "online").await;
        sqlx::query("UPDATE ng_devices SET resolved_name = 'jamie-phone', first_seen_at = now() - interval '1 day' WHERE id = $1")
            .bind(guessed)
            .execute(&pool)
            .await
            .unwrap();
        // Three done: named, owned, and hidden.
        let named = seed_clean_device(&pool, "02:00:5e:00:05:03", None, "online").await;
        sqlx::query("UPDATE ng_devices SET display_name = 'Router' WHERE id = $1")
            .bind(named)
            .execute(&pool)
            .await
            .unwrap();
        seed_clean_device(&pool, "02:00:5e:00:05:04", Some(owner), "online").await;
        let hidden = seed_clean_device(&pool, "02:00:5e:00:05:05", None, "online").await;
        sqlx::query("UPDATE ng_devices SET hidden = true WHERE id = $1")
            .bind(hidden)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let todo = |gather: Arc<GatherService>| async move {
            gather_items(&gather, "ng_devices_todo", HashMap::new())
                .await
                .iter()
                .map(|r| r["mac"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            todo(gather.clone()).await,
            ["02:00:5e:00:05:01", "02:00:5e:00:05:02"],
            "unnamed, unowned and unhidden, newest first"
        );

        let rows = gather_items(&gather, "ng_devices_todo", HashMap::new()).await;
        let html = render_page("ng_devices_todo", "To do", "/devices/todo", &rows);
        assert!(
            html.contains("jamie-phone"),
            "the daemon's guess is shown: {html}"
        );
        assert!(html.contains("/netgrasp/device/rename?"), "{html}");
        assert!(html.contains("/netgrasp/device/owner?"), "{html}");

        // Named through the row menu's form: off the list.
        let response = call_form(
            &pool,
            &admin,
            "device_rename_save",
            "POST",
            FORM_RENAME,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A05%3A02&back=%2Fdevices%2Ftodo&value=Jamie%27s+phone",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(todo(gather.clone()).await, ["02:00:5e:00:05:01"]);

        // Assigned: off the list, and the list is empty.
        sqlx::query("UPDATE ng_devices SET owner_item_id = $1 WHERE id = $2")
            .bind(owner)
            .bind(bare)
            .execute(&pool)
            .await
            .unwrap();
        assert!(todo(gather.clone()).await.is_empty());
        let html = render_page("ng_devices_todo", "To do", "/devices/todo", &[]);
        assert!(
            html.contains("leaves this list once it has a name or an owner"),
            "the empty to-do does not say what empties it: {html}"
        );
    });
}

// ===========================================================================
// The overview's questions, in conversation
// ===========================================================================

/// **"Who came home today" and "what is new this week", answered by the
/// assistant** from the same seeded house the overview tests read, through the
/// real `tap_assistant_tool`. The answers must agree with the page: the same
/// people, the same order, the same devices.
#[test]
fn the_network_assistant_answers_who_came_home_today_and_what_is_new() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let admin = ng_admin(&pool).await;

        let today = call_tool(
            &pool,
            &admin,
            "netgrasp_network",
            None,
            "arrivals_and_departures",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(today["ok"], true, "{today}");
        let text = today["content"].as_str().unwrap_or_default();
        assert!(text.contains("(today)"), "{text}");
        assert!(text.contains("2 arrivals, 1 departure"), "{text}");
        let (aurora, jamie, arlo) = (
            text.find("Aurora arrived").unwrap_or(usize::MAX),
            text.find("Jamie arrived, at Studio, via Driveway")
                .unwrap_or(usize::MAX),
            text.find("Arlo left, via Gate").unwrap_or(usize::MAX),
        );
        assert!(
            aurora < jamie && jamie < arlo && arlo < usize::MAX,
            "the day in the order the overview shows it: {text}"
        );
        assert!(
            !text.contains("Jamie left"),
            "yesterday's departure leaked into today: {text}"
        );
        assert!(text.contains("Home now (2):"), "{text}");

        // Another day, by argument, and a day that does not exist, refused.
        let yesterday: String =
            sqlx::query_scalar("SELECT to_char(now() - interval '1 day', 'YYYY-MM-DD')")
                .fetch_one(&pool)
                .await
                .unwrap();
        let other = call_tool(
            &pool,
            &admin,
            "netgrasp_network",
            None,
            "arrivals_and_departures",
            serde_json::json!({ "day": yesterday }),
            "execute",
        )
        .await;
        let text = other["content"].as_str().unwrap_or_default();
        assert!(text.contains("0 arrivals, 1 departure"), "{text}");
        assert!(!text.contains("(today)"), "{text}");
        let refused = call_tool(
            &pool,
            &admin,
            "netgrasp_network",
            None,
            "arrivals_and_departures",
            serde_json::json!({ "day": "2026-02-30" }),
            "execute",
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");

        let new = call_tool(
            &pool,
            &admin,
            "netgrasp_network",
            None,
            "new_devices",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(new["ok"], true, "{new}");
        let text = new["content"].as_str().unwrap_or_default();
        assert!(
            text.starts_with("2 devices first seen in the last seven days"),
            "{text}"
        );
        assert!(text.contains("aa:bb:cc:00:01:04"), "{text}");
        assert!(text.contains("looks like a phone (92% sure)"), "{text}");
        assert!(text.contains("aa:bb:cc:00:01:05"), "{text}");
        assert!(
            !text.contains("aa:bb:cc:00:01:03"),
            "the hidden device: {text}"
        );
        assert!(
            !text.contains("aa:bb:cc:00:01:02"),
            "the year-old device: {text}"
        );

        // A read changes nothing: the overview's counts are what they were.
        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        assert_eq!(rows[0]["devices_new"], 2);
        assert_eq!(rows[0]["movements_today"], 3);
    });
}

/// The three reads issue #32 and #33 were about, through the real
/// `tap_assistant_tool`: `list_devices` counts every device in its header and
/// says how many rows it showed, `new_devices` takes a window, and `events`
/// counts by type before it lists anything.
#[test]
fn the_network_assistant_counts_devices_and_events_exactly() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let admin = ng_admin(&pool).await;
        let call = |tool: &'static str, args: serde_json::Value| {
            let (pool, admin) = (pool.clone(), admin.clone());
            async move {
                let result = call_tool(
                    &pool,
                    &admin,
                    "netgrasp_network",
                    None,
                    tool,
                    args,
                    "execute",
                )
                .await;
                assert_eq!(result["ok"], true, "{tool}: {result}");
                (
                    result["content"].as_str().unwrap_or_default().to_string(),
                    result["summary"].as_str().unwrap_or_default().to_string(),
                )
            }
        };

        // Five devices, one row asked for: the counts are still all five.
        let (text, summary) = call("list_devices", serde_json::json!({"limit": 1})).await;
        assert!(text.starts_with("5 devices (all)."), "{text}");
        assert!(text.contains("By state: online 4, offline 1."), "{text}");
        assert!(
            text.contains("By vendor: Apple 4, no vendor recorded 1."),
            "{text}"
        );
        assert!(text.contains("Showing 1 of 5."), "{text}");
        assert!(
            summary.starts_with("Showing 1 of 5 devices (all)"),
            "{summary}"
        );

        let (text, _) = call("list_devices", serde_json::json!({"filter": "offline"})).await;
        assert!(text.starts_with("1 device (offline)."), "{text}");
        assert!(text.contains("aa:bb:cc:00:01:05"), "{text}");
        assert!(!text.contains("aa:bb:cc:00:01:04"), "{text}");

        let (text, _) = call(
            "list_devices",
            serde_json::json!({"filter": "vendor:apple"}),
        )
        .await;
        assert!(text.starts_with("4 devices (vendor:apple)."), "{text}");

        // Two hours ago is in the last day; six days ago is not.
        let (text, _) = call("new_devices", serde_json::json!({"hours": 24})).await;
        assert!(
            text.starts_with("1 device first seen in the last 24 hours"),
            "{text}"
        );
        assert!(text.contains("aa:bb:cc:00:01:04"), "{text}");
        assert!(!text.contains("aa:bb:cc:00:01:05"), "{text}");

        // Events: counted by type over the window, then listed.
        let phone: i64 =
            sqlx::query_scalar("SELECT id FROM ng_devices WHERE mac = 'aa:bb:cc:00:01:01'")
                .fetch_one(&pool)
                .await
                .unwrap();
        for minutes in 1..=25 {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
                 VALUES ($1, 'arp_scan', now() - make_interval(mins => $2))",
            )
            .bind(phone)
            .bind(minutes)
            .execute(&pool)
            .await
            .unwrap();
        }
        let (text, summary) = call(
            "events",
            serde_json::json!({"type": "arp_scan", "limit": 5}),
        )
        .await;
        assert!(
            text.contains("25 events in all, 0 of them sent as alerts"),
            "{text}"
        );
        assert!(text.contains("  arp_scan: 25 (security)"), "{text}");
        assert!(
            text.contains("jamie-phone (aa:bb:cc:00:01:01): 25"),
            "{text}"
        );
        assert!(text.contains("Showing 5 of 25 events."), "{text}");
        assert!(summary.contains("showing 5"), "{summary}");

        let refused = call_tool(
            &pool,
            &admin,
            "netgrasp_network",
            None,
            "events",
            serde_json::json!({"hours": 500}),
            "execute",
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");

        // Reads write nothing.
        let dirty: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ng_devices WHERE trovato_item_id IS NOT NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(dirty, 0, "a read minted an item");
    });
}

/// Somebody without the permission gets neither answer.
#[test]
fn the_overview_reads_refuse_a_caller_without_the_permission() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let nobody = ng_nobody(&pool).await;
        for tool in [
            "arrivals_and_departures",
            "new_devices",
            "events",
            "list_devices",
        ] {
            let result = call_tool(
                &pool,
                &nobody,
                "netgrasp_network",
                None,
                tool,
                serde_json::json!({}),
                "execute",
            )
            .await;
            assert_eq!(result["ok"], false, "{tool}: {result}");
            let text = result.to_string();
            assert!(!text.contains("Jamie"), "{tool} leaked a name: {text}");
        }
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

    let categories = CategoryService::new(pool.clone(), Duration::from_secs(60), metrics());
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
        metrics(),
    );
    gather.set_item_service(items(pool));
    gather.set_record_types(Arc::new(registry));
    gather.load_queries().await.expect("load gather queries");
    gather
}

/// Execute a seeded gather by id and return its row count.
///
/// No exposed filters are passed. Every facet is a URL argument
/// (`G-EXPOSED-FILTER-NO-MATCH-ALL`, `DESIGN.md` Decision 6); the one exposed
/// filter the plugin has, the Devices search, is exercised through `searched`.
async fn run_gather(
    gather: &GatherService,
    _pool: &PgPool,
    query_id: &str,
    url_args: HashMap<String, String>,
) -> usize {
    gather_items(gather, query_id, url_args).await.len()
}

/// Somebody allowed to see the network: an authenticated viewer holding
/// `view netgrasp devices`, which is who these pages are for. The kernel runs a
/// gather with no viewer as an anonymous one, and since the plugin's
/// `tap_field_access` an anonymous viewer gets every row with its fields
/// stripped, so the gathers below are run as this viewer, and the anonymous
/// case has a test of its own.
fn network_viewer() -> UserContext {
    UserContext::authenticated(Uuid::now_v7(), vec!["view netgrasp devices".into()])
}

/// The rows a seeded gather returns, for the assertions that are about what a
/// row contains rather than how many there are.
async fn gather_items(
    gather: &GatherService,
    query_id: &str,
    url_args: HashMap<String, String>,
) -> Vec<serde_json::Value> {
    gather_items_as(gather, query_id, url_args, network_viewer()).await
}

/// The rows a seeded gather returns to one particular viewer.
async fn gather_items_as(
    gather: &GatherService,
    query_id: &str,
    url_args: HashMap<String, String>,
    viewer: UserContext,
) -> Vec<serde_json::Value> {
    let context = QueryContext {
        url_args,
        viewer: Some(viewer),
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
                    stage_id: None,
                    author_id: None,
                    created: None,
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
                &UserContext::authenticated(
                    author,
                    vec!["create ng_device content".into(), "publish content".into()],
                ),
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

// ===========================================================================
// Configuring Netgrasp by conversation
// ===========================================================================
//
// The three assistant taps, through the real dispatcher, with a real user
// context and a real Postgres. What is asserted here is everything that only
// shows up when a host is involved: that a Describe changes nothing, that an
// Execute changes exactly the user-owned columns, that a device with no Item
// gets one, and that the permission belt bites.

const SCOPE_DEVICE: &str = "netgrasp_device";
const SCOPE_PERSON: &str = "netgrasp_person";
const SCOPE_NETWORK: &str = "netgrasp_network";
const PERM_ADMINISTER: &str = "administer netgrasp";

/// A user context holding `administer netgrasp`.
///
/// The permission is checked **literally** by the host's
/// `current-user-has-permission` — there is no `administer site` bypass on that
/// call, unlike every kernel route — so a test that expects a tool to run has to
/// carry this exact string.
async fn ng_admin(pool: &PgPool) -> UserContext {
    UserContext::authenticated(
        any_user(pool).await,
        vec![
            // What 003 gives network_admin, plus the two every signed-in
            // user and, since kernel 0.107, every creator holds (024).
            PERM_ADMINISTER.to_string(),
            "view netgrasp devices".to_string(),
            "view ng_device content".to_string(),
            "create ng_device content".to_string(),
            "edit ng_device content".to_string(),
            "delete ng_device content".to_string(),
            "view ng_person content".to_string(),
            "create ng_person content".to_string(),
            "edit ng_person content".to_string(),
            "delete ng_person content".to_string(),
            "access content".to_string(),
            "publish content".to_string(),
        ],
    )
}

/// A user context holding nothing.
async fn ng_nobody(pool: &PgPool) -> UserContext {
    UserContext::authenticated(any_user(pool).await, vec!["access content".to_string()])
}

/// Dispatch one tap with a real user and services, and return its output.
async fn dispatch_as(
    pool: &PgPool,
    user: &UserContext,
    tap: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let disp = dispatcher();
    let state = RequestState::new(user.clone(), services(pool));
    let results = disp.dispatch(tap, &payload.to_string(), state).await;
    assert_eq!(results.len(), 1, "expected exactly one {tap} result");
    serde_json::from_str(&results[0].output)
        .unwrap_or_else(|e| panic!("{tap} returned non-JSON ({e}): {}", results[0].output))
}

/// Call one tool and return its `AssistantToolResult`.
async fn call_tool(
    pool: &PgPool,
    user: &UserContext,
    scope: &str,
    scope_id: Option<&str>,
    tool: &str,
    arguments: serde_json::Value,
    mode: &str,
) -> serde_json::Value {
    dispatch_as(
        pool,
        user,
        "tap_assistant_tool",
        &serde_json::json!({
            "scope": scope,
            "scope_id": scope_id,
            "tool": tool,
            "arguments": arguments,
            "mode": mode,
            "user_id": user.id.to_string(),
        }),
    )
    .await
}

/// The snapshot a conversation would open with.
async fn open_context(
    pool: &PgPool,
    user: &UserContext,
    scope: &str,
    scope_id: Option<&str>,
) -> serde_json::Value {
    dispatch_as(
        pool,
        user,
        "tap_assistant_context",
        &serde_json::json!({
            "scope": scope,
            "scope_id": scope_id,
            "user_id": user.id.to_string(),
        }),
    )
    .await
}

/// Seed a device the demo's way: `clean`, with an owner, and with **no Item**.
async fn seed_clean_device(pool: &PgPool, mac: &str, owner: Option<Uuid>, state: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO ng_devices \
             (mac, owner_item_id, vendor, device_type, os_family, state, last_ip, \
              first_seen_at, last_seen_at, sync_state) \
         VALUES ($1, $2, 'Amazon Technologies', 'tablet', 'Android', $3, '10.0.2.18', \
                 to_timestamp($4), to_timestamp($4), 'clean') \
         RETURNING id",
    )
    .bind(mac)
    .bind(owner)
    .bind(state)
    .bind(now() as f64)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Create a person Item and its mirror row, the way the person taps do.
async fn seed_person_item(pool: &PgPool, name: &str) -> Uuid {
    let author = any_user(pool).await;
    let item_id = items(pool)
        .create(
            CreateItem {
                item_type: PERSON_TYPE.into(),
                title: name.to_string(),
                status: Some(1),
                author_id: author,
                fields: Some(serde_json::json!({
                    "field_notes": "",
                    "field_notify_arrive": false,
                    "field_notify_depart": false,
                })),
                promote: Some(0),
                sticky: Some(0),
                stage_id: None,
                language: None,
                log: None,
            },
            &UserContext::authenticated(
                author,
                vec!["create ng_person content".into(), "publish content".into()],
            ),
        )
        .await
        .expect("create the person item")
        .id;
    // `tap_item_insert` mirrors it, but this test drives the taps directly, so
    // the mirror is written here the same way that tap writes it.
    sqlx::query(
        "INSERT INTO ng_people (item_id, name, notes, notify_arrive, notify_depart) \
         VALUES ($1, $2, '', FALSE, FALSE) ON CONFLICT (item_id) DO UPDATE SET name = EXCLUDED.name",
    )
    .bind(item_id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    item_id
}

/// A device row's user-owned columns and its Item link.
async fn device_state(
    pool: &PgPool,
    device: i64,
) -> (Option<Uuid>, Option<Uuid>, Option<String>, String) {
    let row = sqlx::query(
        "SELECT owner_item_id, trovato_item_id, display_name, sync_state \
         FROM ng_devices WHERE id = $1",
    )
    .bind(device)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.try_get("owner_item_id").unwrap(),
        row.try_get("trovato_item_id").unwrap(),
        row.try_get("display_name").unwrap(),
        row.try_get("sync_state").unwrap(),
    )
}

#[test]
fn the_three_scopes_are_declared_and_the_kernel_registry_accepts_them() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let disp = dispatcher();
        let results = disp
            .dispatch("tap_assistant_scopes", "{}", background(&pool))
            .await;
        assert_eq!(results.len(), 1, "the manifest must list the scopes tap");

        // The kernel's own validation, not a restatement of it: if a scope would
        // be dropped on a real site, it is dropped here and named.
        let registry = trovato_kernel::assistant::AssistantRegistry::from_tap_results(
            results
                .into_iter()
                .map(|r| (r.plugin_name, r.output))
                .collect(),
        );
        assert!(
            registry.rejections().is_empty(),
            "the kernel refused a scope: {:?}",
            registry.rejections()
        );
        assert_eq!(registry.len(), 3);

        let device = registry.get(SCOPE_DEVICE).expect("the device scope");
        assert_eq!(device.scope.permission, PERM_ADMINISTER);
        assert!(device.applies_to_item_type(DEVICE_TYPE));
        assert!(!device.applies_to_item_type(PERSON_TYPE));
        assert_eq!(device.write_tool_count(), 4);

        let person = registry.get(SCOPE_PERSON).expect("the person scope");
        assert!(person.applies_to_item_type(PERSON_TYPE));
        assert_eq!(person.write_tool_count(), 5);

        let network = registry.get(SCOPE_NETWORK).expect("the network scope");
        assert_eq!(
            network.scope.id_kind,
            trovato_sdk::types::AssistantIdKind::None
        );
        assert!(network.tool("who_was_online").is_some());
        // The overview's two questions are reads, and adding them added no
        // write: the count below is the one it was before they existed.
        for read in [
            "arrivals_and_departures",
            "new_devices",
            "events",
            "list_devices",
        ] {
            let tool = network
                .tool(read)
                .unwrap_or_else(|| panic!("the network scope has no {read}"));
            assert_eq!(
                tool.kind,
                trovato_sdk::types::AssistantToolKind::Read,
                "{read} must be a read"
            );
        }
        assert_eq!(network.write_tool_count(), 5);

        // Every scope's prompt says what the daemon cannot know, which is the
        // most confident wrong answer available here.
        for scope in registry.scopes() {
            assert!(
                scope
                    .scope
                    .prompt
                    .contains("it cannot know who is holding one"),
                "{} lost the shared prefix",
                scope.scope.name
            );
        }
    });
}

#[test]
fn each_scopes_context_describes_what_it_was_opened_on() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let jamie = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:04", Some(jamie), "offline").await;
        run_cron(&pool).await; // no dirty rows; the device keeps no Item

        // A device conversation opens on the Item, so give it one.
        sqlx::query("UPDATE ng_devices SET sync_state = 'dirty' WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        run_cron(&pool).await;
        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item_id = item_id.expect("the sync minted an item").to_string();

        let context = open_context(&pool, &admin, SCOPE_DEVICE, Some(&item_id)).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(snapshot.contains("02:00:5e:00:00:04"), "{snapshot}");
        assert!(snapshot.contains("Owner: Jamie"), "{snapshot}");
        assert!(snapshot.contains("Amazon Technologies"), "{snapshot}");
        assert!(
            snapshot.len() < netgrasp_core::assist::SNAPSHOT_MAX_BYTES,
            "the snapshot is {} bytes",
            snapshot.len()
        );
        assert!(!context["links"].as_array().unwrap().is_empty());

        let context = open_context(&pool, &admin, SCOPE_PERSON, Some(&jamie.to_string())).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(snapshot.contains("Person: Jamie"), "{snapshot}");
        assert!(snapshot.contains("Devices (1):"), "{snapshot}");
        assert!(snapshot.contains("02:00:5e:00:00:04"), "{snapshot}");

        let context = open_context(&pool, &admin, SCOPE_NETWORK, None).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(snapshot.contains("People (1):"), "{snapshot}");
        assert!(
            snapshot.contains("Devices by state (1 total): offline 1."),
            "{snapshot}"
        );
        assert!(
            snapshot.contains("Owners: 1 belong to a person, 0 to nobody."),
            "{snapshot}"
        );
        assert!(
            !snapshot.contains("02:00:5e:00:00:04"),
            "the network context lists no device: {snapshot}"
        );
        assert!(
            snapshot.contains("Security events in the last 24 hours: 0"),
            "{snapshot}"
        );
    });
}

#[test]
fn describing_an_assignment_changes_nothing_at_all() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:04", Some(arlo), "offline").await;

        let before_daemon = daemon_snapshot(&pool, device).await;
        let before_state = device_state(&pool, device).await;

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "assign_device",
            serde_json::json!({"device": "02:00:5e:00:00:04", "person": "Jamie"}),
            "describe",
        )
        .await;

        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(
            result["summary"].as_str().unwrap_or_default(),
            "Assign Amazon tablet (02:00:5e:00:00:04) to Jamie (currently Arlo). \
             Changes the owner, and nothing else (creates its Trovato item)",
            "the card names the device, the new owner, the one it replaces, and what changes"
        );

        // Nothing moved. Not the owner, not the link, not a daemon column.
        assert_eq!(device_state(&pool, device).await, before_state);
        assert_eq!(daemon_snapshot(&pool, device).await, before_daemon);
        let items_now: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = 'ng_device'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(items_now, 0, "Describe minted an item");
    });
}

#[test]
fn applying_an_assignment_writes_both_tiers_and_mints_the_missing_item() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        // The demo's own state: clean, owned, and with no Item. `write_back_device`
        // addresses the row by the Item link, so without minting one this would
        // update nothing and report success.
        let device = seed_clean_device(&pool, "02:00:5e:00:00:04", Some(arlo), "offline").await;
        let before_daemon = daemon_snapshot(&pool, device).await;

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "assign_device",
            serde_json::json!({"device": "02:00:5e:00:00:04", "person": "Jamie"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        let (owner, item_id, display_name, sync_state) = device_state(&pool, device).await;
        assert_eq!(owner, Some(jamie), "the daemon's row names the new owner");
        let item_id = item_id.expect("the tool minted the device's item");
        assert_eq!(
            display_name, None,
            "an unchanged title clears display_name rather than pinning the daemon's own name"
        );
        assert_eq!(
            sync_state, "clean",
            "no assistant write may raise sync_state, or the loop has an edge"
        );
        assert_eq!(
            daemon_snapshot(&pool, device).await,
            before_daemon,
            "a daemon-owned column moved"
        );

        // Both tiers agree: the Item's field carries the same owner.
        let item = item_json(&pool, item_id).await;
        assert_eq!(
            item["fields"]["field_owner"].as_str().unwrap_or_default(),
            jamie.to_string()
        );
        assert_eq!(
            item["fields"]["field_mac"].as_str().unwrap_or_default(),
            "02:00:5e:00:00:04",
            "every field is written, because Item::update replaces them wholesale"
        );

        // And a following sync tick is a no-op: nothing was left dirty.
        let report = run_cron(&pool).await;
        assert_eq!(
            report["sync"]["examined"], 0,
            "the write left a row dirty: {report}"
        );

        // Unassigning clears both tiers.
        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id.to_string()),
            "set_owner",
            serde_json::json!({"person": null}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let (owner, _, _, _) = device_state(&pool, device).await;
        assert_eq!(owner, None);
        let item = item_json(&pool, item_id).await;
        assert_eq!(
            item["fields"]["field_owner"].as_str().unwrap_or_default(),
            ""
        );
    });
}

#[test]
fn renaming_stores_a_typed_name_and_clears_a_derived_one() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let device = seed_clean_device(&pool, "02:00:5e:00:00:08", None, "unknown").await;
        sqlx::query("UPDATE ng_devices SET sync_state = 'dirty', vendor = NULL WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        run_cron(&pool).await;
        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item_id = item_id.expect("the sync minted an item").to_string();

        // A name a human typed is stored and wins.
        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id),
            "rename",
            serde_json::json!({"display_name": "Office printer"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let (_, _, display_name, _) = device_state(&pool, device).await;
        assert_eq!(display_name.as_deref(), Some("Office printer"));

        // A name that merely equals what the daemon would have called it anyway
        // is stored as NULL, so the device goes back to tracking what the daemon
        // learns. The existing pinning rule, reached through the assistant.
        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id),
            "rename",
            serde_json::json!({"display_name": "02:00:5e:00:00:08"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let (_, _, display_name, _) = device_state(&pool, device).await;
        assert_eq!(
            display_name, None,
            "the daemon's own name must not be pinned as a human's choice"
        );
    });
}

#[test]
fn creating_and_deleting_a_person_keeps_the_mirror_in_step() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "create_person",
            serde_json::json!({"name": "Aurora"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        let (item_id, name): (Uuid, String) =
            sqlx::query_as("SELECT item_id, name FROM ng_people WHERE name = 'Aurora'")
                .fetch_one(&pool)
                .await
                .expect("the mirror row was written");
        assert_eq!(name, "Aurora");
        let item = item_json(&pool, item_id).await;
        assert_eq!(item["type"], PERSON_TYPE);
        assert_eq!(item["title"], "Aurora");

        // Describing a duplicate is refused, so a card nobody would question is
        // never produced.
        let duplicate = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "create_person",
            serde_json::json!({"name": "aurora"}),
            "describe",
        )
        .await;
        assert_eq!(duplicate["ok"], false, "{duplicate}");
        assert!(
            duplicate["content"]
                .as_str()
                .unwrap_or_default()
                .contains("already exists"),
            "{duplicate}"
        );

        // A device in the way stops the delete, at execute as well as describe.
        let device = seed_clean_device(&pool, "02:00:5e:00:00:03", Some(item_id), "online").await;
        let refused = call_tool(
            &pool,
            &admin,
            SCOPE_PERSON,
            Some(&item_id.to_string()),
            "delete_person",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(
            refused["content"]
                .as_str()
                .unwrap_or_default()
                .contains("Unassign"),
            "{refused}"
        );
        assert!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ng_people WHERE item_id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap()
                == 1,
            "the refused delete removed the mirror row anyway"
        );

        // Unassign, then delete.
        call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "assign_device",
            serde_json::json!({"device": "02:00:5e:00:00:03", "person": null}),
            "execute",
        )
        .await;
        let deleted = call_tool(
            &pool,
            &admin,
            SCOPE_PERSON,
            Some(&item_id.to_string()),
            "delete_person",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(deleted["ok"], true, "{deleted}");

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ng_people WHERE item_id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "the mirror row outlived the person"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "the person's item outlived the person"
        );
        // And no device is left pointing at an id with nothing behind it.
        let (owner, _, _, _) = device_state(&pool, device).await;
        assert_eq!(owner, None);
    });
}

#[test]
fn a_caller_without_the_permission_gets_nothing_done() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let nobody = ng_nobody(&pool).await;

        seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:04", None, "offline").await;
        let before = device_state(&pool, device).await;

        for mode in ["describe", "execute"] {
            let refused = call_tool(
                &pool,
                &nobody,
                SCOPE_NETWORK,
                None,
                "assign_device",
                serde_json::json!({"device": "02:00:5e:00:00:04", "person": "Jamie"}),
                mode,
            )
            .await;
            assert_eq!(refused["ok"], false, "{mode}: {refused}");
            assert!(
                refused["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("do not have permission"),
                "{mode}: {refused}"
            );
        }
        assert_eq!(device_state(&pool, device).await, before);

        // A read is refused too: the belt is on the whole tap, not on the writes.
        let refused = call_tool(
            &pool,
            &nobody,
            SCOPE_NETWORK,
            None,
            "list_people",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");

        // The same call with the permission works, which is what makes the
        // refusal above about the permission rather than about the arguments.
        let allowed = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "list_people",
            serde_json::json!({}),
            "execute",
        )
        .await;
        assert_eq!(allowed["ok"], true, "{allowed}");
        assert!(
            allowed["content"]
                .as_str()
                .unwrap_or_default()
                .contains("Jamie")
        );
    });
}

#[test]
fn an_ambiguous_person_name_names_the_candidates_rather_than_guessing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        // Two people sharing a name by `same_name` cannot both have a mirror
        // row while 018's index is there. They can where it is not: a kernel
        // whose role does not own ng_people cannot create it, and leaves it to
        // the owner. That install is the one modelled here, so the resolver
        // still has to name both rather than pick.
        sqlx::query("DROP INDEX IF EXISTS ng_people_name_key")
            .execute(&pool)
            .await
            .unwrap();
        let first = seed_person_item(&pool, "Sam").await;
        let second = seed_person_item(&pool, "sam").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:07", None, "online").await;

        let refused = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "assign_device",
            serde_json::json!({"device": "02:00:5e:00:00:07", "person": "Sam"}),
            "describe",
        )
        .await;

        // A uuid resolves it, which is what the message tells the model to do.
        let described = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "assign_device",
            serde_json::json!({"device": "02:00:5e:00:00:07", "person": first.to_string()}),
            "describe",
        )
        .await;

        // Put the index back before asserting, so a failure here cannot leave
        // two Sams behind for the next test's migrations to refuse.
        sqlx::query("DELETE FROM ng_people WHERE item_id = $1")
            .bind(second)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/018_netgrasp_one_person_per_name.sql"
        ))
        .execute(&pool)
        .await
        .expect("018 applies once the second Sam is gone");

        assert_eq!(refused["ok"], false, "{refused}");
        let message = refused["content"].as_str().unwrap_or_default();
        assert!(message.contains("matches 2 people"), "{message}");
        assert!(message.contains(&first.to_string()), "{message}");
        assert!(message.contains(&second.to_string()), "{message}");
        assert_eq!(described["ok"], true, "{described}");
        let _ = device;
    });
}

#[test]
fn who_was_online_answers_from_the_seeded_presence_rows() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let jamie = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:01", Some(jamie), "online").await;
        let now = now();
        sqlx::query(
            "INSERT INTO ng_presence (device_id, ip, started_at, ended_at, is_summary) \
             VALUES ($1, '10.0.1.24', to_timestamp($2), NULL, FALSE)",
        )
        .bind(device)
        .bind((now - 7_200) as f64)
        .execute(&pool)
        .await
        .unwrap();

        let from = netgrasp_core::assist::format_utc(now - 3_600)
            .replace(" UTC", "Z")
            .replace(' ', "T");
        let to = netgrasp_core::assist::format_utc(now)
            .replace(" UTC", "Z")
            .replace(' ', "T");

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "who_was_online",
            serde_json::json!({"from": from, "to": to}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let content = result["content"].as_str().unwrap_or_default();
        assert!(content.contains("Jamie"), "{content}");
        assert!(content.contains("02:00:5e:00:00:01"), "{content}");
        assert!(
            content.contains("still online"),
            "an open span is what 'right now' means: {content}"
        );

        // A window wider than a week is refused, with the reason.
        let refused = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "who_was_online",
            serde_json::json!({"from": "2026-01-01T00:00:00Z", "to": "2026-06-01T00:00:00Z"}),
            "execute",
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(
            refused["content"]
                .as_str()
                .unwrap_or_default()
                .contains("7 days")
        );
    });
}

// ===========================================================================
// An edit changes only what it was asked to change
// ===========================================================================
//
// `docs/JOINT-RUN.md`, plugin finding 1. A rename built a whole overlay and
// filled the columns it had not been asked about from the device's Item; the
// cron sync mints Items carrying only `field_mac`; `field_bool` reads an absent
// boolean as `false`; so a rename wrote `notify = false` over a column the
// daemon's schema defaults to TRUE, and the card said nothing about it.
// Observed twice in one run, and 33 of 34 device Items were still in the state
// that reproduces it.
//
// These tests are at this layer because that is where it happened: the value
// came out of a real Item, through a real `save-item`, into a real row. The
// statement-level half is in `netgrasp_core::writeback`.

/// The user-owned columns of a device row, as a row an assertion can compare
/// whole.
///
/// Whole, rather than column by column: the defect was a column nobody was
/// looking at, so a test that names only the columns it expects to move would
/// have passed while `notify` flipped underneath it.
async fn user_owned_state(
    pool: &PgPool,
    device_id: i64,
) -> (Option<String>, Option<Uuid>, Option<String>, bool, bool) {
    let row = sqlx::query(
        "SELECT display_name, owner_item_id, notes, hidden, notify \
         FROM ng_devices WHERE id = $1",
    )
    .bind(device_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.try_get("display_name").unwrap(),
        row.try_get("owner_item_id").unwrap(),
        row.try_get("notes").unwrap(),
        row.try_get("hidden").unwrap(),
        row.try_get("notify").unwrap(),
    )
}

/// Put a device Item back into the shape the cron sync used to mint: the MAC
/// and nothing else.
///
/// Written with SQL rather than through `ItemService`, because going through
/// the service would fire `tap_item_update` and write the stripped Item back
/// over the row — which is the neighbouring defect and would destroy the
/// starting state this test needs.
async fn strip_item_to_mac_only(pool: &PgPool, item_id: Uuid, mac: &str) {
    sqlx::query("UPDATE item SET fields = $2 WHERE id = $1")
        .bind(item_id)
        .bind(serde_json::json!({ "field_mac": mac }))
        .execute(pool)
        .await
        .unwrap();
}

/// A device in the state the run found: owned, with notes, alerts on, hidden
/// off, and an Item carrying only its MAC. Returns the device id and its Item.
async fn device_with_a_bare_item(pool: &PgPool, mac: &str, owner: Uuid) -> (i64, Uuid) {
    let device = seed_clean_device(pool, mac, Some(owner), "online").await;
    sqlx::query(
        "UPDATE ng_devices SET notes = 'in the hall cupboard', hidden = FALSE, \
         notify = TRUE, sync_state = 'dirty' WHERE id = $1",
    )
    .bind(device)
    .execute(pool)
    .await
    .unwrap();

    run_cron(pool).await;
    let (_, item_id, _, _) = device_state(pool, device).await;
    let item_id = item_id.expect("the sync minted an item");
    strip_item_to_mac_only(pool, item_id, mac).await;
    (device, item_id)
}

#[test]
fn a_rename_changes_the_name_and_leaves_every_other_user_column_alone() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        let (device, item_id) = device_with_a_bare_item(&pool, "02:00:5e:00:00:04", arlo).await;
        let before_daemon = daemon_snapshot(&pool, device).await;

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id.to_string()),
            "rename",
            serde_json::json!({"display_name": "Office printer"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        let (display_name, owner, notes, hidden, notify) = user_owned_state(&pool, device).await;
        assert_eq!(display_name.as_deref(), Some("Office printer"));
        assert!(
            notify,
            "the rename turned the device's alerts off — finding 1, reproduced"
        );
        assert!(!hidden, "the rename hid the device");
        assert_eq!(
            notes.as_deref(),
            Some("in the hall cupboard"),
            "the rename cleared the notes"
        );
        assert_eq!(owner, Some(arlo), "the rename unassigned the device");
        assert_eq!(
            daemon_snapshot(&pool, device).await,
            before_daemon,
            "a daemon-owned column moved"
        );

        // The Item agrees, which is the other half: the row is right and the
        // Item still claims the alerts are off would be a state the next edit
        // resolves the wrong way again.
        let item = item_json(&pool, item_id).await;
        assert_eq!(item["title"], "Office printer");
        assert_eq!(item["fields"]["field_notify"], true, "{item}");
        assert_eq!(item["fields"]["field_hidden"], false, "{item}");
        assert_eq!(item["fields"]["field_notes"], "in the hall cupboard");
        assert_eq!(item["fields"]["field_owner"], arlo.to_string());
        assert_eq!(item["fields"]["field_mac"], "02:00:5e:00:00:04");
    });
}

#[test]
fn an_owner_assignment_changes_the_owner_and_leaves_the_flags_alone() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        let (device, item_id) = device_with_a_bare_item(&pool, "02:00:5e:00:00:05", arlo).await;

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id.to_string()),
            "set_owner",
            serde_json::json!({"person": "Jamie"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        let (display_name, owner, notes, hidden, notify) = user_owned_state(&pool, device).await;
        assert_eq!(owner, Some(jamie));
        assert!(notify, "the assignment turned the alerts off — finding 1");
        assert!(!hidden);
        assert_eq!(notes.as_deref(), Some("in the hall cupboard"));
        assert_eq!(
            display_name, None,
            "the assignment pinned a name nobody typed"
        );
    });
}

/// "Mute alerts for this device" says nothing about hiding it, and the write
/// must say nothing either — including on a device that is hidden, where
/// writing the flag's absent value would unhide it.
#[test]
fn setting_one_flag_leaves_the_other_flag_alone() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        let (device, item_id) = device_with_a_bare_item(&pool, "02:00:5e:00:00:06", arlo).await;
        sqlx::query("UPDATE ng_devices SET hidden = TRUE WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id.to_string()),
            "set_flags",
            serde_json::json!({"notify": false}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        let (_, owner, notes, hidden, notify) = user_owned_state(&pool, device).await;
        assert!(!notify, "the flag the call named was not written");
        assert!(hidden, "muting a device also unhid it");
        assert_eq!(notes.as_deref(), Some("in the hall cupboard"));
        assert_eq!(owner, Some(arlo));
    });
}

/// The card and the write are one change set. Every write tool in the device
/// scope is described, and the columns the card names are the columns the
/// following Execute actually moves.
#[test]
fn the_card_names_exactly_the_columns_the_write_then_changes() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        seed_person_item(&pool, "Jamie").await;
        let (device, item_id) = device_with_a_bare_item(&pool, "02:00:5e:00:00:07", arlo).await;
        let item_id = item_id.to_string();

        // (tool, arguments, the clause the card must carry, the columns that
        // may move). One case per write tool the device scope declares.
        let cases: Vec<(&str, serde_json::Value, &str, &[&str])> = vec![
            (
                "rename",
                serde_json::json!({"display_name": "Office printer"}),
                "Changes the name, and nothing else",
                &["display_name"],
            ),
            (
                "set_owner",
                serde_json::json!({"person": "Jamie"}),
                "Changes the owner, and nothing else",
                &["owner_item_id"],
            ),
            (
                "set_notes",
                serde_json::json!({"text": "moved to the study"}),
                "Changes the notes, and nothing else",
                &["notes"],
            ),
            (
                "set_flags",
                serde_json::json!({"notify": false}),
                "Changes the arrival and departure alerts, and nothing else",
                &["notify"],
            ),
            (
                "set_flags",
                serde_json::json!({"hidden": true, "notify": true}),
                "Changes whether it is hidden and the arrival and departure alerts, \
                 and nothing else",
                &["hidden", "notify"],
            ),
        ];

        for (tool, arguments, clause, may_move) in cases {
            let described = call_tool(
                &pool,
                &admin,
                SCOPE_DEVICE,
                Some(&item_id),
                tool,
                arguments.clone(),
                "describe",
            )
            .await;
            assert_eq!(described["ok"], true, "{tool}: {described}");
            let summary = described["summary"].as_str().unwrap_or_default();
            assert!(
                summary.contains(clause),
                "{tool}'s card does not say what it changes.\n  card: {summary}\n  wanted: {clause}"
            );

            let before = user_owned_state(&pool, device).await;
            let applied = call_tool(
                &pool,
                &admin,
                SCOPE_DEVICE,
                Some(&item_id),
                tool,
                arguments,
                "execute",
            )
            .await;
            assert_eq!(applied["ok"], true, "{tool}: {applied}");
            let after = user_owned_state(&pool, device).await;

            // Exactly the columns the card named are allowed to differ.
            let moved = [
                ("display_name", before.0 != after.0),
                ("owner_item_id", before.1 != after.1),
                ("notes", before.2 != after.2),
                ("hidden", before.3 != after.3),
                ("notify", before.4 != after.4),
            ];
            for (column, changed) in moved {
                assert!(
                    !changed || may_move.contains(&column),
                    "{tool} changed {column}, which its card did not name"
                );
            }
        }
    });
}

/// The mint's half of the same finding: an Item created for a row that already
/// has user-owned values carries them, so the two tiers never disagree in the
/// first place.
#[test]
fn a_minted_device_item_carries_the_rows_user_owned_values() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let arlo = seed_person_item(&pool, "Arlo").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:09", Some(arlo), "online").await;
        sqlx::query(
            "UPDATE ng_devices SET notes = 'in the hall cupboard', hidden = TRUE, \
             notify = TRUE, sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();

        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["created"], 1, "{report}");

        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item = item_json(&pool, item_id.expect("the sync minted an item")).await;
        assert_eq!(
            item["fields"]["field_notify"], true,
            "a minted Item claims the alerts are off on a device whose row says they are on: {item}"
        );
        assert_eq!(item["fields"]["field_hidden"], true, "{item}");
        assert_eq!(item["fields"]["field_notes"], "in the hall cupboard");
        assert_eq!(item["fields"]["field_owner"], arlo.to_string());
    });
}

// ===========================================================================
// One title, and it prefers the name a human would use
// ===========================================================================

/// `docs/JOINT-RUN.md`, plugin finding 2: the printer's Item was titled
/// "CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD. device" while the proposal
/// card for the same row said "Brother HL-L8360CDW series". The better name was
/// already in the database.
#[test]
fn a_synced_title_prefers_the_name_the_daemon_resolved_over_the_oui_vendor() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let device = seed_clean_device(&pool, "02:00:5e:00:00:0a", None, "online").await;
        sqlx::query(
            "UPDATE ng_devices SET vendor = 'CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD.', \
             resolved_name = 'Brother HL-L8360CDW series', identity_source = 'mdns', \
             hostname = NULL, sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();

        run_cron(&pool).await;
        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item_id = item_id.expect("the sync minted an item");
        let item = item_json(&pool, item_id).await;
        assert_eq!(
            item["title"], "Brother HL-L8360CDW series",
            "the Item is titled after the OUI holder instead of the resolved name"
        );

        // The assistant's context calls it the same thing, which is the
        // agreement that was missing.
        let context = open_context(&pool, &admin, SCOPE_DEVICE, Some(&item_id.to_string())).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(
            snapshot.contains("Device: Brother HL-L8360CDW series"),
            "{snapshot}"
        );

        // A later resolution re-titles the Item on the next pass, because the
        // derived title moved and nothing pinned the old one.
        sqlx::query(
            "UPDATE ng_devices SET resolved_name = 'Brother HL-L8360CDW (study)', \
             sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
        let report = run_cron(&pool).await;
        assert_eq!(report["sync"]["refreshed"], 1, "{report}");
        assert_eq!(
            item_json(&pool, item_id).await["title"],
            "Brother HL-L8360CDW (study)"
        );
    });
}

/// A name a human typed is not re-titled by a later daemon resolution. This is
/// how the plugin tells a hand-set title from a derived one: the write-back
/// stores it as `display_name`, and `display_name` is the first step of the
/// ladder — so there is no need to guess whether a title was edited.
#[test]
fn a_hand_set_name_survives_every_later_sync_pass() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let device = seed_clean_device(&pool, "02:00:5e:00:00:0b", None, "online").await;
        sqlx::query(
            "UPDATE ng_devices SET resolved_name = 'Brother HL-L8360CDW series', \
             sync_state = 'dirty' WHERE id = $1",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
        run_cron(&pool).await;
        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item_id = item_id.expect("the sync minted an item");

        let result = call_tool(
            &pool,
            &admin,
            SCOPE_DEVICE,
            Some(&item_id.to_string()),
            "rename",
            serde_json::json!({"display_name": "Office printer"}),
            "execute",
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");

        // The daemon resolves something else, twice, and marks the row dirty
        // each time — the mDNS flapping a real LAN produces.
        for name in ["Jeremy's MacBook Pro (2)", "Filbert-3"] {
            sqlx::query(
                "UPDATE ng_devices SET mdns_name = $2, resolved_name = $2, \
                 sync_state = 'dirty' WHERE id = $1",
            )
            .bind(device)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
            run_cron(&pool).await;
        }

        assert_eq!(
            item_json(&pool, item_id).await["title"],
            "Office printer",
            "a misattributed mDNS name took a device's typed name"
        );
        let (_, _, display_name, _) = device_state(&pool, device).await;
        assert_eq!(display_name.as_deref(), Some("Office printer"));
    });
}

// ===========================================================================
// The model is told when monitoring began
// ===========================================================================

/// `docs/JOINT-RUN.md`, plugin finding 3: asked who was online yesterday
/// against a database an hour old, the model found zero spans and speculated
/// about a gap in monitoring, because nothing told it the daemon's earliest
/// observation was that morning.
///
/// The read tool's answer is unchanged — zero spans is the truth. What changed
/// is that the context says why, so the two empty windows a model cannot
/// otherwise tell apart are distinguishable.
#[test]
fn the_network_context_says_when_monitoring_began() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        // A fresh install says so rather than implying a monitored silence.
        let context = open_context(&pool, &admin, SCOPE_NETWORK, None).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(
            snapshot.contains("nothing has been observed yet"),
            "{snapshot}"
        );

        let jamie = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:0c", Some(jamie), "online").await;
        let now = now();
        let began = now - 3_600;
        sqlx::query(
            "INSERT INTO ng_presence (device_id, ip, started_at, ended_at, is_summary) \
             VALUES ($1, '10.0.1.24', to_timestamp($2), NULL, FALSE)",
        )
        .bind(device)
        .bind(began as f64)
        .execute(&pool)
        .await
        .unwrap();

        let context = open_context(&pool, &admin, SCOPE_NETWORK, None).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        let expected = netgrasp_core::assist::format_utc(began);
        assert!(
            snapshot.contains(&format!("Monitoring data begins at {expected}")),
            "the context does not say when the data starts: {snapshot}"
        );
        assert!(
            snapshot.contains("no data before that"),
            "the context does not say what an earlier window means: {snapshot}"
        );
        assert!(snapshot.contains("Current time:"), "{snapshot}");

        // The question that produced the finding: a window that ends before the
        // first observation. The tool still answers "nothing", and the context
        // the model holds while reading that answer explains it.
        let iso = |ts: i64| {
            netgrasp_core::assist::format_utc(ts)
                .replace(" UTC", "Z")
                .replace(' ', "T")
        };
        let yesterday = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "who_was_online",
            serde_json::json!({"from": iso(now - 86_400 * 2), "to": iso(now - 86_400)}),
            "execute",
        )
        .await;
        assert_eq!(yesterday["ok"], true, "{yesterday}");
        assert_eq!(
            yesterday["content"].as_str().unwrap_or_default(),
            "Nothing was online in that window."
        );

        // And a window inside the monitored period is not empty, which is what
        // makes the sentence above a distinction rather than a disclaimer.
        let recent = call_tool(
            &pool,
            &admin,
            SCOPE_NETWORK,
            None,
            "who_was_online",
            serde_json::json!({"from": iso(now - 1_800), "to": iso(now)}),
            "execute",
        )
        .await;
        assert!(
            recent["content"]
                .as_str()
                .unwrap_or_default()
                .contains("Jamie"),
            "{recent}"
        );
    });
}

/// The device scope gets the same sentence about its own device, from its own
/// `first_seen`.
#[test]
fn the_device_context_says_when_that_device_was_first_seen() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let device = seed_clean_device(&pool, "02:00:5e:00:00:0e", None, "online").await;
        let first_seen = now() - 86_400 * 3;
        sqlx::query(
            "UPDATE ng_devices SET first_seen_at = to_timestamp($2), sync_state = 'dirty' \
             WHERE id = $1",
        )
        .bind(device)
        .bind(first_seen as f64)
        .execute(&pool)
        .await
        .unwrap();
        run_cron(&pool).await;
        let (_, item_id, _, _) = device_state(&pool, device).await;
        let item_id = item_id.expect("the sync minted an item").to_string();

        let context = open_context(&pool, &admin, SCOPE_DEVICE, Some(&item_id)).await;
        let snapshot = context["snapshot"].as_str().unwrap_or_default();
        assert!(
            snapshot.contains(&format!(
                "Monitoring data begins at {}",
                netgrasp_core::assist::format_utc(first_seen)
            )),
            "{snapshot}"
        );
    });
}

// ===========================================================================
// The row menu's forms, through the real module
// ===========================================================================
//
// `tap_api` is the plugin's half of a plugin-served request. The kernel's half
// — matching the path, checking the menu entry's permission, and verifying the
// `_token` on a state-changing method — happens in `routes/plugin_api.rs`
// BEFORE the tap is dispatched, so the two halves are tested separately here:
// the tap through the real module against a real Postgres, and the token gate
// against the kernel's own function with a real session.
//
// What matters about these tests is the *pair* of assertions each write makes.
// A device is two tiers, and a form that updated the Item and not the row would
// leave the daemon acting on stale values with a UI that says otherwise. Every
// write below is checked on both sides.

const FORM_RENAME: &str = "/netgrasp/device/rename";
const FORM_OWNER: &str = "/netgrasp/device/owner";
const FORM_HIDDEN: &str = "/netgrasp/device/hidden";
const FORM_NOTIFY: &str = "/netgrasp/device/notify";

/// Dispatch one `tap_api` request and return the `ApiResponse` as JSON.
///
/// `csrf_token` is whatever the kernel would have minted; the tap never checks
/// it, because by the time a tap runs the kernel has already accepted it. That
/// is the contract, and `a_post_with_no_token_never_reaches_the_plugin` below is
/// what holds the other side of it.
async fn call_form(
    pool: &PgPool,
    user: &UserContext,
    callback: &str,
    method: &str,
    path: &str,
    query: serde_json::Value,
    body: &str,
) -> serde_json::Value {
    dispatch_as(
        pool,
        user,
        "tap_api",
        &serde_json::json!({
            "callback": callback,
            "method": method,
            "path": path,
            "params": {},
            "query": query,
            "body": body,
            "user_id": user.id.to_string(),
            "authenticated": true,
            "csrf_token": "minted-by-the-kernel",
        }),
    )
    .await
}

/// A device row's two flags, read straight from the daemon's table.
async fn device_flags(pool: &PgPool, device: i64) -> (bool, bool) {
    let row = sqlx::query("SELECT hidden, notify FROM ng_devices WHERE id = $1")
        .bind(device)
        .fetch_one(pool)
        .await
        .unwrap();
    (
        row.try_get("hidden").unwrap(),
        row.try_get("notify").unwrap(),
    )
}

/// An Item's title and one of its fields.
async fn item_title_and_field(
    pool: &PgPool,
    item_id: Uuid,
    field: &str,
) -> (String, serde_json::Value) {
    let row = sqlx::query("SELECT title, fields FROM item WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let title: String = row.try_get("title").unwrap();
    let fields: serde_json::Value = row.try_get("fields").unwrap();
    let value = fields
        .get(field)
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    (title, value)
}

/// **A POST changes both tiers, and mints the Item the row never had.**
///
/// The device is seeded the way the demo seed and a long-running daemon both
/// leave one: `clean`, so the cron sync has never looked at it, and therefore
/// with no `trovato_item_id` at all. A write that addressed the row by its Item
/// link would update zero rows and report success, which is the first thing this
/// feature got wrong when the assistant grew it.
#[test]
fn a_posted_rename_changes_the_item_and_the_daemon_row_and_mints_what_is_missing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:21", None, "online").await;

        let (_, before, _, _) = device_state(&pool, device).await;
        assert!(before.is_none(), "the fixture must start with no item");

        let response = call_form(
            &pool,
            &admin,
            "device_rename_save",
            "POST",
            FORM_RENAME,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A00%3A21&back=%2Fdevices&value=Office+printer",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");

        // Tier one: the daemon's row.
        let (_, item_id, display_name, sync_state) = device_state(&pool, device).await;
        assert_eq!(display_name.as_deref(), Some("Office printer"));
        assert_eq!(
            sync_state, "clean",
            "a form write must not raise sync_state, or the loop has an edge"
        );

        // Tier two: the Item, which did not exist when the request arrived.
        let item_id = item_id.expect("the form minted the device's item");
        let (title, mac) = item_title_and_field(&pool, item_id, "field_mac").await;
        assert_eq!(title, "Office printer");
        assert_eq!(
            mac.as_str().or_else(|| mac.get("value")?.as_str()),
            Some("02:00:5e:00:00:21"),
            "the minted item carries the device's MAC"
        );
    });
}

/// The owner form writes the owner and leaves the flags alone.
///
/// The sparse-edit discipline, from the form side. A rename that turned a
/// device's alerts off is what `DeviceEdit` was made sparse for, and a form
/// posting a whole overlay would bring it back — so this asserts the two
/// columns nobody named are untouched, not merely that the one named is right.
#[test]
fn a_posted_owner_assignment_changes_the_owner_and_nothing_else() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let person = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:22", None, "online").await;

        // `notify` defaults to TRUE in the daemon's schema, which is exactly the
        // value the old whole-overlay write used to clobber with a fabricated
        // false.
        let (hidden_before, notify_before) = device_flags(&pool, device).await;
        assert!(notify_before, "the schema default is TRUE");

        let response = call_form(
            &pool,
            &admin,
            "device_owner_save",
            "POST",
            FORM_OWNER,
            serde_json::json!({}),
            &format!("target=02%3A00%3A5e%3A00%3A00%3A22&back=%2Fdevices&value={person}"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");

        let (owner, item_id, _, _) = device_state(&pool, device).await;
        assert_eq!(owner, Some(person), "the row names the new owner");

        let (hidden_after, notify_after) = device_flags(&pool, device).await;
        assert_eq!(
            (hidden_before, notify_before),
            (hidden_after, notify_after),
            "assigning an owner changed a flag nobody named"
        );

        let item_id = item_id.expect("the form minted the device's item");
        let (_, owner_field) = item_title_and_field(&pool, item_id, "field_owner").await;
        let stored = owner_field
            .as_str()
            .map(str::to_string)
            .or_else(|| Some(owner_field.get("value")?.as_str()?.to_string()));
        assert_eq!(
            stored,
            Some(person.to_string()),
            "the item and the row disagree about the owner"
        );
    });
}

/// One flag moves and the other does not, on both tiers.
#[test]
fn a_posted_flag_change_moves_one_column_and_leaves_the_other() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:23", None, "online").await;

        let (hidden_before, notify_before) = device_flags(&pool, device).await;
        assert!(!hidden_before);
        assert!(notify_before);

        let response = call_form(
            &pool,
            &admin,
            "device_hidden_save",
            "POST",
            FORM_HIDDEN,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A00%3A23&back=%2Fdevices&value=1",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");

        let (hidden_after, notify_after) = device_flags(&pool, device).await;
        assert!(hidden_after, "the device was not hidden");
        assert_eq!(notify_after, notify_before, "hiding changed the alerts");

        // And back the other way, through the other form, which must move the
        // other column and leave the first one hidden.
        let response = call_form(
            &pool,
            &admin,
            "device_notify_save",
            "POST",
            FORM_NOTIFY,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A00%3A23&back=%2Fdevices&value=0",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");

        let (hidden_end, notify_end) = device_flags(&pool, device).await;
        assert!(hidden_end, "muting unhid the device");
        assert!(!notify_end, "the device was not muted");
    });
}

/// **A caller without the permission changes nothing.**
///
/// The kernel gates the route on the menu entry's `permission` and would not
/// dispatch this at all; what is asserted here is the plugin's own check, which
/// is the one that still bites when the conversation or the request outlives the
/// grant. It is checked literally, with no `administer site` bypass
/// (`G-USER-API-NO-ADMIN-BYPASS`), which is why `ng_nobody` is refused and why
/// `ng_admin` has to carry the exact string.
#[test]
fn a_form_post_from_a_caller_without_the_permission_changes_nothing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let nobody = ng_nobody(&pool).await;
        let device = seed_clean_device(&pool, "02:00:5e:00:00:24", None, "online").await;
        let before = daemon_snapshot(&pool, device).await;

        let response = call_form(
            &pool,
            &nobody,
            "device_rename_save",
            "POST",
            FORM_RENAME,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A00%3A24&back=%2Fdevices&value=Should+not+happen",
        )
        .await;

        assert_eq!(response["status"], 403, "{response}");
        assert_eq!(
            before,
            daemon_snapshot(&pool, device).await,
            "a refused caller changed the device row"
        );
        let (_, item_id, display_name, _) = device_state(&pool, device).await;
        assert!(item_id.is_none(), "a refused caller minted an item");
        assert!(
            display_name.is_none(),
            "a refused caller renamed the device"
        );
    });
}

/// The GET renders the field, carries the kernel's token, and contains no
/// script — which is the whole claim the feature makes.
#[test]
fn the_form_a_get_renders_carries_the_token_and_no_script() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        seed_clean_device(&pool, "02:00:5e:00:00:25", None, "online").await;

        let response = call_form(
            &pool,
            &admin,
            "device_rename_form",
            "GET",
            FORM_RENAME,
            serde_json::json!({"target": "02:00:5e:00:00:25", "back": "/devices/online"}),
            "",
        )
        .await;

        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(
            response["theme"], true,
            "a page a person reads must be themed"
        );
        let body = response["body"].as_str().unwrap_or_default();
        assert!(
            body.contains(r#"name="_token" value="minted-by-the-kernel""#),
            "{body}"
        );
        assert!(body.contains(r#"name="value""#), "{body}");
        assert!(body.contains(r#"method="post""#), "{body}");
        assert!(!body.contains("<script"), "{body}");
        assert!(!body.contains("onsubmit"), "{body}");
        assert!(!body.contains("onclick"), "{body}");
        // The listing it came from is carried through, so the way back is the
        // page the person was actually on.
        assert!(
            body.contains(r#"name="back" value="/devices/online""#),
            "{body}"
        );
    });
}

/// A `back` that points off the site is dropped rather than reflected.
///
/// It arrives in a URL somebody may have been handed and it lands in an `href`
/// and in a `<meta refresh>`, so it is an open redirect if it is trusted. The
/// unit test in `forms.rs` covers the classification; this one proves the
/// hostile value never reaches the rendered page.
#[test]
fn a_back_parameter_pointing_off_the_site_is_not_reflected() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        seed_clean_device(&pool, "02:00:5e:00:00:26", None, "online").await;

        let response = call_form(
            &pool,
            &admin,
            "device_rename_form",
            "GET",
            FORM_RENAME,
            serde_json::json!({
                "target": "02:00:5e:00:00:26",
                "back": "https://evil.example/steal",
            }),
            "",
        )
        .await;

        let body = response["body"].as_str().unwrap_or_default();
        assert!(!body.contains("evil.example"), "{body}");
        assert!(body.contains(r#"name="back" value="/devices""#), "{body}");
    });
}

/// **A POST with no token never reaches the plugin.**
///
/// This is the kernel's half of the contract and cannot be asserted through the
/// tap, because the tap is what does not run: `routes/plugin_api.rs` verifies
/// the token for any state-changing method before dispatching, and answers 403
/// on its own. So the check itself is called here, with a real session and the
/// real function the route calls, over the three bodies that matter.
///
/// The plugin's side of it is that its write routes really are POSTs — a form
/// registered as a GET would skip this gate entirely — which the route
/// declarations in `forms.rs` assert.
#[test]
fn a_post_with_no_token_never_reaches_the_plugin() {
    serial(async {
        use axum::http::{HeaderMap, HeaderValue, header};
        use tower_sessions::{MemoryStore, Session};

        let session = Session::new(None, std::sync::Arc::new(MemoryStore::default()), None);
        let token = trovato_kernel::form::csrf::generate_csrf_token(&session).await;

        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );

        let check = |body: String| {
            let session = session.clone();
            let headers = headers.clone();
            async move {
                trovato_kernel::routes::helpers::require_csrf_header_or_field(
                    &session, &headers, &body,
                )
                .await
                .is_ok()
            }
        };

        assert!(
            !check("target=02%3A00%3A5e%3A00%3A00%3A27&value=Renamed".to_string()).await,
            "a body with no _token was accepted"
        );
        assert!(
            !check("_token=not-a-real-token&value=Renamed".to_string()).await,
            "a forged _token was accepted"
        );
        // The valid one, last, because verification consumes it — which is also
        // why a form re-rendered after a failure needs the fresh token the
        // kernel minted for that request rather than the one that arrived.
        assert!(
            check(format!("_token={token}&value=Renamed")).await,
            "the token the kernel minted was refused"
        );
        assert!(
            !check(format!("_token={token}&value=Renamed")).await,
            "a spent token was accepted a second time"
        );
    });
}

/// **The navigation, applied to a real kernel database**: the site menu is
/// exactly the four rows the kernel will draw, /devices opens on what is online,
/// the full list moved to /devices/all, and /who-is-home is one 301 to the Home
/// now tab, however many times the migrations have run (every test here runs
/// them all again on the same database).
#[test]
fn the_navigation_migrations_write_five_menu_rows_the_aliases_and_one_redirect() {
    serial(async {
        let pool = fresh_pool().await;

        let menu: Vec<(String, String, i32)> = sqlx::query_as(
            "SELECT path::text, title::text, weight FROM menu_link \
             WHERE menu_name = 'main' AND stage_id = '0193a5a0-0000-7000-8000-000000000001' \
             ORDER BY weight, title",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let titles: Vec<&str> = menu.iter().map(|m| m.1.as_str()).collect();
        assert_eq!(
            titles,
            ["Home", "Devices", "People", "Alerts", "Activity"],
            "{menu:?}"
        );

        let source = |alias: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT source::text FROM url_alias WHERE alias = $1 AND language = 'en'",
                )
                .bind(alias)
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|e| panic!("{alias} has no alias: {e}"))
            }
        };
        assert_eq!(source("/devices").await, "/gather/ng_device_online");
        assert_eq!(source("/devices/online").await, "/gather/ng_device_online");
        assert_eq!(source("/devices/all").await, "/gather/ng_device_list");
        assert_eq!(source("/activity").await, "/gather/ng_activity");
        // The page that went away keeps its alias; the redirect answers first.
        assert_eq!(source("/who-is-home").await, "/gather/ng_who_is_home");

        let redirects: Vec<(String, i16)> = sqlx::query_as(
            "SELECT destination::text, status_code FROM redirect WHERE source = '/who-is-home'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(redirects, [("/people/home".to_string(), 301)]);

        let canonical: String = sqlx::query_scalar(
            "SELECT display ->> 'canonical_url' FROM gather_query WHERE query_id = 'ng_device_list'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(canonical, "/devices/all");
    });
}

/// **Activity's default tab** lists the arrivals, departures, new devices and
/// the three actionable security events, and none of the routine or noisy
/// ones, and renders them in the event table with its tabs.
#[test]
fn activity_lists_the_notable_events_and_leaves_out_the_noise() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:00:31", None, "online").await;
        for event_type in [
            "ip_changed",
            "ip_changed",
            "arp_scan",
            "identity_change",
            "name_updated",
            "new_device",
            "person_arrived",
            "person_departed",
            "arp_spoof",
            "ip_conflict",
            "rogue_dhcp",
        ] {
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
        let rows = gather_items(&gather, "ng_activity", HashMap::new()).await;
        let mut types: Vec<String> = rows
            .iter()
            .map(|r| r["event_type"].as_str().unwrap_or_default().to_string())
            .collect();
        types.sort();
        let mut expected: Vec<String> = netgrasp_core::model::NOTABLE_EVENT_TYPES
            .iter()
            .map(|t| (*t).to_string())
            .collect();
        expected.sort();
        assert_eq!(types, expected);
        assert_eq!(
            run_gather(&gather, &pool, "ng_event_log", HashMap::new()).await,
            11,
            "Everything is still everything"
        );

        let html = render_page("ng_activity", "Activity", "/activity", &rows);
        assert!(html.contains("ng-tabs__tab--active"), "{html}");
        assert!(html.contains("ng-row--security"), "{html}");
        assert!(html.contains("ng-menu__button"), "{html}");
    });
}

/// The rows a device gather returns for one search, given the way the kernel's
/// gather route gives it: as an exposed filter value keyed by the filter's
/// field, `search`, which is also the query-string key the search box sends.
async fn searched(
    gather: &GatherService,
    query_id: &str,
    search: &str,
    url_args: HashMap<String, String>,
) -> Vec<String> {
    let context = QueryContext {
        url_args,
        viewer: Some(network_viewer()),
        ..QueryContext::default()
    };
    let exposed = if search.is_empty() {
        HashMap::new()
    } else {
        HashMap::from([(
            "search".to_string(),
            FilterValue::String(search.to_string()),
        )])
    };
    let mut macs: Vec<String> = gather
        .execute(
            query_id,
            1,
            exposed,
            Uuid::parse_str(LIVE_STAGE).unwrap(),
            &context,
        )
        .await
        .unwrap_or_else(|e| panic!("gather {query_id} failed: {e:#}"))
        .items
        .iter()
        .map(|r| r["mac"].as_str().unwrap_or_default().to_string())
        .collect();
    macs.sort();
    macs
}

/// **Search, on every Devices tab, against the real gathers**: a word finds
/// only the devices it is in, whichever column it is in and whatever its case;
/// a blank search finds everything the tab would show anyway; and the tab's
/// own filters still apply under a search.
#[test]
fn the_device_search_finds_only_matching_rows_on_every_tab_and_blank_finds_all() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let motivo = seed_device(&pool, "aa:bb:cc:00:00:41", Some("motivo"), "online").await;
        let printer = seed_device(&pool, "aa:bb:cc:00:00:42", Some("printer"), "online").await;
        let hidden = seed_device(&pool, "aa:bb:cc:00:00:43", Some("motivo-backup"), "online").await;
        let offline = seed_device(&pool, "aa:bb:cc:00:00:44", Some("garage-pi"), "offline").await;
        sqlx::query("UPDATE ng_devices SET hidden = true WHERE id = $1")
            .bind(hidden)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE ng_devices SET vendor = 'Raspberry Pi Trading', last_ip = '10.0.1.12' WHERE id IN ($1, $2)")
            .bind(motivo)
            .bind(offline)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE ng_devices SET display_name = 'Office Printer' WHERE id = $1")
            .bind(printer)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let none = HashMap::new;

        // A hostname, any case, on the Online tab and on All.
        for query_id in ["ng_device_online", "ng_device_list", "ng_devices_todo"] {
            assert_eq!(
                searched(&gather, query_id, "MOTIVO", none()).await,
                ["aa:bb:cc:00:00:41"],
                "{query_id}: the hidden motivo-backup must stay hidden"
            );
        }
        // Blank is everything the tab shows.
        assert_eq!(
            searched(&gather, "ng_device_list", "", none()).await,
            [
                "aa:bb:cc:00:00:41",
                "aa:bb:cc:00:00:42",
                "aa:bb:cc:00:00:44"
            ]
        );
        assert_eq!(
            searched(&gather, "ng_device_online", "", none()).await,
            ["aa:bb:cc:00:00:41", "aa:bb:cc:00:00:42"]
        );
        // A vendor, a name a person gave, part of a MAC, and an address.
        assert_eq!(
            searched(&gather, "ng_device_list", "raspberry", none()).await,
            ["aa:bb:cc:00:00:41", "aa:bb:cc:00:00:44"]
        );
        assert_eq!(
            searched(&gather, "ng_device_list", "office printer", none()).await,
            ["aa:bb:cc:00:00:42"]
        );
        assert_eq!(
            searched(&gather, "ng_device_list", "00:44", none()).await,
            ["aa:bb:cc:00:00:44"]
        );
        assert_eq!(
            searched(&gather, "ng_device_list", "10.0.1.12", none()).await,
            ["aa:bb:cc:00:00:41", "aa:bb:cc:00:00:44"]
        );
        assert!(
            searched(&gather, "ng_device_list", "nothing-called-this", none())
                .await
                .is_empty()
        );
        // The Hidden tab searches the hidden ones, and only them.
        assert_eq!(
            searched(&gather, "ng_devices_hidden", "", none()).await,
            ["aa:bb:cc:00:00:43"]
        );
        assert_eq!(
            searched(&gather, "ng_devices_hidden", "motivo", none()).await,
            ["aa:bb:cc:00:00:43"]
        );
        // New, By place and By type take the same search.
        assert_eq!(
            searched(&gather, "ng_devices_new", "motivo", none()).await,
            ["aa:bb:cc:00:00:41"]
        );
        assert_eq!(
            searched(&gather, "ng_devices_by_location", "garage", none()).await,
            ["aa:bb:cc:00:00:44"]
        );
        assert_eq!(
            searched(
                &gather,
                "ng_device_by_type",
                "printer",
                HashMap::from([("device_type".to_string(), "phone".to_string())]),
            )
            .await,
            ["aa:bb:cc:00:00:42"],
            "a search under a type facet keeps the facet"
        );
    });
}

/// By type with no type is every visible device, grouped by type in the
/// order the template draws: by type, and the untyped last.
#[test]
fn by_type_with_no_type_lists_every_visible_device_typed_ones_first() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let a = seed_device(&pool, "aa:bb:cc:00:00:51", Some("a"), "online").await;
        let b = seed_device(&pool, "aa:bb:cc:00:00:52", Some("b"), "online").await;
        let c = seed_device(&pool, "aa:bb:cc:00:00:53", Some("c"), "online").await;
        let d = seed_device(&pool, "aa:bb:cc:00:00:54", Some("d"), "online").await;
        for (id, kind) in [(a, Some("tv")), (b, None), (c, Some("laptop"))] {
            sqlx::query("UPDATE ng_devices SET device_type = $1 WHERE id = $2")
                .bind(kind)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE ng_devices SET hidden = true WHERE id = $1")
            .bind(d)
            .execute(&pool)
            .await
            .unwrap();

        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_device_by_type", HashMap::new()).await;
        let kinds: Vec<Option<String>> = rows
            .iter()
            .map(|r| r["device_type"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            kinds,
            [Some("laptop".to_string()), Some("tv".to_string()), None],
            "{rows:?}"
        );
        let html = render_page(
            "ng_device_by_type",
            "Devices by type",
            "/devices/type",
            &rows,
        );
        assert!(html.contains("id=\"type-laptop\""), "{html}");
        assert!(html.contains("Not yet identified"), "{html}");
    });
}

/// **Home never lists more than six devices to sort**, however many arrived
/// this week, and says how many more there are.
#[test]
fn home_lists_at_most_six_devices_to_sort_and_counts_the_rest() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        for i in 0..9 {
            seed_device(&pool, &format!("aa:bb:cc:00:02:{i:02x}"), None, "online").await;
        }
        let gather = wire_gather(&pool).await;
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        let ov = &rows[0];
        assert_eq!(ov["attention"].as_array().map(Vec::len), Some(6), "{ov}");
        assert_eq!(ov["devices_unsorted"], 9, "{ov}");
        let html = render_page("ng_overview", "Home", "/overview", &rows);
        assert!(html.contains("3 more devices have no owner."), "{html}");
        assert_eq!(html.matches("class=\"ng-assign\"").count(), 6, "{html}");
    });
}

/// **An anonymous visitor reads no device data** (docs/UI.md, U1 check 7):
/// through the real kernel gather and the plugin's real `tap_field_access`,
/// every device, event and Home row reaches an anonymous viewer with no MAC,
/// no name and no address in it, and the page asks them to sign in. A viewer
/// with the permission sees everything, and so does an administrator.
#[test]
fn an_anonymous_viewer_gets_rows_with_nothing_in_them_and_a_sign_in() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let gather = wire_gather(&pool).await;

        for (query_id, base) in [
            ("ng_device_online", "/devices"),
            ("ng_device_list", "/devices/all"),
            ("ng_event_log", "/events"),
            ("ng_activity", "/activity"),
            ("ng_overview", "/overview"),
            ("ng_people_home", "/people/home"),
        ] {
            let anon =
                gather_items_as(&gather, query_id, HashMap::new(), UserContext::anonymous()).await;
            assert!(!anon.is_empty(), "{query_id} returned no rows at all");
            let text = serde_json::to_string(&anon).unwrap();
            for secret in ["aa:bb:cc", "192.168.1.10", "Jamie", "jamie-phone", "Apple"] {
                assert!(
                    !text.contains(secret),
                    "{query_id} showed {secret} to an anonymous viewer: {text}"
                );
            }
            let html = render_page(query_id, "Netgrasp", base, &anon);
            assert!(
                html.contains("Sign in to see the devices on this network."),
                "{query_id}: {html}"
            );
            assert!(!html.contains("aa:bb:cc"), "{query_id}: {html}");

            let seen = gather_items(&gather, query_id, HashMap::new()).await;
            let text = serde_json::to_string(&seen).unwrap();
            assert!(
                text.contains("aa:bb:cc") || text.contains("Jamie") || text.contains("event_type"),
                "{query_id} hid data from a viewer with the permission: {text}"
            );
        }

        // The permission is a role grant, and the anonymous role no longer
        // holds it after 013.
        let anonymous: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM role_permissions \
             WHERE role_id = '00000000-0000-0000-0000-000000000001' \
               AND permission = 'view netgrasp devices'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(anonymous, 0);
    });
}

// ===========================================================================
// People (U2): assign with create, Add person, bulk, merge, delete
// ===========================================================================
//
// Every form below writes through `assist_host`, the assistant's own path, so
// what these prove is the forms' half: the right people are found or created,
// the right devices move on both tiers, and a write that fails part way leaves
// nothing changed. Failures are injected with triggers, because the plugin's
// writes are real kernel and database calls and there is no seam to fake one.

const FORM_PERSON_ADD: &str = "/people/add";
const FORM_ASSIGN: &str = "/devices/assign";
const FORM_MERGE: &str = "/netgrasp/person/merge";
const FORM_DELETE: &str = "/netgrasp/person/delete";

/// An Item field as text, whichever of the two shapes the kernel stored it in,
/// and `None` for absent or blank.
fn field_text(value: &serde_json::Value) -> Option<String> {
    let raw = value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.get("value")?.as_str().map(str::to_string))?;
    if raw.is_empty() { None } else { Some(raw) }
}

/// A device's owner on both tiers: the daemon row and its Item.
async fn owner_on_both_tiers(pool: &PgPool, device: i64) -> (Option<Uuid>, Option<String>) {
    let (owner, item_id, _, _) = device_state(pool, device).await;
    let field = match item_id {
        Some(item_id) => field_text(&item_title_and_field(pool, item_id, "field_owner").await.1),
        None => None,
    };
    (owner, field)
}

/// How many people are called this, by the case-insensitive rule, as Items and
/// as mirror rows.
async fn people_named(pool: &PgPool, name: &str) -> (i64, i64) {
    let items: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM item WHERE type = 'ng_person' AND lower(trim(title)) = lower(trim($1))",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    let mirrors: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ng_people WHERE lower(trim(name)) = lower(trim($1))",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    (items, mirrors)
}

/// The one person called this, as the mirror row has them.
async fn person_id_named(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT item_id FROM ng_people WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn person_exists(pool: &PgPool, id: Uuid) -> (bool, bool) {
    let item: i64 = sqlx::query_scalar("SELECT count(*) FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    let mirror: i64 = sqlx::query_scalar("SELECT count(*) FROM ng_people WHERE item_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    (item == 1, mirror == 1)
}

/// Set a person's two alert flags on the Item and the mirror, as the notify
/// form would have.
async fn set_alerts(pool: &PgPool, id: Uuid, arrive: bool, depart: bool) {
    sqlx::query(
        "UPDATE item SET fields = fields || jsonb_build_object(\
             'field_notify_arrive', $2::boolean, 'field_notify_depart', $3::boolean) \
         WHERE id = $1",
    )
    .bind(id)
    .bind(arrive)
    .bind(depart)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ng_people SET notify_arrive = $2, notify_depart = $3 WHERE item_id = $1")
        .bind(id)
        .bind(arrive)
        .bind(depart)
        .execute(pool)
        .await
        .unwrap();
}

async fn alerts_of(pool: &PgPool, id: Uuid) -> (bool, bool) {
    let row = sqlx::query("SELECT notify_arrive, notify_depart FROM ng_people WHERE item_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

/// A person arrival the daemon recorded, carrying the person's id and name in
/// `details` the way the daemon writes them.
async fn seed_arrival(pool: &PgPool, device: i64, person: Uuid, name: &str) {
    sqlx::query(
        // The person in both places the daemon puts them since its V5: the
        // details and the `person_item_id` column (022 gives a standalone
        // schema the column, and 019's movements view reads it when present).
        "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details, person_item_id) \
         VALUES ($1, 'person_arrived', now() - interval '1 hour', \
                 jsonb_build_object('person_item_id', $2::text, 'person', $3::text, 'location', 'Kitchen'), \
                 $2::uuid)",
    )
    .bind(device)
    .bind(person.to_string())
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// Make the daemon row of `mac` refuse to change owner to `to`.
///
/// Raised only on that change, so putting the owner back is still allowed —
/// which is the undo the forms are meant to perform.
async fn fail_owner_change(pool: &PgPool, mac: &str, to: Uuid) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE OR REPLACE FUNCTION u2_fail_device() RETURNS trigger AS $$ \
         BEGIN \
           IF NEW.mac = '{mac}' AND NEW.owner_item_id IS NOT DISTINCT FROM '{to}'::uuid \
              AND OLD.owner_item_id IS DISTINCT FROM NEW.owner_item_id THEN \
             RAISE EXCEPTION 'injected failure moving {mac}'; \
           END IF; \
           RETURN NEW; \
         END $$ LANGUAGE plpgsql; \
         DROP TRIGGER IF EXISTS u2_fail_device ON ng_devices; \
         CREATE TRIGGER u2_fail_device BEFORE UPDATE ON ng_devices \
           FOR EACH ROW EXECUTE FUNCTION u2_fail_device();"
    )))
    .execute(pool)
    .await
    .unwrap();
}

/// Make the kernel refuse to delete one Item: the last step of a merge and of
/// a delete, and the one with no undo of its own.
async fn fail_item_delete(pool: &PgPool, id: Uuid) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE OR REPLACE FUNCTION u2_fail_item() RETURNS trigger AS $$ \
         BEGIN \
           IF OLD.id = '{id}'::uuid THEN RAISE EXCEPTION 'injected failure deleting {id}'; END IF; \
           RETURN OLD; \
         END $$ LANGUAGE plpgsql; \
         DROP TRIGGER IF EXISTS u2_fail_item ON item; \
         CREATE TRIGGER u2_fail_item BEFORE DELETE ON item \
           FOR EACH ROW EXECUTE FUNCTION u2_fail_item();"
    )))
    .execute(pool)
    .await
    .unwrap();
}

/// A device with an owner on both tiers, through the owner form, so its Item
/// carries the owner the way a real one would.
async fn owned_device(pool: &PgPool, admin: &UserContext, mac: &str, owner: Uuid) -> i64 {
    let device = seed_clean_device(pool, mac, None, "online").await;
    let response = call_form(
        pool,
        admin,
        "device_owner_save",
        "POST",
        FORM_OWNER,
        serde_json::json!({}),
        &format!("target={mac}&back=%2Fdevices&value={owner}"),
    )
    .await;
    assert_eq!(response["status"], 200, "{response}");
    assert_eq!(
        owner_on_both_tiers(pool, device).await,
        (Some(owner), Some(owner.to_string()))
    );
    device
}

/// **Render**: the owner form lists everybody by name, ignoring case, offers a
/// "new person" field, and carries the filter box hidden for the script.
#[test]
fn the_owner_form_lists_people_sorted_and_offers_a_new_person_field() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        seed_person_item(&pool, "bob").await;
        seed_person_item(&pool, "Alice").await;
        seed_clean_device(&pool, "02:00:5e:00:01:01", None, "online").await;

        let response = call_form(
            &pool,
            &admin,
            "device_owner_form",
            "GET",
            FORM_OWNER,
            serde_json::json!({"target": "02:00:5e:00:01:01", "back": "/devices"}),
            "",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let body = response["body"].as_str().unwrap_or_default();
        assert!(body.contains(r#"name="new_person""#), "{body}");
        assert!(body.contains("or a new person named"), "{body}");
        assert!(
            body.contains(r#"name="_token" value="minted-by-the-kernel""#),
            "{body}"
        );
        let alice = body.find(">Alice<").expect("Alice listed");
        let bob = body.find(">bob<").expect("bob listed");
        assert!(alice < bob, "people are not sorted by name: {body}");
        assert!(body.contains("data-ng-filter=\"ng-value\""), "{body}");
        assert!(
            body.contains("Search people\" hidden>"),
            "the filter box is visible without the script: {body}"
        );
        assert!(body.contains("/static/js/netgrasp-forms.js"), "{body}");
    });
}

/// **Save, create**: a name nobody has creates the person through the
/// assistant's function, and the device is theirs on both tiers.
#[test]
fn a_new_person_named_on_the_owner_form_is_created_and_owns_the_device() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let device = seed_clean_device(&pool, "02:00:5e:00:01:02", None, "online").await;
        let (_, notify_before) = device_flags(&pool, device).await;

        let response = call_form(
            &pool,
            &admin,
            "device_owner_save",
            "POST",
            FORM_OWNER,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A01%3A02&back=%2Fdevices&value=&new_person=Jamie",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(
            response["body"]
                .as_str()
                .unwrap_or_default()
                .contains("who is new here"),
            "{response}"
        );

        assert_eq!(people_named(&pool, "Jamie").await, (1, 1));
        let jamie = person_id_named(&pool, "Jamie").await;
        assert_eq!(
            owner_on_both_tiers(&pool, device).await,
            (Some(jamie), Some(jamie.to_string()))
        );
        assert_eq!(device_flags(&pool, device).await.1, notify_before);
        // Created the way the assistant creates: alerts off.
        assert_eq!(alerts_of(&pool, jamie).await, (false, false));
    });
}

/// **The duplicate rule**: "jamie " is Jamie. The owner form assigns to the
/// existing person and says so; Add person refuses and links to them; nobody
/// second is created by either.
#[test]
fn a_name_differing_only_in_case_or_space_is_the_existing_person() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:01:03", None, "online").await;

        let response = call_form(
            &pool,
            &admin,
            "device_owner_save",
            "POST",
            FORM_OWNER,
            serde_json::json!({}),
            "target=02%3A00%3A5e%3A00%3A01%3A03&back=%2Fdevices&value=&new_person=jamie+",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let body = response["body"].as_str().unwrap_or_default();
        assert!(
            body.contains("the existing <strong>Jamie</strong>"),
            "{body}"
        );
        assert_eq!(people_named(&pool, "Jamie").await, (1, 1), "a second Jamie");
        assert_eq!(
            owner_on_both_tiers(&pool, device).await,
            (Some(jamie), Some(jamie.to_string()))
        );

        for typed in ["JAMIE", "%20%20jamie%20"] {
            let response = call_form(
                &pool,
                &admin,
                "person_add_save",
                "POST",
                FORM_PERSON_ADD,
                serde_json::json!({}),
                &format!("back=%2Fpeople&name={typed}"),
            )
            .await;
            assert_eq!(response["status"], 422, "{typed}: {response}");
            let body = response["body"].as_str().unwrap_or_default();
            assert!(body.contains(&format!("/item/{jamie}")), "{body}");
            assert_eq!(people_named(&pool, "Jamie").await, (1, 1), "{typed}");
        }
    });
}

/// **Two creates at once**: "Jamie" and "jamie " posted together leave one
/// person Item and one mirror row, and both callers are given that person.
///
/// The race is made certain rather than hoped for. A transaction holds
/// `ng_people` in SHARE mode, which lets both requests read it (so both pass
/// the plugin's own name check, finding nobody) and stops both at the mirror
/// insert. Once both are waiting the lock is released: one insert lands, and
/// 018's index refuses the other, whose request must take back the Item it
/// made and answer with the winner.
#[test]
fn two_simultaneous_creates_of_one_name_leave_one_person() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let mut gate = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE ng_people IN SHARE MODE")
            .execute(&mut *gate)
            .await
            .unwrap();

        let post = |typed: &'static str| {
            let pool = pool.clone();
            let admin = admin.clone();
            tokio::spawn(async move {
                call_form(
                    &pool,
                    &admin,
                    "person_add_save",
                    "POST",
                    FORM_PERSON_ADD,
                    serde_json::json!({}),
                    &format!("back=%2Fpeople&name={typed}"),
                )
                .await
            })
        };
        let one = post("Jamie");
        let two = post("jamie%20");

        // Both are past the name check, have made their Item, and are queued
        // on the mirror insert. Polled well inside the db host's 5 s
        // statement timeout, which the wait counts against.
        let mut waiting = 0;
        for _ in 0..200 {
            waiting = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_locks WHERE relation = 'ng_people'::regclass \
                 AND NOT granted",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(waiting, 2, "both creates should be waiting on the mirror");
        assert_eq!(
            people_named(&pool, "Jamie").await.0,
            2,
            "both creates should have made an Item before the mirror write"
        );
        gate.commit().await.unwrap();

        let one = one.await.unwrap();
        let two = two.await.unwrap();

        assert_eq!(
            people_named(&pool, "Jamie").await,
            (1, 1),
            "a second Jamie survived: {one} {two}"
        );
        // Whichever post won, so "Jamie" or "jamie": the one mirror row.
        let jamie: Uuid = sqlx::query_scalar("SELECT item_id FROM ng_people")
            .fetch_one(&pool)
            .await
            .unwrap();
        let items: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM item WHERE type = 'ng_person'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(items, [jamie], "the Item left is not the mirrored one");

        // One was created and one found, and both name the same person.
        let mut statuses = [one["status"].as_i64(), two["status"].as_i64()];
        statuses.sort_unstable();
        assert_eq!(statuses, [Some(200), Some(422)], "{one} {two}");
        for response in [&one, &two] {
            let body = response["body"].as_str().unwrap_or_default();
            assert!(body.contains(&format!("/item/{jamie}")), "{body}");
        }
        let lost = if one["status"] == 422 { &one } else { &two };
        assert!(
            lost["body"]
                .as_str()
                .unwrap_or_default()
                .contains("already exists"),
            "{lost}"
        );
    });
}

/// **Add person**: renders with the token and the arrival tick, and creates
/// one person with arrival alerts as ticked.
#[test]
fn add_person_renders_and_creates_one_person_with_alerts_as_ticked() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;

        let form = call_form(
            &pool,
            &admin,
            "person_add_form",
            "GET",
            FORM_PERSON_ADD,
            serde_json::json!({}),
            "",
        )
        .await;
        assert_eq!(form["status"], 200, "{form}");
        let body = form["body"].as_str().unwrap_or_default();
        assert!(body.contains(r#"name="name""#), "{body}");
        assert!(body.contains("Alert me when they get home"), "{body}");
        assert!(
            body.contains(r#"name="_token" value="minted-by-the-kernel""#),
            "{body}"
        );

        let saved = call_form(
            &pool,
            &admin,
            "person_add_save",
            "POST",
            FORM_PERSON_ADD,
            serde_json::json!({}),
            "back=%2Fpeople&name=++Wren++&arrive=1",
        )
        .await;
        assert_eq!(saved["status"], 200, "{saved}");
        let wren = person_id_named(&pool, "Wren").await;
        assert_eq!(alerts_of(&pool, wren).await, (true, false));
        assert_eq!(people_named(&pool, "Wren").await, (1, 1));
        let (title, _) = item_title_and_field(&pool, wren, "field_notes").await;
        assert_eq!(title, "Wren", "the name was stored untrimmed");

        let blank = call_form(
            &pool,
            &admin,
            "person_add_save",
            "POST",
            FORM_PERSON_ADD,
            serde_json::json!({}),
            "back=%2Fpeople&name=+++",
        )
        .await;
        assert_eq!(blank["status"], 422, "{blank}");
    });
}

/// Give a seeded device an mDNS name, as the daemon does.
async fn set_mdns(pool: &PgPool, device: i64, name: &str) {
    sqlx::query("UPDATE ng_devices SET mdns_name = $2 WHERE id = $1")
        .bind(device)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

/// **Bulk, render**: the devices nobody named or owns, with a checkbox each,
/// and a suggestion line per possessive mDNS owner that pre-ticks theirs.
#[test]
fn the_bulk_form_lists_the_devices_to_sort_and_suggests_owners_from_mdns() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        let mac1 = seed_clean_device(&pool, "02:00:5e:00:02:01", None, "online").await;
        let mac2 = seed_clean_device(&pool, "02:00:5e:00:02:02", None, "offline").await;
        let other = seed_clean_device(&pool, "02:00:5e:00:02:03", None, "online").await;
        seed_clean_device(&pool, "02:00:5e:00:02:04", Some(jamie), "online").await;
        set_mdns(&pool, mac1, "Jeremy's Mac Studio").await;
        set_mdns(&pool, mac2, "jeremy\u{2019}s iPad").await;
        set_mdns(&pool, other, "JAMIE's iPhone").await;

        let response = call_form(
            &pool,
            &admin,
            "devices_assign_form",
            "GET",
            FORM_ASSIGN,
            serde_json::json!({"suggest": "jeremy"}),
            "",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let body = response["body"].as_str().unwrap_or_default();
        assert!(
            body.contains("3 devices with no name and no owner"),
            "{body}"
        );
        assert!(
            !body.contains("02:00:5e:00:02:04"),
            "an owned device is listed: {body}"
        );
        assert!(
            body.contains("2 devices are named &ldquo;Jeremy&rsquo;s"),
            "{body}"
        );
        assert!(body.contains("/devices/assign?suggest=jamie"), "{body}");
        assert!(
            body.contains(r#"value="02:00:5e:00:02:01" checked"#)
                && body.contains(r#"value="02:00:5e:00:02:02" checked"#),
            "the suggestion did not tick Jeremy's devices: {body}"
        );
        assert!(
            !body.contains(r#"value="02:00:5e:00:02:03" checked"#),
            "the suggestion ticked somebody else's device: {body}"
        );
        // Nobody is called Jeremy yet, so the suggestion fills in the name.
        assert!(
            body.contains(r#"name="new_person" value="Jeremy""#),
            "{body}"
        );

        // Jamie exists, so suggesting Jamie selects her rather than typing.
        let response = call_form(
            &pool,
            &admin,
            "devices_assign_form",
            "GET",
            FORM_ASSIGN,
            serde_json::json!({"suggest": "jamie"}),
            "",
        )
        .await;
        let body = response["body"].as_str().unwrap_or_default();
        assert!(
            body.contains(&format!(r#"value="{jamie}" selected"#)),
            "{body}"
        );
    });
}

/// **Bulk, save**: every ticked device goes to one person, created on the spot,
/// on both tiers; the unticked one does not move.
#[test]
fn the_bulk_form_assigns_every_ticked_device_to_one_new_person() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let a = seed_clean_device(&pool, "02:00:5e:00:03:01", None, "online").await;
        let b = seed_clean_device(&pool, "02:00:5e:00:03:02", None, "online").await;
        let c = seed_clean_device(&pool, "02:00:5e:00:03:03", None, "online").await;

        let response = call_form(
            &pool,
            &admin,
            "devices_assign_save",
            "POST",
            FORM_ASSIGN,
            serde_json::json!({}),
            "_token=t&back=%2Fdevices%2Ftodo&device=02%3A00%3A5e%3A00%3A03%3A01\
             &device=02%3A00%3A5e%3A00%3A03%3A02&owner=&new_person=Jeremy",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(
            response["body"]
                .as_str()
                .unwrap_or_default()
                .contains("2 devices now belong"),
            "{response}"
        );
        let jeremy = person_id_named(&pool, "Jeremy").await;
        for device in [a, b] {
            assert_eq!(
                owner_on_both_tiers(&pool, device).await,
                (Some(jeremy), Some(jeremy.to_string()))
            );
        }
        assert_eq!(owner_on_both_tiers(&pool, c).await.0, None);

        // Nobody ticked, or nobody named: refused, nothing changes.
        let response = call_form(
            &pool,
            &admin,
            "devices_assign_save",
            "POST",
            FORM_ASSIGN,
            serde_json::json!({}),
            "back=%2Fdevices%2Ftodo&device=02%3A00%3A5e%3A00%3A03%3A03&owner=&new_person=",
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        assert_eq!(owner_on_both_tiers(&pool, c).await.0, None);
    });
}

/// **Bulk, failure**: the second device refuses, so the first is put back and
/// the person created for them is taken back too. Nothing half done.
#[test]
fn a_bulk_assign_that_fails_part_way_changes_nothing_and_creates_nobody() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let wren = seed_person_item(&pool, "Wren").await;
        let a = seed_clean_device(&pool, "02:00:5e:00:04:01", None, "online").await;
        let b = seed_clean_device(&pool, "02:00:5e:00:04:02", None, "online").await;
        fail_owner_change(&pool, "02:00:5e:00:04:02", wren).await;

        // To an existing person: the first device is put back.
        let response = call_form(
            &pool,
            &admin,
            "devices_assign_save",
            "POST",
            FORM_ASSIGN,
            serde_json::json!({}),
            &format!(
                "back=%2Fdevices%2Ftodo&device=02%3A00%3A5e%3A00%3A04%3A01\
                 &device=02%3A00%3A5e%3A00%3A04%3A02&owner={wren}"
            ),
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        for device in [a, b] {
            assert_eq!(owner_on_both_tiers(&pool, device).await, (None, None));
        }
        assert_eq!(people_named(&pool, "Wren").await, (1, 1));

        // Make every owner change on the second device fail, then name a new
        // person: they are created, the move fails, and they are taken back.
        sqlx::raw_sql(
            "CREATE OR REPLACE FUNCTION u2_fail_device() RETURNS trigger AS $$ \
             BEGIN \
               IF NEW.mac = '02:00:5e:00:04:02' AND NEW.owner_item_id IS NOT NULL THEN \
                 RAISE EXCEPTION 'injected'; \
               END IF; \
               RETURN NEW; \
             END $$ LANGUAGE plpgsql",
        )
        .execute(&pool)
        .await
        .unwrap();
        let response = call_form(
            &pool,
            &admin,
            "devices_assign_save",
            "POST",
            FORM_ASSIGN,
            serde_json::json!({}),
            "back=%2Fdevices%2Ftodo&device=02%3A00%3A5e%3A00%3A04%3A01\
             &device=02%3A00%3A5e%3A00%3A04%3A02&owner=&new_person=Arlo",
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        assert_eq!(
            people_named(&pool, "Arlo").await,
            (0, 0),
            "Arlo was left behind"
        );
        for device in [a, b] {
            assert_eq!(owner_on_both_tiers(&pool, device).await, (None, None));
        }
    });
}

/// Two people who are the same, the first with two devices, an arrival on
/// record and arrival alerts on; the second with no alerts.
async fn two_jamies(pool: &PgPool, admin: &UserContext) -> (Uuid, Uuid, i64, i64) {
    let from = seed_person_item(pool, "Jamie (2)").await;
    let into = seed_person_item(pool, "Jamie").await;
    let d1 = owned_device(pool, admin, "02:00:5e:00:05:01", from).await;
    let d2 = owned_device(pool, admin, "02:00:5e:00:05:02", from).await;
    seed_arrival(pool, d1, from, "Jamie (2)").await;
    set_alerts(pool, from, true, false).await;
    (from, into, d1, d2)
}

/// **Merge, render**: the confirmation names both people, every device that
/// moves, the history, and which alerts win, with the keep box ticked because
/// the merged person had an alert the kept one does not. The chooser before
/// it lists everybody but the person being merged.
#[test]
fn the_merge_page_shows_exactly_what_will_move_before_anything_changes() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let (from, into, d1, _) = two_jamies(&pool, &admin).await;

        let chooser = call_form(
            &pool,
            &admin,
            "person_merge_form",
            "GET",
            FORM_MERGE,
            serde_json::json!({"target": from.to_string()}),
            "",
        )
        .await;
        assert_eq!(chooser["status"], 200, "{chooser}");
        let body = chooser["body"].as_str().unwrap_or_default();
        assert!(
            body.contains(&format!(r#"<option value="{into}">Jamie</option>"#)),
            "{body}"
        );
        assert!(
            !body.contains(&format!(r#"<option value="{from}">"#)),
            "{body}"
        );

        let confirm = call_form(
            &pool,
            &admin,
            "person_merge_form",
            "GET",
            FORM_MERGE,
            serde_json::json!({"target": from.to_string(), "into": into.to_string()}),
            "",
        )
        .await;
        assert_eq!(confirm["status"], 200, "{confirm}");
        let body = confirm["body"].as_str().unwrap_or_default();
        for expected in [
            "Merge Jamie (2) into Jamie",
            "2 devices move from Jamie (2) to Jamie",
            "02:00:5e:00:05:01",
            "02:00:5e:00:05:02",
            "1 past coming or going is shown as",
            "Jamie's settings win",
            r#"name="keep" value="1" checked"#,
            "had arrival alerts on",
            r#"name="_token" value="minted-by-the-kernel""#,
        ] {
            assert!(body.contains(expected), "missing {expected:?}: {body}");
        }
        // Rendering it changed nothing.
        assert_eq!(owner_on_both_tiers(&pool, d1).await.0, Some(from));
        assert_eq!(person_exists(&pool, from).await, (true, true));

        // A person cannot be merged into themselves.
        let same = call_form(
            &pool,
            &admin,
            "person_merge_form",
            "GET",
            FORM_MERGE,
            serde_json::json!({"target": from.to_string(), "into": from.to_string()}),
            "",
        )
        .await;
        assert_eq!(same["status"], 422, "{same}");
    });
}

/// **Merge, save**: every device moves on both tiers, the kept person keeps
/// the merged one's arrival alert (the box was ticked), the merged person's
/// Item and mirror row are gone, and their past arrival is now the kept
/// person's, under the kept name, with the daemon's event row untouched.
#[test]
fn merge_moves_the_devices_keeps_the_history_and_removes_the_merged_person() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let (from, into, d1, d2) = two_jamies(&pool, &admin).await;
        let details_before: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM ng_events WHERE event_type = 'person_arrived'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let response = call_form(
            &pool,
            &admin,
            "person_merge_save",
            "POST",
            FORM_MERGE,
            serde_json::json!({}),
            &format!("target={from}&into={into}&keep=1&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let body = response["body"].as_str().unwrap_or_default();
        assert!(body.contains("2 devices moved"), "{body}");

        for device in [d1, d2] {
            assert_eq!(
                owner_on_both_tiers(&pool, device).await,
                (Some(into), Some(into.to_string()))
            );
        }
        assert_eq!(alerts_of(&pool, into).await, (true, false));
        assert_eq!(person_exists(&pool, from).await, (false, false));

        let row = sqlx::query("SELECT person_item_id, person_name FROM ng_person_movements")
            .fetch_one(&pool)
            .await
            .unwrap();
        let (who, name): (Option<String>, Option<String>) = (row.get(0), row.get(1));
        assert_eq!(who, Some(into.to_string()));
        assert_eq!(name.as_deref(), Some("Jamie"));
        let details_after: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM ng_events WHERE event_type = 'person_arrived'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            details_before, details_after,
            "the daemon's event was rewritten"
        );

        // A second merge keeps the record one hop deep: Jamie into Jem, and
        // Jamie (2)'s old arrival is Jem's.
        let jem = seed_person_item(&pool, "Jem").await;
        let response = call_form(
            &pool,
            &admin,
            "person_merge_save",
            "POST",
            FORM_MERGE,
            serde_json::json!({}),
            &format!("target={into}&into={jem}&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let (who, name): (Option<String>, Option<String>) = {
            let row = sqlx::query("SELECT person_item_id, person_name FROM ng_person_movements")
                .fetch_one(&pool)
                .await
                .unwrap();
            (row.get(0), row.get(1))
        };
        assert_eq!(who, Some(jem.to_string()));
        assert_eq!(name.as_deref(), Some("Jem"));
        // Unticked this time: Jem's settings (off) win.
        assert_eq!(alerts_of(&pool, jem).await, (false, false));
        assert_eq!(owner_on_both_tiers(&pool, d1).await.0, Some(jem));
    });
}

/// Everything a merge or delete may change, for a before-and-after compare.
async fn people_state(
    pool: &PgPool,
    people: &[Uuid],
    devices: &[i64],
) -> (
    Vec<(bool, bool)>,
    Vec<(bool, bool)>,
    Vec<(Option<Uuid>, Option<String>)>,
    i64,
) {
    let mut exists = Vec::new();
    let mut alerts = Vec::new();
    for person in people {
        let e = person_exists(pool, *person).await;
        exists.push(e);
        if e.1 {
            alerts.push(alerts_of(pool, *person).await);
        }
    }
    let mut owners = Vec::new();
    for device in devices {
        owners.push(owner_on_both_tiers(pool, *device).await);
    }
    let merges: i64 = sqlx::query_scalar("SELECT count(*) FROM ng_person_merge")
        .fetch_one(pool)
        .await
        .unwrap();
    (exists, alerts, owners, merges)
}

/// **Merge, failure part way**: the second device refuses to move, so the
/// first is put back, the kept person's alerts are put back, and nothing is
/// recorded or removed.
#[test]
fn a_merge_that_fails_moving_a_device_changes_nothing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let (from, into, d1, d2) = two_jamies(&pool, &admin).await;
        let before = people_state(&pool, &[from, into], &[d1, d2]).await;
        fail_owner_change(&pool, "02:00:5e:00:05:02", into).await;

        let response = call_form(
            &pool,
            &admin,
            "person_merge_save",
            "POST",
            FORM_MERGE,
            serde_json::json!({}),
            &format!("target={from}&into={into}&keep=1&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        assert!(
            response["body"]
                .as_str()
                .unwrap_or_default()
                .contains("Nothing was changed"),
            "{response}"
        );
        assert_eq!(people_state(&pool, &[from, into], &[d1, d2]).await, before);
        let item_alert = field_text(
            &item_title_and_field(&pool, into, "field_notify_arrive")
                .await
                .1,
        );
        assert_ne!(
            item_alert.as_deref(),
            Some("true"),
            "the kept person's Item kept the alert"
        );
    });
}

/// **Merge, failure at the last step**: the merged person's Item will not
/// delete, so the record is taken back, their mirror row is written again,
/// the devices go back and the alerts go back.
#[test]
fn a_merge_whose_item_delete_fails_is_undone_completely() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let (from, into, d1, d2) = two_jamies(&pool, &admin).await;
        // An earlier merge into the person going away, which must still name
        // them once the merge is undone.
        let earlier = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ng_person_merge (from_item_id, into_item_id, from_name) VALUES ($1, $2, 'J')",
        )
        .bind(earlier)
        .bind(from)
        .execute(&pool)
        .await
        .unwrap();
        let before = people_state(&pool, &[from, into], &[d1, d2]).await;
        fail_item_delete(&pool, from).await;

        let response = call_form(
            &pool,
            &admin,
            "person_merge_save",
            "POST",
            FORM_MERGE,
            serde_json::json!({}),
            &format!("target={from}&into={into}&keep=1&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        assert_eq!(people_state(&pool, &[from, into], &[d1, d2]).await, before);
        let pointed: Uuid =
            sqlx::query_scalar("SELECT into_item_id FROM ng_person_merge WHERE from_item_id = $1")
                .bind(earlier)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pointed, from, "the earlier merge was not pointed back");
    });
}

/// The merge record, written by the statements a merge sends, in the order
/// two overlapping merges can interleave them: A into B records its row while
/// B into C is under way, after B into C would have read the merges into B.
/// The collapse that ends each merge points A at C, so A's past movements are
/// C's and not those of a person who no longer exists.
#[test]
fn overlapping_merges_leave_no_merge_naming_a_person_merged_away() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let record = |from: Uuid, into: Uuid| {
            sqlx::query(netgrasp_core::queries::RECORD_PERSON_MERGE)
                .bind(from.to_string())
                .bind(into.to_string())
                .bind("Someone")
                .execute(&pool)
        };
        record(a, b).await.unwrap();
        record(b, c).await.unwrap();
        // One pass of each merge's collapse; the second finds nothing left.
        let first = sqlx::query(netgrasp_core::queries::COLLAPSE_PERSON_MERGES)
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(first, 1);
        let second = sqlx::query(netgrasp_core::queries::COLLAPSE_PERSON_MERGES)
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(second, 0);
        let into_of = |from: Uuid| {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT into_item_id FROM ng_person_merge WHERE from_item_id = $1",
            )
            .bind(from)
            .fetch_one(&pool)
        };
        assert_eq!(into_of(a).await.unwrap(), c);
        assert_eq!(into_of(b).await.unwrap(), c);

        // Two people merged into each other at the same moment: no row is
        // pointed at itself, and the collapse stops.
        sqlx::query("TRUNCATE ng_person_merge")
            .execute(&pool)
            .await
            .unwrap();
        record(a, b).await.unwrap();
        record(b, a).await.unwrap();
        for _ in 0..3 {
            sqlx::query(netgrasp_core::queries::COLLAPSE_PERSON_MERGES)
                .execute(&pool)
                .await
                .unwrap();
        }
        let selfish: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ng_person_merge WHERE from_item_id = into_item_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(selfish, 0);
    });
}

/// A merge chain left behind (a merge stopped after its record, or two merges
/// overlapping) is repaired by the next cron run, through the plugin, and the
/// chained person's movements are the kept person's again.
#[test]
fn cron_points_a_merge_chain_at_the_person_kept() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for (from, into) in [(a, b), (b, c)] {
            sqlx::query(
                "INSERT INTO ng_person_merge (from_item_id, into_item_id, from_name) \
                 VALUES ($1, $2, 'Someone')",
            )
            .bind(from)
            .bind(into)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(run_cron(&pool).await["collapsed"], 1);
        let pointed: Uuid =
            sqlx::query_scalar("SELECT into_item_id FROM ng_person_merge WHERE from_item_id = $1")
                .bind(a)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pointed, c);
        assert_eq!(run_cron(&pool).await["collapsed"], 0);
    });
}

/// **Delete, render and save**: the confirmation lists the devices that lose
/// their owner; the POST unassigns them on both tiers, keeps them, and
/// removes the person.
#[test]
fn delete_lists_then_unassigns_the_devices_and_removes_the_person() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let arlo = seed_person_item(&pool, "Arlo").await;
        let d1 = owned_device(&pool, &admin, "02:00:5e:00:06:01", arlo).await;
        let d2 = owned_device(&pool, &admin, "02:00:5e:00:06:02", arlo).await;

        let confirm = call_form(
            &pool,
            &admin,
            "person_delete_form",
            "GET",
            FORM_DELETE,
            serde_json::json!({"target": arlo.to_string()}),
            "",
        )
        .await;
        assert_eq!(confirm["status"], 200, "{confirm}");
        let body = confirm["body"].as_str().unwrap_or_default();
        assert!(
            body.contains("These 2 devices will have no owner"),
            "{body}"
        );
        assert!(
            body.contains("02:00:5e:00:06:01") && body.contains("02:00:5e:00:06:02"),
            "{body}"
        );
        assert!(
            body.contains(r#"name="_token" value="minted-by-the-kernel""#),
            "{body}"
        );
        assert_eq!(
            person_exists(&pool, arlo).await,
            (true, true),
            "rendering deleted"
        );

        let response = call_form(
            &pool,
            &admin,
            "person_delete_save",
            "POST",
            FORM_DELETE,
            serde_json::json!({}),
            &format!("target={arlo}&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(person_exists(&pool, arlo).await, (false, false));
        for device in [d1, d2] {
            assert_eq!(owner_on_both_tiers(&pool, device).await, (None, None));
            let still_there: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ng_devices WHERE id = $1")
                    .bind(device)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(still_there, 1, "a device was deleted with its owner");
        }
    });
}

/// **Delete, failure**: the Item will not delete, so the mirror row comes back
/// and the devices are theirs again on both tiers.
#[test]
fn a_delete_that_fails_changes_nothing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let arlo = seed_person_item(&pool, "Arlo").await;
        let d1 = owned_device(&pool, &admin, "02:00:5e:00:07:01", arlo).await;
        let before = people_state(&pool, &[arlo], &[d1]).await;
        fail_item_delete(&pool, arlo).await;

        let response = call_form(
            &pool,
            &admin,
            "person_delete_save",
            "POST",
            FORM_DELETE,
            serde_json::json!({}),
            &format!("target={arlo}&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
        assert_eq!(people_state(&pool, &[arlo], &[d1]).await, before);
    });
}

/// **Permissions**: every new form, read or write, answers 403 to a caller
/// without `administer netgrasp` and to an anonymous one, and changes nothing.
#[test]
fn the_people_forms_refuse_a_caller_without_the_permission_and_anonymous() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let arlo = seed_person_item(&pool, "Arlo").await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        let d1 = owned_device(&pool, &admin, "02:00:5e:00:08:01", arlo).await;
        let loose = seed_clean_device(&pool, "02:00:5e:00:08:02", None, "online").await;
        let before = people_state(&pool, &[arlo, jamie], &[d1, loose]).await;

        let nobody = ng_nobody(&pool).await;
        let calls = [
            ("device_owner_form", "GET", FORM_OWNER, String::new()),
            (
                "device_owner_save",
                "POST",
                FORM_OWNER,
                "target=02%3A00%3A5e%3A00%3A08%3A02&value=&new_person=Mallory".to_string(),
            ),
            ("person_add_form", "GET", FORM_PERSON_ADD, String::new()),
            (
                "person_add_save",
                "POST",
                FORM_PERSON_ADD,
                "name=Mallory".to_string(),
            ),
            ("devices_assign_form", "GET", FORM_ASSIGN, String::new()),
            (
                "devices_assign_save",
                "POST",
                FORM_ASSIGN,
                format!("device=02%3A00%3A5e%3A00%3A08%3A02&owner={jamie}"),
            ),
            ("person_merge_form", "GET", FORM_MERGE, String::new()),
            (
                "person_merge_save",
                "POST",
                FORM_MERGE,
                format!("target={arlo}&into={jamie}"),
            ),
            ("person_delete_form", "GET", FORM_DELETE, String::new()),
            (
                "person_delete_save",
                "POST",
                FORM_DELETE,
                format!("target={arlo}"),
            ),
        ];
        for user in [nobody, UserContext::anonymous()] {
            for (callback, method, path, body) in &calls {
                let response = call_form(
                    &pool,
                    &user,
                    callback,
                    method,
                    path,
                    serde_json::json!({"target": arlo.to_string()}),
                    body,
                )
                .await;
                assert_eq!(response["status"], 403, "{callback}: {response}");
            }
        }
        assert_eq!(
            people_state(&pool, &[arlo, jamie], &[d1, loose]).await,
            before
        );
        assert_eq!(people_named(&pool, "Mallory").await, (0, 0));
    });
}

/// **The person page**, through the real `tap_item_view`: where they are,
/// their devices, their comings and goings (a merged person's included), and
/// the four actions.
#[test]
fn the_person_page_shows_devices_movements_alerts_and_the_actions() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let (from, into, _, _) = two_jamies(&pool, &admin).await;
        let response = call_form(
            &pool,
            &admin,
            "person_merge_save",
            "POST",
            FORM_MERGE,
            serde_json::json!({}),
            &format!("target={from}&into={into}&keep=1&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");

        let item = item_json(&pool, into).await;
        let results =
            dispatch_as_raw(&pool, &admin_viewer(&pool).await, "tap_item_view", &item).await;
        let html = serde_json::from_str::<String>(&results).unwrap();
        for expected in [
            "ng-person-page",
            "02:00:5e:00:05:01",
            "Arrived",
            "at Kitchen",
            "you are told when Jamie gets home",
            "/netgrasp/person/rename?target=",
            "/netgrasp/person/notify?target=",
            "/netgrasp/person/merge?target=",
            "/netgrasp/person/delete?target=",
            "/devices/assign?person=",
        ] {
            assert!(html.contains(expected), "missing {expected:?}: {html}");
        }

        // Device data: nothing for a viewer without the permission.
        let item = item_json(&pool, into).await;
        let results =
            dispatch_as_raw(&pool, &UserContext::anonymous(), "tap_item_view", &item).await;
        let html = serde_json::from_str::<String>(&results).unwrap();
        assert!(!html.contains("02:00:5e"), "{html}");
    });
}

/// A viewer holding the device-data permission and the admin one.
async fn admin_viewer(pool: &PgPool) -> UserContext {
    UserContext::authenticated(
        any_user(pool).await,
        vec![
            PERM_ADMINISTER.to_string(),
            "view netgrasp devices".to_string(),
        ],
    )
}

/// Dispatch a tap as a user and return its raw output.
async fn dispatch_as_raw(
    pool: &PgPool,
    user: &UserContext,
    tap: &str,
    payload: &serde_json::Value,
) -> String {
    let disp = dispatcher();
    let state = RequestState::new(user.clone(), services(pool));
    let results = disp.dispatch(tap, &payload.to_string(), state).await;
    assert_eq!(results.len(), 1, "expected exactly one {tap} result");
    results[0].output.clone()
}

// ===========================================================================
// Alerts (U3)
// ===========================================================================

const ALERTS: &str = "/alerts";
const ALERTS_ADD: &str = "/alerts/add";
const ALERTS_DEVICES: &str = "/alerts/devices";

/// A person's two alert flags on both tiers: the Item's fields and the
/// mirror the daemon reads.
async fn person_alerts_both_tiers(pool: &PgPool, id: Uuid) -> ((bool, bool), (bool, bool)) {
    let (_, arrive) = item_title_and_field(pool, id, "field_notify_arrive").await;
    let (_, depart) = item_title_and_field(pool, id, "field_notify_depart").await;
    (
        (
            arrive.as_bool().unwrap_or(false),
            depart.as_bool().unwrap_or(false),
        ),
        alerts_of(pool, id).await,
    )
}

/// What the daemon writes into its status row when ntfy is configured.
async fn seed_notify_status(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ng_notify_status \
             (id, notify_configured, notify_backend, last_delivery_ok, last_delivery_at, updated_at) \
         VALUES (1, TRUE, 'ntfy', TRUE, now() - interval '5 minutes', now())",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// **The page says every rule as a sentence**, a person's own first and then
/// their devices, then the devices nobody owns; where alerts go; and what was
/// sent, in the words it was sent with. And every switch on it is a form
/// carrying the kernel's token.
#[test]
fn the_alerts_page_says_every_rule_where_alerts_go_and_what_was_sent() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        set_alerts(&pool, jamie, true, false).await;
        let phone = seed_clean_device(&pool, "02:00:5e:00:09:01", Some(jamie), "online").await;
        sqlx::query("UPDATE ng_devices SET display_name = 'Jamie''s iPhone' WHERE id = $1")
            .bind(phone)
            .execute(&pool)
            .await
            .unwrap();
        let printer = seed_clean_device(&pool, "02:00:5e:00:09:02", None, "online").await;
        sqlx::query("UPDATE ng_devices SET display_name = 'Office printer' WHERE id = $1")
            .bind(printer)
            .execute(&pool)
            .await
            .unwrap();
        let muted = seed_clean_device(&pool, "02:00:5e:00:09:03", None, "online").await;
        sqlx::query("UPDATE ng_devices SET notify = FALSE WHERE id = $1")
            .bind(muted)
            .execute(&pool)
            .await
            .unwrap();
        seed_notify_status(&pool).await;
        sqlx::query(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details, notified, person_item_id) VALUES \
             ($1, 'person_arrived', now() - interval '5 minutes', \
              jsonb_build_object('person', 'Jamie', 'person_item_id', $2::text, \
                                 'device_name', 'Jamie''s iPhone'), TRUE, $2::uuid), \
             ($3, 'returned', now() - interval '4 minutes', '{}'::jsonb, FALSE, NULL)",
        )
        .bind(phone)
        .bind(jamie.to_string())
        .bind(printer)
        .execute(&pool)
        .await
        .unwrap();

        let response = call_form(
            &pool,
            &admin,
            "alerts_form",
            "GET",
            ALERTS,
            serde_json::json!({}),
            "",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let html = response["body"].as_str().unwrap();

        let rule = html.find("Tell me when Jamie gets home.").expect(html);
        let device = html
            .find("Tell me when Jamie&#x27;s iPhone comes online.")
            .expect(html);
        let nobodys = html
            .find("Tell me when Office printer comes online.")
            .expect(html);
        assert!(rule < device && device < nobodys, "{html}");
        assert!(html.contains("2 devices of 3 alert when they come back online."));
        assert!(html.contains("Alerts go to ntfy."), "{html}");
        assert!(html.contains("Last alert delivered today at"), "{html}");
        assert!(html.contains("Send a test alert"), "{html}");
        assert!(
            html.contains("Jamie is home: Jamie&#x27;s iPhone joined at"),
            "{html}"
        );
        assert!(
            !html.contains("Back online: Office printer"),
            "an alert that was not delivered is not listed as sent"
        );
        assert!(!html.contains("<script"), "the page needs no script");
        assert_eq!(
            html.matches("<form").count(),
            html.matches("name=\"_token\" value=\"minted-by-the-kernel\"")
                .count(),
            "every switch carries the token"
        );
    });
}

/// **Adding an alert for a person turns on exactly that alert**, on the Item
/// and in the mirror the daemon reads, through the person save every other
/// form uses. The other flag is left as it was.
#[test]
fn an_alert_added_for_a_person_turns_on_that_alert_alone_on_both_tiers() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        set_alerts(&pool, jamie, false, true).await;

        let form = call_form(
            &pool,
            &admin,
            "alerts_add_form",
            "GET",
            ALERTS_ADD,
            serde_json::json!({}),
            "",
        )
        .await;
        assert_eq!(form["status"], 200, "{form}");
        let html = form["body"].as_str().unwrap();
        assert!(html.contains(&format!("value=\"{jamie}\"")), "{html}");
        assert!(html.contains("gets home"), "{html}");

        let response = call_form(
            &pool,
            &admin,
            "alerts_add_save",
            "POST",
            ALERTS_ADD,
            serde_json::json!({}),
            &format!("kind=person&person={jamie}&when=arrive&back=%2Falerts"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(
            response["body"]
                .as_str()
                .unwrap()
                .contains("Tell me when Jamie gets home."),
            "{response}"
        );
        assert_eq!(
            person_alerts_both_tiers(&pool, jamie).await,
            ((true, true), (true, true)),
            "arrival on, departure still on, on both tiers"
        );
    });
}

/// **Adding an alert for a device turns its `notify` on** and touches no
/// other column, and an unknown device changes nothing.
#[test]
fn an_alert_added_for_a_device_turns_its_notify_on_and_nothing_else() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let device = seed_clean_device(&pool, "02:00:5e:00:09:11", None, "offline").await;
        sqlx::query("UPDATE ng_devices SET notify = FALSE WHERE id = $1")
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        let before = daemon_snapshot(&pool, device).await;

        let response = call_form(
            &pool,
            &admin,
            "alerts_add_save",
            "POST",
            ALERTS_ADD,
            serde_json::json!({}),
            "kind=device&device=02%3A00%3A5e%3A00%3A09%3A11&back=%2Falerts",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(device_flags(&pool, device).await, (false, true));
        assert_eq!(
            before,
            daemon_snapshot(&pool, device).await,
            "a daemon-owned column moved"
        );

        let response = call_form(
            &pool,
            &admin,
            "alerts_add_save",
            "POST",
            ALERTS_ADD,
            serde_json::json!({}),
            "kind=device&device=02%3A00%3A5e%3A00%3A09%3A99&back=%2Falerts",
        )
        .await;
        assert_eq!(response["status"], 422, "{response}");
    });
}

/// **"Only the devices I choose"** mutes every listed device left unticked,
/// unmutes the ticked ones, and leaves a device the page did not list alone,
/// so one that appeared while the form was open is not muted for it.
#[test]
fn the_bulk_alerts_form_sets_exactly_the_listed_devices() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let keep = seed_clean_device(&pool, "02:00:5e:00:09:21", None, "online").await;
        let mute = seed_clean_device(&pool, "02:00:5e:00:09:22", None, "online").await;
        let unmute = seed_clean_device(&pool, "02:00:5e:00:09:23", None, "online").await;
        let unlisted = seed_clean_device(&pool, "02:00:5e:00:09:24", None, "online").await;
        sqlx::query("UPDATE ng_devices SET notify = FALSE WHERE id = $1")
            .bind(unmute)
            .execute(&pool)
            .await
            .unwrap();

        let form = call_form(
            &pool,
            &admin,
            "alerts_devices_form",
            "GET",
            ALERTS_DEVICES,
            serde_json::json!({}),
            "",
        )
        .await;
        assert_eq!(form["status"], 200, "{form}");
        let html = form["body"].as_str().unwrap();
        assert_eq!(html.matches("type=\"checkbox\"").count(), 4, "{html}");
        assert_eq!(html.matches(" checked>").count(), 3, "{html}");

        let listed = |mac: &str| format!("listed={}", mac.replace(':', "%3A"));
        let ticked = |mac: &str| format!("device={}", mac.replace(':', "%3A"));
        let body = [
            listed("02:00:5e:00:09:21"),
            ticked("02:00:5e:00:09:21"),
            listed("02:00:5e:00:09:22"),
            listed("02:00:5e:00:09:23"),
            ticked("02:00:5e:00:09:23"),
            "back=%2Falerts".to_string(),
        ]
        .join("&");
        let response = call_form(
            &pool,
            &admin,
            "alerts_devices_save",
            "POST",
            ALERTS_DEVICES,
            serde_json::json!({}),
            &body,
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(
            response["body"]
                .as_str()
                .unwrap()
                .contains("1 device will now alert; 1 device muted."),
            "{response}"
        );
        assert_eq!(device_flags(&pool, keep).await, (false, true));
        assert_eq!(device_flags(&pool, mute).await, (false, false));
        assert_eq!(device_flags(&pool, unmute).await, (false, true));
        assert_eq!(
            device_flags(&pool, unlisted).await,
            (false, true),
            "a device the page did not list was changed"
        );
    });
}

/// **Send test asks the daemon and does nothing else.** One row in
/// `ng_notify_requests`, naming who asked, with the daemon's columns empty; no
/// event, and nothing about where alerts go.
#[test]
fn send_test_inserts_one_request_for_the_daemon_and_nothing_else() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        seed_notify_status(&pool).await;

        let response = call_form(
            &pool,
            &admin,
            "alerts_save",
            "POST",
            ALERTS,
            serde_json::json!({}),
            "",
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        let html = response["body"].as_str().unwrap();
        assert!(
            html.contains("Asked netgraspd to send a test alert."),
            "{html}"
        );

        let rows: Vec<(Option<String>, Option<String>, bool)> = sqlx::query_as(
            "SELECT requested_by, outcome, handled_at IS NULL FROM ng_notify_requests",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows, [(Some(admin.id.to_string()), None, true)]);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM ng_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(events, 0);

        // The page shows the request waiting, and then what the daemon said.
        let page = call_form(
            &pool,
            &admin,
            "alerts_form",
            "GET",
            ALERTS,
            serde_json::json!({}),
            "",
        )
        .await;
        assert!(
            page["body"]
                .as_str()
                .unwrap()
                .contains("waiting for netgraspd"),
            "{page}"
        );
        sqlx::query("UPDATE ng_notify_requests SET handled_at = now(), outcome = 'sent'")
            .execute(&pool)
            .await
            .unwrap();
        let page = call_form(
            &pool,
            &admin,
            "alerts_form",
            "GET",
            ALERTS,
            serde_json::json!({}),
            "",
        )
        .await;
        assert!(
            page["body"]
                .as_str()
                .unwrap()
                .contains("Test alert sent today at"),
            "{page}"
        );
    });
}

/// **The page's switch turns one rule off and keeps the other**, through the
/// person notify save it posts to.
#[test]
fn the_alerts_page_switch_turns_one_person_rule_off_and_keeps_the_other() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        set_alerts(&pool, jamie, true, true).await;

        // Exactly the fields the page's "Turn off" beside the arrival rule posts.
        let response = call_form(
            &pool,
            &admin,
            "person_notify_save",
            "POST",
            "/netgrasp/person/notify",
            serde_json::json!({}),
            &format!("target={jamie}&back=%2Falerts&depart=1"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(
            person_alerts_both_tiers(&pool, jamie).await,
            ((false, true), (false, true))
        );
    });
}

/// **Nobody without the permission reaches any of it**: every Alerts page and
/// form answers 403 to a caller without `administer netgrasp` and to an
/// anonymous visitor, and nothing changes.
#[test]
fn the_alerts_pages_refuse_a_caller_without_the_permission_and_anonymous() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let jamie = seed_person_item(&pool, "Jamie").await;
        let device = seed_clean_device(&pool, "02:00:5e:00:09:31", None, "online").await;
        let nobody = ng_nobody(&pool).await;
        let calls = [
            ("alerts_form", "GET", ALERTS, String::new()),
            ("alerts_save", "POST", ALERTS, String::new()),
            ("alerts_add_form", "GET", ALERTS_ADD, String::new()),
            (
                "alerts_add_save",
                "POST",
                ALERTS_ADD,
                format!("kind=person&person={jamie}&when=arrive"),
            ),
            ("alerts_devices_form", "GET", ALERTS_DEVICES, String::new()),
            (
                "alerts_devices_save",
                "POST",
                ALERTS_DEVICES,
                "listed=02%3A00%3A5e%3A00%3A09%3A31".to_string(),
            ),
        ];
        for user in [nobody, UserContext::anonymous()] {
            for (callback, method, path, body) in &calls {
                let response = call_form(
                    &pool,
                    &user,
                    callback,
                    method,
                    path,
                    serde_json::json!({}),
                    body,
                )
                .await;
                assert_eq!(response["status"], 403, "{callback}: {response}");
            }
        }
        let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM ng_notify_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(requests, 0, "a refused caller asked for a test");
        assert_eq!(alerts_of(&pool, jamie).await, (false, false));
        assert_eq!(device_flags(&pool, device).await, (false, true));
    });
}

// ===========================================================================
// The event pages' SQL, as the kernel builds it, reads the daemon's indexes
// ===========================================================================

/// The daemon's indexes on `ng_events` the event pages rely on, in its own
/// words (netgraspd V1 and V2). The plugin's 001 copy of the table carries
/// none of them, and the plugin never creates one; the test does, as the
/// daemon would have.
const DAEMON_EVENT_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS ng_events_time_idx ON ng_events (\"timestamp\" DESC)",
    "CREATE INDEX IF NOT EXISTS ng_events_device_idx ON ng_events (device_id, \"timestamp\" DESC)",
    "CREATE INDEX IF NOT EXISTS ng_events_security_idx ON ng_events (event_type, \"timestamp\" DESC) \
     WHERE event_type IN ('arp_scan', 'arp_spoof', 'rogue_dhcp', 'identity_change', \
     'ip_conflict', 'gratuitous_arp')",
];

/// The two statements the kernel runs for page one of a record gather: the
/// rows and the pager's `COUNT(*)`.
///
/// Built by the kernel's own `GatherQueryBuilder` from the gather as the
/// migrations leave it, after the rewrite `GatherService` applies to a record
/// gather (`resolve_record_context`, which is private, so its three steps are
/// repeated here: the record type's relation becomes the base table, and each
/// filter and sort field is mapped through the record type's field map). A
/// `url_arg` filter gets its argument, as `resolve_contextual_values` gives it.
async fn kernel_gather_sql(
    pool: &PgPool,
    query_id: &str,
    url_args: &[(&str, &str)],
) -> (String, String) {
    kernel_gather_page_sql(pool, query_id, url_args, 1).await
}

/// [`kernel_gather_sql`] for any page: the rows of page `page` and the count.
async fn kernel_gather_page_sql(
    pool: &PgPool,
    query_id: &str,
    url_args: &[(&str, &str)],
    page: u32,
) -> (String, String) {
    use trovato_kernel::gather::types::ContextualValue;

    let gather = wire_gather(pool).await;
    let query = gather
        .get_query(query_id)
        .unwrap_or_else(|| panic!("{query_id} is not seeded"));
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

    let mut definition = query.definition.clone();
    let record_type = definition
        .record_type
        .clone()
        .unwrap_or_else(|| panic!("{query_id} is not a record gather"));
    let record = registry
        .get(&record_type)
        .unwrap_or_else(|| panic!("{record_type} is not admitted"));
    definition.base_table = record.table.clone();
    definition.item_type = None;
    definition.stage_aware = false;
    for filter in &mut definition.filters {
        if let Some(target) = record.resolve_field(&filter.field) {
            filter.field = target.to_string();
        }
        if let FilterValue::Contextual(ContextualValue::UrlArg(name)) = &filter.value {
            let value = url_args
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| (*v).to_string())
                .unwrap_or_else(|| panic!("{query_id} needs ?{name}="));
            filter.value = FilterValue::String(value);
        }
    }
    for sort in &mut definition.sorts {
        if let Some(target) = record.resolve_field(&sort.field) {
            sort.field = target.to_string();
        }
    }
    let per_page = query.display.items_per_page;
    let builder = trovato_kernel::gather::GatherQueryBuilder::new(
        definition,
        Uuid::parse_str(LIVE_STAGE).unwrap(),
    );
    (
        format!(
            "SELECT row_to_json(t) FROM ({}) t",
            builder.build(page, per_page)
        ),
        builder.build_count(),
    )
}

/// The plan of `sql` with sequential scans and explicit sorts priced out. The
/// test tables hold a handful of rows and have no statistics, so the planner
/// would otherwise scan or sort them whatever their indexes; pricing both out
/// leaves it an index and its order wherever one serves the statement, and a
/// statement that sorts on the unindexed epoch twin still plans that sort.
async fn plan_with_indexes_preferred(pool: &PgPool, sql: &str) -> String {
    let mut tx = pool.begin().await.unwrap();
    for set in [
        "SET LOCAL enable_seqscan = off",
        "SET LOCAL enable_sort = off",
    ] {
        sqlx::query(set).execute(&mut *tx).await.unwrap();
    }
    let lines: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {sql}")))
        .fetch_all(&mut *tx)
        .await
        .unwrap_or_else(|e| panic!("EXPLAIN {sql}: {e}"));
    tx.rollback().await.unwrap();
    lines.join("\n")
}

/// **The event pages read the daemon's indexes.** For /events, one device's
/// /events/device and /events/security, both statements a page runs, the
/// rows and the pager's total, exactly as the kernel builds them from the
/// gathers and record types the plugin ships, plan onto the daemon's time,
/// device and security indexes, and neither sorts nor compares the unindexed
/// `timestamp_epoch`. The two totals that could count the whole log stop at a
/// bound. Against the gathers as they were before 020 and 021 every one of
/// these statements sorted or counted all of `ng_events`.
///
/// The security page's total is the overview's `security_events`: the same
/// statement over the same view, so the same number.
#[test]
fn the_event_pages_as_the_kernel_runs_them_read_the_daemons_indexes() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        for ddl in DAEMON_EVENT_INDEXES {
            sqlx::query(*ddl).execute(&pool).await.unwrap();
        }
        let device = seed_device(&pool, "aa:bb:cc:00:24:01", None, "online").await;
        for (event_type, ago) in [
            ("ip_changed", 10),
            ("arp_scan", 20),
            ("arp_spoof", 30),
            ("name_updated", 40),
            ("identity_change", 90_000),
        ] {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
                 VALUES ($1, $2, to_timestamp($3))",
            )
            .bind(device)
            .bind(event_type)
            .bind((now() - ago) as f64)
            .execute(&pool)
            .await
            .unwrap();
        }
        let device_arg = device.to_string();

        for (query_id, args, index, rows_in_index_order, bounded_count) in [
            ("ng_event_log", vec![], "ng_events_time_idx", true, true),
            (
                "ng_event_by_device",
                vec![("device", device_arg.as_str())],
                "ng_events_device_idx",
                false,
                true,
            ),
            ("ng_event_security", vec![], "ng_events_", true, false),
        ] {
            let (rows_sql, count_sql) = kernel_gather_sql(&pool, query_id, &args).await;
            for (what, sql) in [("rows", &rows_sql), ("count", &count_sql)] {
                let plan = plan_with_indexes_preferred(&pool, sql).await;
                assert!(plan.contains(index), "{query_id} {what}: {sql}\n{plan}");
                assert!(
                    !plan.contains("Seq Scan on ng_events"),
                    "{query_id} {what}: {sql}\n{plan}"
                );
                assert!(
                    !plan.contains("timestamp_epoch"),
                    "{query_id} {what} sorts or compares the unindexed twin: {sql}\n{plan}"
                );
            }
            if rows_in_index_order {
                let plan = plan_with_indexes_preferred(&pool, &rows_sql).await;
                assert!(
                    !plan.contains("Sort"),
                    "{query_id}: the index gives the order: {rows_sql}\n{plan}"
                );
            }
            if bounded_count {
                let plan = plan_with_indexes_preferred(&pool, &count_sql).await;
                assert!(
                    plan.contains("Limit"),
                    "{query_id}'s total is not bounded: {count_sql}\n{plan}"
                );
            }
        }

        // One statement for the security total, on the page and the overview.
        let (_, security_count) = kernel_gather_sql(&pool, "ng_event_security", &[]).await;
        assert_eq!(
            security_count,
            "SELECT COUNT(*) FROM \"ng_security_events\""
        );
        let plan = plan_with_indexes_preferred(&pool, &security_count).await;
        assert!(plan.contains("ng_events_security_idx"), "{plan}");
        let overview: String = sqlx::query_scalar("SELECT pg_get_viewdef('ng_overview'::regclass)")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            overview.contains("FROM ng_security_events) AS security_events"),
            "{overview}"
        );

        let gather = wire_gather(&pool).await;
        let context = QueryContext {
            viewer: Some(network_viewer()),
            ..QueryContext::default()
        };
        let page = gather
            .execute(
                "ng_event_security",
                1,
                HashMap::new(),
                Uuid::parse_str(LIVE_STAGE).unwrap(),
                &context,
            )
            .await
            .unwrap();
        let on_overview: i64 = sqlx::query_scalar("SELECT security_events FROM ng_home")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.total, on_overview as u64);

        for ddl in [
            "DROP INDEX IF EXISTS ng_events_time_idx",
            "DROP INDEX IF EXISTS ng_events_device_idx",
            "DROP INDEX IF EXISTS ng_events_security_idx",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
    });
}

/// The kernel's `compute_page_list` (`crates/kernel/src/routes/gather.rs`),
/// which is private: page 1, the last page and two either side of the current
/// one, with "..." for each gap.
fn kernel_page_list(current: u32, total: u32) -> Vec<serde_json::Value> {
    let mut shown = std::collections::BTreeSet::from([1, total]);
    for p in current.saturating_sub(2).max(1)..=(current + 2).min(total) {
        shown.insert(p);
    }
    let mut pages = Vec::new();
    let mut last = 0;
    for p in shown {
        if last > 0 && p > last + 1 {
            pages.push(serde_json::json!("..."));
        }
        pages.push(serde_json::json!(p));
        last = p;
    }
    pages
}

/// The kernel's `templates/gather/pager.html` at the pinned revision, which
/// netgrasp's `page.html` includes and the plugin's template directory does not
/// carry. Verbatim, so a page that keeps the kernel's pager renders it here as
/// it would there.
const KERNEL_PAGER: &str = r#"<nav class="pager" aria-label="Pagination">
    <ul class="pager__items">
        {% if pager.current_page > 1 %}
        <li class="pager__item pager__item--previous">
            <a href="{{ pager.page_url_prefix }}page={{ pager.current_page - 1 }}">Previous</a>
        </li>
        {% endif %}

        {% for page in pager.pages %}
        <li class="pager__item{% if page == pager.current_page %} pager__item--active{% endif %}">
            {% if page == pager.current_page %}
            <span aria-current="page">{{ page }}</span>
            {% elif page == "..." %}
            <span class="pager__ellipsis">...</span>
            {% else %}
            <a href="{{ pager.page_url_prefix }}page={{ page }}">{{ page }}</a>
            {% endif %}
        </li>
        {% endfor %}

        {% if pager.current_page < pager.total_pages %}
        <li class="pager__item pager__item--next">
            <a href="{{ pager.page_url_prefix }}page={{ pager.current_page + 1 }}">Next</a>
        </li>
        {% endif %}
    </ul>
</nav>
{# Pager styles live in static/css/theme.css #}
"#;

/// [`render_page`] for page `page` of a result of `total` rows, with the pager
/// context `render_gather_with_theme` adds when there is more than one page.
fn render_paged(
    query_id: &str,
    base_path: &str,
    rows: &[serde_json::Value],
    total: u64,
    page: u32,
    per_page: u32,
) -> String {
    let dir = plugin_source_dir().join("../../templates/**/*.html");
    let mut tera = tera::Tera::new(&dir.to_string_lossy()).expect("the templates parse");
    tera.add_raw_template("gather/pager.html", KERNEL_PAGER)
        .expect("the kernel's pager parses");
    let total_pages = total.div_ceil(u64::from(per_page)) as u32;
    let mut context = tera::Context::new();
    context.insert(
        "query",
        &serde_json::json!({"query_id": query_id, "label": query_id}),
    );
    context.insert("rows", rows);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("per_page", &per_page);
    context.insert("total_pages", &total_pages);
    context.insert("has_next", &(page < total_pages));
    context.insert("has_prev", &(page > 1));
    context.insert("base_path", base_path);
    context.insert("exposed_filters", &serde_json::json!([]));
    context.insert("filter_values", &serde_json::json!({}));
    if total_pages > 1 {
        context.insert(
            "pager",
            &serde_json::json!({
                "current_page": page,
                "total_pages": total_pages,
                "page_url_prefix": format!("{base_path}?"),
                "pages": kernel_page_list(page, total_pages),
            }),
        );
    }
    tera.render(&format!("gather/query--{query_id}.html"), &context)
        .unwrap_or_else(|e| panic!("{query_id} failed to render: {e:#?}"))
}

/// Every page number a rendered page links to, in order.
fn pager_links(html: &str) -> Vec<u32> {
    html.split("?page=")
        .skip(1)
        .filter_map(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        })
        .collect()
}

/// How many rows the input of the statement's `Limit` produced: the rows a
/// page read to return its 50, which for `OFFSET n` is at least `n + 50`.
async fn rows_under_the_limit(pool: &PgPool, sql: &str) -> u64 {
    fn find(node: &serde_json::Value) -> Option<u64> {
        if node["Node Type"] == "Limit" {
            let child = &node["Plans"][0];
            let loops = child["Actual Loops"].as_u64().unwrap_or(1).max(1);
            return child["Actual Rows"].as_u64().map(|r| r * loops);
        }
        node["Plans"].as_array()?.iter().find_map(find)
    }
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE, FORMAT JSON) {sql}"
    )))
    .fetch_one(pool)
    .await
    .unwrap();
    find(&plan[0]["Plan"]).unwrap_or_else(|| panic!("no Limit in {plan}"))
}

/// **/events/security offers no page deeper than the newest 10000.** Its
/// total stays the exact count, the overview's, but the pager stops at the
/// page that holds the 10000th newest event, so the deepest page it links to
/// reads at most 10000 rows to show its 50. Before, the pager ran to the last
/// of `total`, and that page read every security event on record and sorted
/// them: about 190 ms on the live network, and more each day.
///
/// The plan is asked with ANALYZE, of the statement the kernel builds for the
/// deepest page the rendered pager links to, so it fails on the old template
/// whichever plan the planner picks for a table this size.
#[test]
fn the_security_pages_deepest_page_reads_at_most_the_newest_10000() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        for ddl in DAEMON_EVENT_INDEXES {
            sqlx::query(*ddl).execute(&pool).await.unwrap();
        }
        // No statistics: the assertion below is on the rows a page reads, which
        // no plan changes, and statistics for 72,000 rows would outlive the
        // TRUNCATE at the end and change the plans the other event tests
        // assert. So the table is not analyzed here, and autovacuum is kept
        // from analyzing it in the background until it is empty again.
        sqlx::query("ALTER TABLE ng_events SET (autovacuum_enabled = false)")
            .execute(&pool)
            .await
            .unwrap();
        let device = seed_device(&pool, "aa:bb:cc:00:27:01", None, "online").await;
        // 12,000 security events among 60,000 others, a second apart.
        sqlx::query(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
             SELECT $1, CASE WHEN g % 6 = 0 THEN 'arp_scan' ELSE 'ip_changed' END, \
                    now() - make_interval(secs => g) \
             FROM generate_series(1, 72000) g",
        )
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();

        let gather = wire_gather(&pool).await;
        let context = QueryContext {
            viewer: Some(network_viewer()),
            ..QueryContext::default()
        };
        let first = gather
            .execute(
                "ng_event_security",
                1,
                HashMap::new(),
                Uuid::parse_str(LIVE_STAGE).unwrap(),
                &context,
            )
            .await
            .unwrap();
        let on_overview: i64 = sqlx::query_scalar("SELECT security_events FROM ng_home")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(first.total, 12_000, "the total is every security event");
        assert_eq!(first.total, on_overview as u64);

        let html = render_paged(
            "ng_event_security",
            "/events/security",
            &first.items,
            first.total,
            1,
            first.per_page,
        );
        assert!(
            html.contains(&format!(
                "showing {} of the newest 10000, page 1 of 200",
                first.items.len()
            )),
            "{html}"
        );
        assert!(html.contains(">12000</span>"), "the total is exact: {html}");
        let deepest = pager_links(&html).into_iter().max().unwrap();
        assert_eq!(deepest, 10_000 / first.per_page, "{html}");

        let (rows_sql, _) = kernel_gather_page_sql(&pool, "ng_event_security", &[], deepest).await;
        let read = rows_under_the_limit(&pool, &rows_sql).await;
        assert!(
            read <= 10_000,
            "the deepest page offered, {deepest}, read {read} rows: {rows_sql}"
        );

        sqlx::raw_sql(
            "TRUNCATE ng_events CASCADE; ALTER TABLE ng_events RESET (autovacuum_enabled)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for ddl in [
            "DROP INDEX IF EXISTS ng_events_time_idx",
            "DROP INDEX IF EXISTS ng_events_device_idx",
            "DROP INDEX IF EXISTS ng_events_security_idx",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }

        // The pager's window and its last page, from the middle and the end.
        let middle = render_paged(
            "ng_event_security",
            "/events/security",
            &first.items,
            first.total,
            100,
            first.per_page,
        );
        assert_eq!(pager_links(&middle), [99, 1, 98, 99, 101, 102, 200, 101]);
        assert_eq!(middle.matches("pager__ellipsis").count(), 2, "{middle}");
        let last = render_paged(
            "ng_event_security",
            "/events/security",
            &first.items,
            first.total,
            200,
            first.per_page,
        );
        assert_eq!(pager_links(&last), [199, 1, 198, 199]);
        assert!(!last.contains("pager__item--next"), "{last}");
        assert!(last.contains("page 200 of 200"), "{last}");

        // Under the bound the page reads as it always did: no bound to name,
        // and the kernel's own last page.
        let few = render_paged(
            "ng_event_security",
            "/events/security",
            &first.items,
            120,
            1,
            first.per_page,
        );
        assert!(!few.contains("newest"), "{few}");
        assert!(few.contains("page 1 of 3"), "{few}");
        assert_eq!(pager_links(&few), [2, 3, 2]);
    });
}

// ===========================================================================
// U18: presence is opt in
// ===========================================================================

const FORM_DEVICE_PRESENCE: &str = "/netgrasp/device/presence";
const FORM_PERSON_PRESENCE: &str = "/netgrasp/person/presence";
const FORM_PERSON_RENAME: &str = "/netgrasp/person/rename";

/// An Item field's value, whichever of the two shapes it is stored in.
async fn item_field(pool: &PgPool, item_id: Uuid, field: &str) -> serde_json::Value {
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT fields -> $2 FROM item WHERE id = $1")
            .bind(item_id)
            .bind(field)
            .fetch_one(pool)
            .await
            .unwrap();
    let value = value.unwrap_or(serde_json::Value::Null);
    value.get("value").cloned().unwrap_or(value)
}

/// The daemon's status row, standing in for a netgraspd at V7 that has
/// published its `presence_tracking` setting.
async fn publish_presence_tracking(pool: &PgPool, on: bool) {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS ng_daemon_status; \
         CREATE TABLE ng_daemon_status (id BIGINT PRIMARY KEY DEFAULT 1, version TEXT NOT NULL, \
           timezone TEXT NOT NULL, timezone_source TEXT NOT NULL, started_at TIMESTAMPTZ NOT NULL, \
           updated_at TIMESTAMPTZ NOT NULL, presence_tracking BOOLEAN NOT NULL DEFAULT FALSE)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ng_daemon_status (version, timezone, timezone_source, started_at, updated_at, \
         presence_tracking) VALUES ('test', 'UTC', 'config', now(), now(), $1)",
    )
    .bind(on)
    .execute(pool)
    .await
    .unwrap();
}

/// **The two presence forms write both tiers.** The device form offers three
/// choices and says the effective value and why; each choice lands in the
/// Item's field and in `ng_devices.counts_for_presence` and reads back as
/// itself. The person form's one switch lands in `field_track_presence` and
/// `ng_people.track_presence`, and survives a rename, which rebuilds the
/// person's fields.
#[test]
fn the_presence_forms_write_both_tiers_and_round_trip_the_three_choices() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let admin = ng_admin(&pool).await;
        // A tablet: by type it does not count.
        let device = seed_clean_device(&pool, "02:00:5e:00:18:01", None, "online").await;
        let target = "02%3A00%3A5e%3A00%3A18%3A01";

        let form = call_form(
            &pool,
            &admin,
            "device_presence_form",
            "GET",
            FORM_DEVICE_PRESENCE,
            serde_json::json!({"target": "02:00:5e:00:18:01"}),
            "",
        )
        .await;
        assert_eq!(form["status"], 200, "{form}");
        let body = form["body"].as_str().unwrap_or_default();
        for expected in [
            r#"<option value="by_type" selected>By device type</option>"#,
            r#"<option value="counts">Counts</option>"#,
            r#"<option value="does_not_count">Does not count</option>"#,
            "Now: <strong>Does not count: tablet</strong>",
            "minted-by-the-kernel",
        ] {
            assert!(body.contains(expected), "missing {expected:?}: {body}");
        }

        for (value, column) in [
            ("counts", Some(true)),
            ("does_not_count", Some(false)),
            ("by_type", None),
        ] {
            let response = call_form(
                &pool,
                &admin,
                "device_presence_save",
                "POST",
                FORM_DEVICE_PRESENCE,
                serde_json::json!({}),
                &format!("target={target}&back=%2Fdevices&value={value}"),
            )
            .await;
            assert_eq!(response["status"], 200, "{response}");
            let (counts, item): (Option<bool>, Option<Uuid>) = sqlx::query_as(
                "SELECT counts_for_presence, trovato_item_id FROM ng_devices WHERE id = $1",
            )
            .bind(device)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(counts, column, "{value}: the column");
            let item = item.expect("the edit minted the device's Item");
            assert_eq!(
                item_field(&pool, item, "field_counts_for_presence").await,
                serde_json::json!(value),
                "{value}: the Item field"
            );
            if value == "counts" {
                let again = call_form(
                    &pool,
                    &admin,
                    "device_presence_form",
                    "GET",
                    FORM_DEVICE_PRESENCE,
                    serde_json::json!({"target": "02:00:5e:00:18:01"}),
                    "",
                )
                .await;
                let body = again["body"].as_str().unwrap_or_default();
                assert!(
                    body.contains(r#"<option value="counts" selected>"#)
                        && body.contains("Counts: chosen for this device"),
                    "{body}"
                );
            }
        }
        // The edit wrote its one column: alerts untouched.
        assert_eq!(device_flags(&pool, device).await, (false, true));

        // The person: off by default, on, kept through a rename, off again.
        let person = seed_person_item(&pool, "Resident").await;
        let track = |pool: PgPool| async move {
            sqlx::query_scalar::<_, bool>("SELECT track_presence FROM ng_people WHERE item_id = $1")
                .bind(person)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert!(!track(pool.clone()).await, "a new person is not tracked");
        let form = call_form(
            &pool,
            &admin,
            "person_presence_form",
            "GET",
            FORM_PERSON_PRESENCE,
            serde_json::json!({"target": person.to_string()}),
            "",
        )
        .await;
        assert_eq!(form["status"], 200, "{form}");
        let body = form["body"].as_str().unwrap_or_default();
        assert!(
            body.contains(r#"name="track" value="1"> Track whether they are home"#),
            "{body}"
        );

        let response = call_form(
            &pool,
            &admin,
            "person_presence_save",
            "POST",
            FORM_PERSON_PRESENCE,
            serde_json::json!({}),
            &format!("target={person}&back=%2Fpeople&track=1"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(track(pool.clone()).await, "the mirror did not follow");
        assert_eq!(
            item_field(&pool, person, "field_track_presence").await,
            serde_json::json!(true)
        );

        let response = call_form(
            &pool,
            &admin,
            "person_rename_save",
            "POST",
            FORM_PERSON_RENAME,
            serde_json::json!({}),
            &format!("target={person}&back=%2Fpeople&value=Resident+Two"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(track(pool.clone()).await, "a rename turned tracking off");
        assert_eq!(
            item_field(&pool, person, "field_track_presence").await,
            serde_json::json!(true),
            "a rename dropped the field"
        );

        let response = call_form(
            &pool,
            &admin,
            "person_presence_save",
            "POST",
            FORM_PERSON_PRESENCE,
            serde_json::json!({}),
            &format!("target={person}&back=%2Fpeople"),
        )
        .await;
        assert_eq!(response["status"], 200, "{response}");
        assert!(!track(pool.clone()).await);
        assert_eq!(
            item_field(&pool, person, "field_track_presence").await,
            serde_json::json!(false)
        );
    });
}

/// **Nobody untracked is home or away, anywhere.** Home counts them as
/// neither and lists them on a line of their own; /people/home lists them last
/// as "Not tracked"; /people's cards say "Not tracked"; Who is home leaves out
/// their online device. With the install's switch off, Home, /people/home and
/// /people say so in one line naming the config key; with it on, none does.
#[test]
fn untracked_people_are_not_tracked_everywhere_and_the_switch_off_is_said() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        seed_house(&pool).await;
        let untracked = seed_person_item(&pool, "Resident").await;
        sqlx::query("UPDATE ng_people SET state = 'untracked' WHERE item_id = $1")
            .bind(untracked)
            .execute(&pool)
            .await
            .unwrap();
        // Their laptop is online, which made somebody home before U18.
        seed_clean_device(&pool, "02:00:5e:00:18:02", Some(untracked), "online").await;
        publish_presence_tracking(&pool, false).await;
        let gather = wire_gather(&pool).await;
        let notice = "Set <code>presence_tracking = true</code> in netgraspd's config";

        // Home.
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        let ov = &rows[0];
        assert_eq!(
            (
                &ov["people_home"],
                &ov["people_away"],
                &ov["people_untracked"]
            ),
            (
                &serde_json::json!(2),
                &serde_json::json!(1),
                &serde_json::json!(1)
            ),
            "{ov}"
        );
        assert_eq!(ov["presence_tracking"], false, "{ov}");
        let away: Vec<&str> = ov["away"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        let not: Vec<&str> = ov["untracked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!((away, not), (vec!["Arlo"], vec!["Resident"]));
        let html = render_page("ng_overview", "Overview", "/overview", &rows);
        assert!(html.contains(notice), "{html}");
        assert!(html.contains("Not tracked:"), "{html}");

        // /people/home: everybody, the untracked last and never away.
        let home = gather_items(&gather, "ng_people_home", HashMap::new()).await;
        let last = home.last().unwrap();
        assert_eq!(
            (last["name"].as_str(), last["state"].as_str()),
            (Some("Resident"), Some("untracked"))
        );
        let html = render_page("ng_people_home", "Home now", "/people/home", &home);
        assert!(html.contains(notice), "{html}");
        assert!(html.contains("Not tracked"), "{html}");

        // /people: the card carries the daemon's state through 023's include.
        let people_viewer = || {
            UserContext::authenticated(
                Uuid::now_v7(),
                vec!["view netgrasp devices".into(), "access content".into()],
            )
        };
        let people =
            gather_items_as(&gather, "ng_person_list", HashMap::new(), people_viewer()).await;
        let card = people
            .iter()
            .find(|r| r["title"] == "Resident")
            .unwrap_or_else(|| panic!("the person's card: {people:?}"));
        assert_eq!(card["presence"]["state"], "untracked", "{card}");
        let html = render_page("ng_person_list", "People", "/people", &people);
        assert!(html.contains(notice), "{html}");
        assert!(
            html.contains("ng-person__state--untracked\">Not tracked"),
            "{html}"
        );

        // Who is home: the untracked owner's online laptop does not count.
        let who = gather_items(&gather, "ng_who_is_home", HashMap::new()).await;
        assert!(
            who.iter().all(|r| r["mac"] != "02:00:5e:00:18:02"),
            "an untracked person's device made them home: {who:?}"
        );
        assert!(!who.is_empty(), "the tracked owner's phone is still listed");

        // The switch on: no notice anywhere.
        publish_presence_tracking(&pool, true).await;
        let rows = gather_items(&gather, "ng_overview", HashMap::new()).await;
        assert!(!render_page("ng_overview", "Overview", "/overview", &rows).contains(notice));
        let home = gather_items(&gather, "ng_people_home", HashMap::new()).await;
        assert!(!render_page("ng_people_home", "Home now", "/people/home", &home).contains(notice));
        let people =
            gather_items_as(&gather, "ng_person_list", HashMap::new(), people_viewer()).await;
        assert!(!people.is_empty());
        assert!(!render_page("ng_person_list", "People", "/people", &people).contains(notice));
        reset(&pool).await;
    });
}

// ===========================================================================
// Activity and the new device list read the daemon's V8 type index
// ===========================================================================

/// The daemon's schema fixture, which carries netgraspd V8's indexes on
/// `ng_events` in the daemon's words.
const DAEMON_SCHEMA_FIXTURE: &str =
    include_str!("../../../crates/netgrasp-core/tests/fixtures/daemon_schema.sql");

/// The `CREATE INDEX` statement for `name`, taken from the daemon's schema
/// fixture, so that the test below fails on a fixture without it.
fn fixture_index(name: &str) -> String {
    let start = DAEMON_SCHEMA_FIXTURE
        .find(&format!("CREATE INDEX {name}"))
        .unwrap_or_else(|| panic!("the daemon schema fixture has no {name}"));
    let statement = &DAEMON_SCHEMA_FIXTURE[start..];
    let end = statement.find(';').expect("a statement ends");
    statement[..end].to_string()
}

/// **Activity and /events/new-devices read `ng_events_type_time_idx`.** Both
/// statements each page runs, the rows and the pager's total, exactly as the
/// kernel builds them from the gathers the plugin ships, plan onto netgraspd
/// V8's index on every event but `ip_changed` by type and time, and read no
/// sequential scan of `ng_events`. Before V8 no index covered `new_device` or
/// the person events, and each statement read the whole log (110 to 200 ms
/// live, over 4.1 million events).
///
/// Without the index (a daemon at V7) the same statements still run, plan
/// without it, and the pages list the same events with the same totals: slower
/// is the only difference.
#[test]
fn activity_and_new_devices_as_the_kernel_runs_them_read_the_daemons_type_index() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let device = seed_device(&pool, "aa:bb:cc:00:26:01", None, "online").await;
        for (event_type, ago) in [
            ("ip_changed", 10),
            ("new_device", 20),
            ("arp_spoof", 30),
            ("returned", 40),
            ("ip_changed", 50),
            ("new_device", 60),
            ("arp_scan", 70),
        ] {
            sqlx::query(
                "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
                 VALUES ($1, $2, to_timestamp($3))",
            )
            .bind(device)
            .bind(event_type)
            .bind((now() - ago) as f64)
            .execute(&pool)
            .await
            .unwrap();
        }
        let index = fixture_index("ng_events_type_time_idx");
        let gather = wire_gather(&pool).await;
        let context = QueryContext {
            viewer: Some(network_viewer()),
            ..QueryContext::default()
        };

        let mut pages = HashMap::new();
        for with_index in [true, false] {
            if with_index {
                sqlx::query(sqlx::AssertSqlSafe(index.as_str()))
                    .execute(&pool)
                    .await
                    .unwrap();
            } else {
                sqlx::query("DROP INDEX ng_events_type_time_idx")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            for query_id in ["ng_activity", "ng_event_new_devices"] {
                let (rows_sql, count_sql) = kernel_gather_sql(&pool, query_id, &[]).await;
                for (what, sql) in [("rows", &rows_sql), ("count", &count_sql)] {
                    let plan = plan_with_indexes_preferred(&pool, sql).await;
                    assert_eq!(
                        plan.contains("ng_events_type_time_idx"),
                        with_index,
                        "{query_id} {what}, index {with_index}: {sql}\n{plan}"
                    );
                    if with_index {
                        assert!(
                            !plan.contains("Seq Scan on ng_events"),
                            "{query_id} {what}: {sql}\n{plan}"
                        );
                    }
                }
                let page = gather
                    .execute(
                        query_id,
                        1,
                        HashMap::new(),
                        Uuid::parse_str(LIVE_STAGE).unwrap(),
                        &context,
                    )
                    .await
                    .unwrap();
                let listed: Vec<(String, i64)> = page
                    .items
                    .iter()
                    .map(|item| {
                        (
                            item["event_type"].as_str().unwrap_or_default().to_string(),
                            item["id"].as_i64().unwrap_or_default(),
                        )
                    })
                    .collect();
                let answer = (page.total, listed);
                match pages.get(query_id) {
                    None => {
                        pages.insert(query_id, answer);
                    }
                    Some(with) => assert_eq!(
                        with, &answer,
                        "{query_id} lists something else without the index"
                    ),
                }
            }
        }
        let types = |query_id: &str| -> Vec<String> {
            pages[query_id].1.iter().map(|(t, _)| t.clone()).collect()
        };
        assert_eq!(pages["ng_activity"].0, 3);
        assert_eq!(
            types("ng_activity"),
            ["new_device", "arp_spoof", "new_device"]
        );
        assert_eq!(pages["ng_event_new_devices"].0, 2);
        assert_eq!(types("ng_event_new_devices"), ["new_device", "new_device"]);
    });
}
