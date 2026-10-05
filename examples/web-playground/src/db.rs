//! In-browser SQLite connection backed by `sqlite-wasm-rs` + Diesel.
//!
//! Thread-local because Dioxus's wasm runtime is single-threaded, so `RefCell`
//! suffices.

use std::{cell::RefCell, sync::Once};

use diesel::{
    connection::SimpleConnection, prelude::*, sql_types::BigInt, sqlite::SqliteConnection,
};

/// Stand-in for every session-variable UDF (e.g. `current_app_user()`). Matches
/// seed row `owner_id = 42` so RLS policies produce visible output. Must be
/// INTEGER: SQLite does not coerce TEXT to INTEGER in trigger `NEW.column`
/// comparisons.
const SESSION_VAR_VALUE: i64 = 42;

thread_local! {
    static CONN: RefCell<Option<SqliteConnection>> = const { RefCell::new(None) };
}

static INIT_AUTO_EXTENSIONS: Once = Once::new();

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

/// Open a fresh in-memory connection. `session_var_funcs` lists the UDF names
/// pg2sqlite emits for `current_setting`/`current_user` references. Each gets a
/// no-arg stub returning `SESSION_VAR_VALUE`.
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

pub fn with_conn<R>(f: impl FnOnce(&mut SqliteConnection) -> R) -> R {
    CONN.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let conn = borrow.as_mut().expect("db::reopen must be called before with_conn");
        f(conn)
    })
}

pub fn run_script(sql: &str) -> Result<(), String> {
    with_conn(|c| c.batch_execute(sql).map_err(|e| e.to_string()))
}
