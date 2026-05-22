//! In-browser SQLite connection backed by `sqlite-wasm-rs` + Diesel.
//!
//! Thread-local because Dioxus's wasm runtime is single-threaded;
//! `RefCell` is sufficient. Mirrors
//! `geolite/examples/web-demo/worker/src/lib.rs`, including the SQLiteGIS +
//! sqlite-vec auto-extension registration so schemas that translate to `ST_*`
//! calls or `vec0` virtual tables actually execute against the in-memory
//! connection.

use std::{cell::RefCell, sync::Once};

use diesel::{
    connection::SimpleConnection, prelude::*, sql_types::BigInt, sqlite::SqliteConnection,
};

/// Stand-in value returned by every session-variable UDF the playground
/// registers (e.g. `current_app_user()` after pg2sqlite rewrites
/// `current_setting('app.user_id')`). Picked to match the seed row
/// `owner_id = 42` in the RLS sample so policies produce visible filtered
/// output the moment the user clicks the sample. Returned as INTEGER so
/// the comparison `NEW.owner_id = current_app_user()` inside the INSTEAD
/// OF INSERT trigger that pg2sqlite emits compares integer-to-integer.
/// (The translator drops the original `::integer` cast on the policy
/// predicate, and SQLite does not implicitly coerce TEXT to INTEGER for
/// `NEW.column` references inside a trigger - so a TEXT-returning UDF
/// would silently fail the WITH CHECK and abort every seeded INSERT.)
const SESSION_VAR_VALUE: i64 = 42;

thread_local! {
    static CONN: RefCell<Option<SqliteConnection>> = const { RefCell::new(None) };
}

static INIT_AUTO_EXTENSIONS: Once = Once::new();

/// Auto-extension callback registering every SQLiteGIS scalar /
/// virtual-table-helper function on a freshly-opened connection.
unsafe extern "C" fn sqlitegis_init(
    db: *mut sqlite_wasm_rs::sqlite3,
    _pz_err_msg: *mut *mut std::ffi::c_char,
    _p_api: *const sqlite_wasm_rs::sqlite3_api_routines,
) -> std::ffi::c_int {
    unsafe { sqlitegis::sqlite::register_functions(db) }
}

fn ensure_auto_extensions() {
    INIT_AUTO_EXTENSIONS.call_once(|| unsafe {
        sqlite_wasm_rs::sqlite3_auto_extension(Some(sqlitegis_init));
        sqlite_wasm_rs::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut sqlite_wasm_rs::sqlite3,
                *mut *mut std::ffi::c_char,
                *const sqlite_wasm_rs::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(
            sqlite_wasm_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Open a fresh in-memory database, replacing any previously open
/// connection. Called on every successful translation so the apply
/// step starts from a clean slate (no leftover tables from the
/// previous schema).
///
/// `session_var_funcs` carries the names of the SQLite UDFs that
/// pg2sqlite emits in place of `current_setting(...)` / `current_user`
/// references. Each name gets a no-arg stub returning the constant
/// `SESSION_VAR_VALUE` so RLS policies and other predicates that
/// reference them can actually evaluate at apply time.
pub fn reopen(session_var_funcs: &[String]) -> Result<(), String> {
    ensure_auto_extensions();
    let conn = SqliteConnection::establish(":memory:").map_err(|e| e.to_string())?;
    for name in session_var_funcs {
        conn.register_noarg_sql_function::<BigInt, _, _>(name, true, || SESSION_VAR_VALUE)
            .map_err(|e| format!("could not register session-variable UDF '{name}': {e}"))?;
    }
    CONN.with(|cell| *cell.borrow_mut() = Some(conn));
    Ok(())
}

/// Borrow the live connection for one operation.
///
/// Panics if `reopen` hasn't been called first. The wizard guarantees
/// `reopen` runs before any code path that ends up here.
pub fn with_conn<R>(f: impl FnOnce(&mut SqliteConnection) -> R) -> R {
    CONN.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let conn = borrow.as_mut().expect("db::reopen must be called before with_conn");
        f(conn)
    })
}

/// Execute a free-form SQL script (multiple statements, separated by `;`).
/// Used by Step 1's "apply translated schema" step.
pub fn run_script(sql: &str) -> Result<(), String> {
    with_conn(|c| c.batch_execute(sql).map_err(|e| e.to_string()))
}
