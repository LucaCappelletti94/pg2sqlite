//! In-browser SQLite connection backed by `sqlite-wasm-rs` + Diesel.
//!
//! Thread-local because Dioxus's wasm runtime is single-threaded;
//! `RefCell` is sufficient. Mirrors `geolite/examples/web-demo/src/db.rs`,
//! minus the auto-extension registration: pg2sqlite does the translation
//! at compile time, so the in-browser SQLite only needs the stock
//! `sqlite-wasm-rs` build for now. Geolite loading lands in Step 4.

use std::cell::RefCell;

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};

thread_local! {
    static CONN: RefCell<Option<SqliteConnection>> = const { RefCell::new(None) };
}

/// Open a fresh in-memory database, replacing any previously open
/// connection. Called on every successful translation so the apply
/// step starts from a clean slate (no leftover tables from the
/// previous schema).
pub fn reopen() -> Result<(), String> {
    let conn = SqliteConnection::establish(":memory:").map_err(|e| e.to_string())?;
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
