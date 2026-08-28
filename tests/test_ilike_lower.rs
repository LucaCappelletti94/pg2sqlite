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
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn).unwrap();
    }
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
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn).unwrap();
    }
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

// ── M7: ILIKE with non-ASCII pattern literal ─────────────────────────────────

/// M7: SQLite lower() folds ASCII only. A literal pattern with non-ASCII
/// letters such as 'CAFE\u{301}%' gets wrong case folding after the lower()
/// rewrite, producing silent wrong answers. Per Decision 2, translation must
/// refuse when no fold function option is configured.
#[test]
fn non_ascii_ilike_pattern_literal_is_refused() {
    let result = Pg2Sqlite::default()
        .sql("CREATE TABLE t (name TEXT); SELECT * FROM t WHERE name ILIKE 'CAF\u{00c9}%'")
        .unwrap()
        .translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "non-ASCII ILIKE pattern must refuse without a fold option");
}

/// Green companion: a pure-ASCII pattern translates, executes, and matches
/// case-insensitively.
#[test]
fn pure_ascii_ilike_pattern_translates_and_executes() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
         INSERT INTO t VALUES (1, 'cafe');
         SELECT count(*) FROM t WHERE name ILIKE 'CAFE%';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())], "ASCII ILIKE must match case-insensitively");
}

// ── M7: with a configured fold function ──────────────────────────────────────

/// M7: with a configured fold function, 'café' ILIKE 'CAFÉ' returns true.
///
/// Rusqlite is used directly because diesel provides no API for registering a
/// custom scalar UDF, which is what makes the fold function work at runtime.
#[test]
fn with_fold_function_ilike_matches_non_ascii_case_insensitively() {
    use rusqlite::{Connection, functions::FunctionFlags};

    const FOLD: &str = "unicode_fold";

    let options = Pg2SqliteOptions::default().with_ilike_fold_function(FOLD);
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
             INSERT INTO t VALUES (1, 'caf\u{00e9}');
             SELECT count(*) FROM t WHERE name ILIKE 'CAF\u{00c9}';",
        )
        .expect("parse")
        .translate_to_sql(&options)
        .expect("translate");

    let (probe, setup) = statements.split_last().expect("at least one statement");
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    // Register the fold UDF: str::to_lowercase gives full Unicode case folding.
    conn.create_scalar_function(FOLD, 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let text: String = ctx.get(0)?;
        Ok(text.to_lowercase())
    })
    .expect("register fold UDF");
    for stmt in setup {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("setup failed: {e}\n{stmt}"));
    }
    let count: i64 = conn
        .query_row(probe, [], |row| row.get(0))
        .unwrap_or_else(|e| panic!("probe failed: {e}\n{probe}"));
    assert_eq!(count, 1, "'caf\u{00e9}' ILIKE 'CAF\u{00c9}' must match with the fold UDF");
}
