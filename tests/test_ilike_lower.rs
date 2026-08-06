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

// ---------------------------------------------------------------------------
// ESCAPE and the lower() fold (R91)
// ---------------------------------------------------------------------------

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

/// PostgreSQL, measured on 16 with `ESCAPE 'X'`: `'aXbc' ILIKE 'aXb_'` is
/// false, because `X` escapes the `b` and the pattern is three characters.
/// Before the fix the pattern was lowered while the escape stayed `X`, so the
/// escape vanished from the pattern and SQLite answered true.
#[test]
fn a_letter_escape_is_lowered_with_the_pattern() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, 'aXbc');
         SELECT count(*) FROM t WHERE s ILIKE 'aXb_' ESCAPE 'X';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("0".to_string())], "the escaped b must stay a literal");
}

/// The other direction of the same fold: `'a%bc' ILIKE 'aX%b_' ESCAPE 'X'` is
/// true in PostgreSQL, since `X%` is a literal percent, and the unlowered
/// escape turned it back into a live wildcard chased by a literal `x`.
#[test]
fn an_escaped_wildcard_survives_the_lowering() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, 'a%bc');
         SELECT count(*) FROM t WHERE s ILIKE 'aX%b_' ESCAPE 'X';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())], "the escaped percent must stay a literal");
}

/// Guards the fix. A backslash has no case, so its output is unchanged and
/// the escaped wildcard keeps working.
#[test]
fn a_backslash_escape_is_untouched() {
    let translated = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
              SELECT count(*) FROM t WHERE s ILIKE '50\\%' ESCAPE '\\';",
        )
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap()
        .join("\n");
    assert!(translated.contains("ESCAPE '\\'"), "the escape must survive verbatim: {translated}");

    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, '50%');
         SELECT count(*) FROM t WHERE s ILIKE '50\\%' ESCAPE '\\';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// An escape character whose lowering is not one character would shift the
/// pattern instead of escaping in it, so it is refused rather than folded.
#[test]
fn an_escape_whose_lowering_grows_is_refused() {
    let error = Pg2Sqlite::default()
        .sql("SELECT 'a' ILIKE 'a' ESCAPE 'İ';")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("a length-changing fold cannot escape anything")
        .to_string();
    assert!(error.contains("escape"), "the refusal must name the construct: {error}");
}
