//! Tests for ILIKE → `lower(expr) LIKE lower(pattern)` translation.
//!
//! SQLite's built-in LIKE is case-insensitive for ASCII only when the
//! `case_sensitive_like` pragma is OFF (the default).  If any application or
//! test-runner enables that pragma, plain `LIKE` silently becomes
//! case-sensitive and ILIKE semantics are lost.  The correct translation is
//! `lower(expr) LIKE lower(pattern)`, which is pragma-independent.

use diesel::{QueryableByName, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ── 1. Output shape tests ────────────────────────────────────────────────────

/// Translated ILIKE must use `lower()` wrapping.
#[test]
fn ilike_output_uses_lower_wrapping() {
    let sql = "CREATE TABLE t (id INT, name TEXT);
               SELECT * FROM t WHERE name ILIKE '%test%'";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let select = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(select.to_lowercase().contains("lower("), "Expected lower() wrapping, got: {select}");
    assert!(!select.to_uppercase().contains("ILIKE"), "Should not contain ILIKE, got: {select}");
}

/// Translated `NOT ILIKE` must use `lower()` wrapping and produce `NOT LIKE`.
#[test]
fn not_ilike_output_uses_lower_wrapping() {
    let sql = "CREATE TABLE t (id INT, name TEXT);
               SELECT * FROM t WHERE name NOT ILIKE '%test%'";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let select = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(select.to_lowercase().contains("lower("), "Expected lower() wrapping, got: {select}");
    assert!(select.to_uppercase().contains("NOT LIKE"), "Expected NOT LIKE, got: {select}");
}

/// `lower(expr) LIKE lower(pattern)` must match every case variant even though
/// the translator now emits `PRAGMA case_sensitive_like = ON` alongside any
/// LIKE, which makes plain `LIKE` case-sensitive.
#[test]
fn ilike_matches_case_insensitively_with_case_sensitive_like_pragma() {
    let schema_sql = "CREATE TABLE words (id INTEGER PRIMARY KEY, word TEXT NOT NULL)";
    let query_sql = "SELECT * FROM words WHERE word ILIKE '%hello%'";

    let options = Pg2SqliteOptions::default();
    let ddl = Pg2Sqlite::default().sql(schema_sql).unwrap().translate(&options).unwrap();
    let query_stmts = Pg2Sqlite::default().sql(query_sql).unwrap().translate(&options).unwrap();
    // The translation leads with the pragma, so pick the query out by kind.
    assert!(
        query_stmts.iter().any(|s| matches!(s, sqlparser::ast::Statement::Pragma { .. })),
        "an ILIKE translation still carries the case-sensitive LIKE pragma"
    );
    let select_sql = query_stmts
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("a query statement")
        .to_string();

    let mut conn = SqliteConnection::establish(":memory:").unwrap();

    // Create the table
    for stmt in &ddl {
        diesel::sql_query(stmt.to_string()).execute(&mut conn).unwrap();
    }

    // Enable case-sensitive LIKE — this breaks plain LIKE for case-insensitive
    // patterns
    diesel::sql_query("PRAGMA case_sensitive_like = ON").execute(&mut conn).unwrap();

    // Insert rows with different cases of "hello"
    diesel::sql_query("INSERT INTO words VALUES (1, 'HELLO world')").execute(&mut conn).unwrap();
    diesel::sql_query("INSERT INTO words VALUES (2, 'hello world')").execute(&mut conn).unwrap();
    diesel::sql_query("INSERT INTO words VALUES (3, 'HeLLo world')").execute(&mut conn).unwrap();
    diesel::sql_query("INSERT INTO words VALUES (4, 'goodbye')").execute(&mut conn).unwrap();

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let rows = diesel::sql_query(&select_sql).load::<Row>(&mut conn).unwrap();
    assert_eq!(
        rows.len(),
        3,
        "Expected 3 rows (all 'hello' variants), got {} — translated: {select_sql}",
        rows.len()
    );
    // "goodbye" (id=4) must not appear
    assert!(rows.iter().all(|r| r.id != 4), "Row with 'goodbye' must not match");
}
