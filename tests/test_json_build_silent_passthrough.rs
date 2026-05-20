//! Regression tests for the `JSON_BUILD_ARRAY` / `JSON_BUILD_OBJECT` family.
//!
//! These tests enforce pg2sqlite's "no silent passthroughs" guarantee: every
//! PG construct must either translate to runnable SQLite or error at
//! translation time with `Error::UnsupportedSQLiteFeature(...)`. A silent
//! passthrough that fails downstream in SQLite (`no such function:
//! JSON_BUILD_ARRAY`) is exactly the failure mode pg2sqlite advertises that
//! it does not produce.
//!
//! The two SQLite equivalents:
//!
//! * `JSON_BUILD_ARRAY(...)`  -> `json_array(...)`
//! * `JSON_BUILD_OBJECT(...)` -> `json_object(...)`  (already wired)
//! * `JSONB_BUILD_ARRAY(...)` -> `json_array(...)`   (already wired)
//! * `JSONB_BUILD_OBJECT(...)`-> `json_object(...)`  (already wired)
//!
//! As of writing, only the leading `JSON_BUILD_ARRAY` case is missing from
//! the rename table in `src/impls/translator_impls/function.rs` and
//! therefore passes through unchanged, surfacing as the lone runtime ✗ in
//! `examples/compare_polyglot.rs` (case P4). The other three are present;
//! their tests below are green-on-write and act as regression coverage.

#![allow(missing_docs)]
#![cfg(feature = "std")]

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Helper: translate a PG `SELECT ... FROM t` statement, then execute it
/// against an in-memory SQLite database (pre-populated with a minimal
/// `t (a, b, c)` table) and assert it both translates and runs.
///
/// Returns the translated SQL text on success so the assertion can also
/// inspect the rewrite shape.
fn assert_translates_and_runs(pg_sql: &str) -> String {
    let stmts = Pg2Sqlite::default()
        .sql(pg_sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .expect("translation must succeed");

    let translated_sql = stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; ");

    let mut conn = SqliteConnection::establish(":memory:").expect("open in-memory SQLite");
    diesel::sql_query("CREATE TABLE t (a TEXT, b TEXT, c TEXT)")
        .execute(&mut conn)
        .expect("seed schema");
    diesel::sql_query("INSERT INTO t (a, b, c) VALUES ('x', 'y', 'z')")
        .execute(&mut conn)
        .expect("seed row");

    for stmt in &stmts {
        diesel::sql_query(stmt.to_string()).execute(&mut conn).unwrap_or_else(|e| {
            panic!(
                "translated SQL must execute in SQLite without runtime \
                     errors.\nInput PG:   {pg_sql}\nTranslated: {translated_sql}\nError:      \
                     {e}"
            )
        });
    }

    translated_sql
}

#[test]
fn json_build_array_must_rewrite_to_json_array() {
    // This is the live gap: pg2sqlite currently emits
    // `SELECT JSON_BUILD_ARRAY(a, b, c) FROM t` verbatim and SQLite then
    // fails with `no such function: JSON_BUILD_ARRAY`. Expected behavior:
    // rewrite to `json_array(a, b, c)` (the SQLite equivalent that has
    // existed since 3.38).
    let translated = assert_translates_and_runs("SELECT JSON_BUILD_ARRAY(a, b, c) FROM t");
    assert!(
        !translated.to_lowercase().contains("json_build_array"),
        "JSON_BUILD_ARRAY must not appear in translated SQL; found: {translated}"
    );
    assert!(
        translated.to_lowercase().contains("json_array"),
        "translated SQL should use SQLite's json_array; got: {translated}"
    );
}

#[test]
fn json_build_object_rewrites_to_json_object() {
    let translated = assert_translates_and_runs("SELECT JSON_BUILD_OBJECT('a', a, 'b', b) FROM t");
    assert!(
        !translated.to_lowercase().contains("json_build_object"),
        "JSON_BUILD_OBJECT must not appear in translated SQL; found: {translated}"
    );
    assert!(
        translated.to_lowercase().contains("json_object"),
        "translated SQL should use SQLite's json_object; got: {translated}"
    );
}

#[test]
fn jsonb_build_array_rewrites_to_json_array() {
    let translated = assert_translates_and_runs("SELECT JSONB_BUILD_ARRAY(a, b, c) FROM t");
    assert!(
        !translated.to_lowercase().contains("jsonb_build_array"),
        "JSONB_BUILD_ARRAY must not appear in translated SQL; found: {translated}"
    );
    assert!(
        translated.to_lowercase().contains("json_array"),
        "translated SQL should use SQLite's json_array; got: {translated}"
    );
}

#[test]
fn jsonb_build_object_rewrites_to_json_object() {
    let translated = assert_translates_and_runs("SELECT JSONB_BUILD_OBJECT('a', a, 'b', b) FROM t");
    assert!(
        !translated.to_lowercase().contains("jsonb_build_object"),
        "JSONB_BUILD_OBJECT must not appear in translated SQL; found: {translated}"
    );
    assert!(
        translated.to_lowercase().contains("json_object"),
        "translated SQL should use SQLite's json_object; got: {translated}"
    );
}
