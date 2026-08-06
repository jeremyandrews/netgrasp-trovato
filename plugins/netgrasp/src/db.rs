//! The `db` host, wrapped so the rest of the plugin talks in
//! [`netgrasp_core::CoreError`] rather than in negative host error codes.
//!
//! Every statement in this plugin is parameterized. The `db` host's `raw_sql`
//! capability permits `format!()`-built SQL and the plugin never uses it: the one
//! place a column name is chosen dynamically — the write-back's `SET` list — picks
//! from a compile-time constant list (`netgrasp_core::columns::USER_OWNED`) and
//! binds every value.

use netgrasp_core::{CoreError, CoreResult};
use serde::Deserialize;
use serde_json::Value;
use trovato_sdk::host;

/// Translate a `db` host error code into a transient store error.
///
/// Transient by default: a db failure inside `tap_cron` means the row stays
/// `dirty` and the next tick re-attempts it, which is the behaviour that makes
/// the sync self-healing.
fn map_db_err(code: i32) -> CoreError {
    CoreError::Store(format!("db host error {code}"))
}

/// Run a query and decode its rows.
///
/// The `db` host returns an array of row objects as JSON. Values are decoded by
/// `serde`, so a caller's row struct is the schema assertion.
///
/// # Errors
///
/// [`CoreError::Store`] when the host call fails or the response does not decode
/// into `T`.
pub fn query_rows<T: for<'de> Deserialize<'de>>(sql: &str, params: &[Value]) -> CoreResult<Vec<T>> {
    let json = host::query_raw(sql, params).map_err(map_db_err)?;
    serde_json::from_str(&json).map_err(|e| CoreError::Store(format!("row decode: {e}")))
}

/// Run a DML statement, returning rows affected.
///
/// # Errors
///
/// [`CoreError::Store`] when the host call fails.
pub fn exec(sql: &str, params: &[Value]) -> CoreResult<u64> {
    host::execute_raw(sql, params).map_err(map_db_err)
}

/// The database's clock, in unix seconds.
///
/// Read from Postgres rather than from the guest: a wasm guest has no clock, and
/// the daemon's timestamps are written against the database's, so comparing
/// against anything else would produce an off-by-a-timezone at best.
///
/// # Errors
///
/// [`CoreError::Store`] when the query fails.
pub fn now() -> CoreResult<i64> {
    #[derive(Deserialize)]
    struct TsRow {
        ts: i64,
    }
    let rows: Vec<TsRow> = query_rows(netgrasp_core::queries::SELECT_CLOCK, &[])?;
    rows.into_iter()
        .next()
        .map(|r| r.ts)
        .ok_or_else(|| CoreError::Store("clock query returned no row".into()))
}
