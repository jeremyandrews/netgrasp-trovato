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
        sqlx::query(&format!(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\", details) \
             VALUES ($1, $2, {at}, $3::jsonb || jsonb_build_object('person', $4::text, 'person_item_id', $5::text, 'device', 'aa:bb:cc:00:01:01'))"
        ))
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
        sqlx::query(&format!(
            "INSERT INTO ng_events (device_id, event_type, \"timestamp\") \
             VALUES ($1, $2, now() - interval '{ago}')"
        ))
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
        assert_eq!(checked, 4, "not every overview record type was checked");
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
        let columns: HashSet<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns WHERE table_name = 'ng_overview'",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
        let names: Vec<&String> = includes.as_object().unwrap().keys().collect();
        assert_eq!(names.len(), 3, "{includes}");
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

        let html = render_page("ng_overview", "Overview", "/overview", &rows);
        for expected in [
            "Aurora",
            "Home since",
            "via Driveway",
            "via Gate",
            "Left",
            "92%",
            "Not yet identified",
            "aa:bb:cc:00:01:05",
            "ng-menu__button",
            "href=\"/events/security\"",
            "ng-stat--alert",
        ] {
            assert!(
                html.contains(expected),
                "the overview lost {expected:?}: {html}"
            );
        }
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

        let home = gather_items(&gather, "ng_people_home", HashMap::new()).await;
        assert_eq!(home.len(), 2, "only the people who are home: {home:?}");
        assert!(render_page("ng_people_home", "Home now", "/people/home", &home).contains("Jamie"));

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
        for list in ["home", "movements", "new_devices"] {
            assert_eq!(rows[0][list], serde_json::json!([]), "{list}: {}", rows[0]);
        }
        let html = render_page("ng_overview", "Overview", "/overview", &rows);
        assert!(html.contains("Nobody is home."), "{html}");
        assert!(!html.contains("ng-stat--alert"), "{html}");
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
            sqlx::raw_sql(&migration).execute(&pool).await.unwrap();
            assert_eq!(
                front(pool.clone()).await,
                Some(serde_json::json!(after)),
                "front page {before:?} became the wrong thing"
            );
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
            PERM_ADMINISTER.to_string(),
            "edit ng_device content".to_string(),
            "edit ng_person content".to_string(),
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
    let state = RequestState::new(
        user.clone(),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    );
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
            &UserContext::authenticated(author, vec!["create ng_person content".into()]),
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
        assert!(snapshot.contains("Jamie's devices:"), "{snapshot}");
        assert!(
            snapshot.contains("Devices by state (1 total):"),
            "{snapshot}"
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
        assert_eq!(refused["ok"], false, "{refused}");
        let message = refused["content"].as_str().unwrap_or_default();
        assert!(message.contains("matches 2 people"), "{message}");
        assert!(message.contains(&first.to_string()), "{message}");
        assert!(message.contains(&second.to_string()), "{message}");

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
