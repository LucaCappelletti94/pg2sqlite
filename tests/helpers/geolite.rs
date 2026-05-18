//! Test harness that registers geolite spatial functions onto every
//! `SqliteConnection::establish()` via SQLite's `sqlite3_auto_extension`.
//!
//! Only compiled when the `geolite` cargo feature is enabled.

#![allow(dead_code)]

use std::sync::Once;

use diesel::{Connection, sqlite::SqliteConnection};

static INIT: Once = Once::new();

/// SQLite auto-extension entry point. Invoked on every new connection.
///
/// # Safety
/// `db` must be a valid SQLite handle owned by the caller; SQLite guarantees
/// this for auto-extension callbacks.
unsafe extern "C" fn geolite_init(
    db: *mut libsqlite3_sys::sqlite3,
    _pz_err_msg: *mut *mut std::ffi::c_char,
    _p_api: *const libsqlite3_sys::sqlite3_api_routines,
) -> std::ffi::c_int {
    unsafe { geolite_sqlite::register_functions(db) }
}

/// Returns a fresh in-memory `SqliteConnection` with geolite's spatial
/// functions registered. Safe to call from any test; the auto-extension
/// registration runs once per process.
#[must_use]
pub fn geolite_connection() -> SqliteConnection {
    INIT.call_once(|| {
        // SAFETY: `sqlite3_auto_extension` stores the function pointer in a
        // global table. Single-threaded init is enforced by `Once`.
        let rc = unsafe { libsqlite3_sys::sqlite3_auto_extension(Some(geolite_init)) };
        assert_eq!(rc, 0, "sqlite3_auto_extension failed with rc={rc}");
    });
    SqliteConnection::establish(":memory:").expect("open :memory: SQLite")
}
