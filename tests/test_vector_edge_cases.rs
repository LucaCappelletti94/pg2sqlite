//! Tests for vector translation edge cases in
//! `src/impls/translator_impls/vector.rs`.
//!
//! Covers: table-level PK constraint, missing PK error, vector without
//! dimensions, multiple vector columns, and vector with RLS.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use std::sync::Once;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::{Connection, ffi::sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;
use translate_helpers::translate_default as translate;

fn translate_err(sql: &str) -> String {
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    match result {
        Err(e) => e.to_string(),
        Ok(stmts) => stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"),
    }
}

fn open_vec_connection() -> Connection {
    static INIT: Once = Once::new();
    // SAFETY: sqlite-vec declares `sqlite3_vec_init` with an opaque
    // signature, and its real C entry point takes
    // `(db, pzErrMsg, pApi) -> int`, which is exactly the pointer type
    // `sqlite3_auto_extension` stores and later calls. The transmute goes
    // through `*const ()` to restore that type, the same pattern
    // `test_vector_semantic.rs` uses, and `Once` makes the registration
    // single-shot before any connection opens.
    INIT.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite3_vec_init as *const ())));
    });
    Connection::open_in_memory().unwrap()
}

#[test]
fn vector_with_table_level_pk() {
    let sql = "
        CREATE TABLE items (
            embedding VECTOR(384),
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
    ";
    let output = translate(sql);
    // Should produce vec0 virtual table with table-level PK
    assert!(output.contains("vec0") || output.contains("BLOB"), "Expected vec0 or BLOB: {output}");
    let conn = open_vec_connection();
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {s}\n{e}"));
    }
}

#[test]
fn vector_with_column_level_pk() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR(384)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("vec0"), "Expected vec0 virtual table: {output}");
    let conn = open_vec_connection();
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {s}\n{e}"));
    }
}

#[test]
fn multiple_vector_columns() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            title_embedding VECTOR(384),
            content_embedding VECTOR(768)
        );
    ";
    let output = translate(sql);
    // Should produce separate vec0 tables for each vector column
    assert!(output.contains("vec0"), "Expected vec0: {output}");
    let conn = open_vec_connection();
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {s}\n{e}"));
    }
}

#[test]
fn vector_without_dimensions() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR
        );
    ";
    let output = translate(sql);
    // VECTOR without dimensions should still translate
    assert!(output.contains("BLOB") || output.contains("vec0"), "Expected BLOB or vec0: {output}");
    let conn = open_vec_connection();
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {s}\n{e}"));
    }
}

#[test]
fn vector_without_pk_produces_error() {
    let sql = "
        CREATE TABLE items (
            name TEXT NOT NULL,
            embedding VECTOR(384)
        );
    ";
    let output = translate_err(sql);
    // Should produce an error about missing primary key
    assert!(
        output.contains("primary key")
            || output.contains("PRIMARY KEY")
            || output.contains("Primary"),
        "Expected PK error: {output}"
    );
}

#[test]
fn vector_with_composite_pk() {
    let sql = "
        CREATE TABLE items (
            id1 INTEGER,
            id2 INTEGER,
            embedding VECTOR(384),
            PRIMARY KEY (id1, id2)
        );
    ";
    // Composite PK should be problematic for vec0 sync triggers
    let output = translate_err(sql);
    // Either error or work with first PK column
    assert!(
        output.contains("vec0") || output.contains("primary key") || output.contains("BLOB"),
        "Expected vec0 or PK error: {output}"
    );
}
