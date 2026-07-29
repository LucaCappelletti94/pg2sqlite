//! Focused tests for the query clause gaps addressed in this fix.
//!
//! Each test asserts either the exact emitted SQL (for translated constructs)
//! or the error message text (for rejected constructs). LIMIT/OFFSET rewrite
//! tests also execute the translated SQL against an in-memory SQLite database
//! and confirm that the row count matches what PostgreSQL would return.
//!
//! Running the full conformance sweep for this group:
//!
//! ```shell
//! cargo test --release --test test_emitted_sql_is_valid_sqlite query_constructs
//! ```

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

/// Translate a snippet of PostgreSQL SQL and return every emitted statement.
fn translate(pg_sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(pg_sql)
        .map_err(|e| e.to_string())?
        .translate_to_sql(&opts())
        .map_err(|e| e.to_string())
}

/// Translate a single SELECT statement (no schema context needed).
///
/// Panics if the translation fails or if there is no SELECT in the output.
fn translate_one(pg_sql: &str) -> String {
    translate(pg_sql)
        .unwrap_or_else(|e| panic!("translation failed: {e}"))
        .into_iter()
        .find(|s| s.trim_start().to_ascii_uppercase().starts_with("SELECT"))
        .expect("no SELECT in translated output")
}

/// Translate SQL that is expected to fail and return the error string.
fn translate_err(pg_sql: &str) -> String {
    translate(pg_sql).expect_err("expected a translation error")
}

/// Open an in-memory SQLite database with a five-row `nums` table.
fn nums_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE nums (n INTEGER NOT NULL);\
         INSERT INTO nums VALUES (1),(2),(3),(4),(5);",
    )
    .unwrap();
    conn
}

/// Execute `sql` and return the number of rows produced.
fn count_rows(conn: &Connection, sql: &str) -> usize {
    // We run the raw translated string, which is the artifact under test.
    // rusqlite is correct here: the SQL is dynamically generated and
    // structurally unknown at compile time, so the Diesel DSL cannot
    // express it.
    let mut stmt =
        conn.prepare(sql).unwrap_or_else(|e| panic!("prepare failed: {e}\n  sql: {sql}"));
    stmt.query_map([], |_| Ok(())).unwrap().count()
}

// FETCH FIRST m ROWS ONLY -> LIMIT m

#[test]
fn fetch_first_rows_only_becomes_limit() {
    let sql = translate_one("SELECT n FROM nums FETCH FIRST 3 ROWS ONLY");
    assert_eq!(sql, "SELECT n FROM nums LIMIT 3");

    let conn = nums_db();
    assert_eq!(count_rows(&conn, &sql), 3, "LIMIT 3 should return 3 rows from a 5-row table");
}

// OFFSET n ROWS FETCH FIRST m ROWS ONLY -> LIMIT m OFFSET n

#[test]
fn offset_rows_fetch_first_becomes_limit_offset() {
    let sql = translate_one("SELECT n FROM nums OFFSET 2 ROWS FETCH FIRST 3 ROWS ONLY");
    assert_eq!(sql, "SELECT n FROM nums LIMIT 3 OFFSET 2");

    let conn = nums_db();
    assert_eq!(count_rows(&conn, &sql), 3, "LIMIT 3 OFFSET 2 should return 3 of 5 rows");
}

// FETCH FIRST m ROWS WITH TIES -> reject (SQLite has no equivalent)

#[test]
fn fetch_with_ties_is_rejected() {
    let err = translate_err("SELECT n FROM nums ORDER BY n FETCH FIRST 3 ROWS WITH TIES");
    assert!(err.contains("WITH TIES"), "error should mention WITH TIES, got: {err}");
}

// LATERAL on a trivially uncorrelated subquery -> drop the LATERAL keyword

#[test]
fn lateral_constant_subquery_drops_keyword() {
    // The subquery has no FROM clause and no column references: safe to inline.
    let sql = translate_one("SELECT 1 FROM nums, LATERAL (SELECT 1) AS lat");
    assert_eq!(sql, "SELECT 1 FROM nums, (SELECT 1) AS lat");

    // The result must be valid SQLite syntax.
    let conn = nums_db();
    conn.prepare(&sql)
        .unwrap_or_else(|e| panic!("translated SQL must be valid SQLite: {e}\n  sql: {sql}"));
}

// LATERAL on a correlated subquery -> reject

#[test]
fn lateral_correlated_subquery_is_rejected() {
    // The subquery references a column from the outer query scope.
    let err = translate_err("SELECT 1 FROM nums, LATERAL (SELECT nums.n) AS lat");
    assert!(err.contains("LATERAL"), "error should mention LATERAL, got: {err}");
    assert!(
        err.to_lowercase().contains("correlated") || err.to_lowercase().contains("column"),
        "error should explain the correlation issue, got: {err}"
    );
}

// Table alias with a column list -> reject

#[test]
fn alias_column_list_is_rejected() {
    let err = translate_err("SELECT a FROM (VALUES (1),(2)) AS v(a)");
    assert!(
        err.to_lowercase().contains("column list") || err.to_lowercase().contains("column"),
        "error should mention column list, got: {err}"
    );
    // The error should suggest projecting names instead.
    assert!(
        err.contains("SELECT") || err.to_lowercase().contains("project"),
        "error should suggest SELECT projection as a workaround, got: {err}"
    );
}

// TABLESAMPLE in any form -> reject, suggest ORDER BY random() LIMIT n

#[test]
fn tablesample_is_rejected() {
    let err = translate_err("SELECT n FROM nums TABLESAMPLE BERNOULLI(10)");
    assert!(
        err.to_uppercase().contains("TABLESAMPLE"),
        "error should mention TABLESAMPLE, got: {err}"
    );
    assert!(
        err.contains("random()"),
        "error should suggest ORDER BY random() LIMIT n as an approximation, got: {err}"
    );
}
