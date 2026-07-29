//! Translations that SQLite accepts but that mean something different from the
//! PostgreSQL they came from.
//!
//! A construct that fails to parse is caught by
//! `test_emitted_sql_is_valid_sqlite.rs`. This file covers the worse case: SQL
//! that runs and quietly returns the wrong answer. Each test states the value
//! PostgreSQL produces and asserts that the translated SQLite produces the
//! same.
//!
//! The expected PostgreSQL values are from the documented semantics of each
//! function, noted per test.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT);";

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translates")
}

/// Apply the translated DDL, seed one row, then evaluate the translated form of
/// `select_list` and return it as text.
fn eval_on(row: &str, select_list: &str) -> Option<String> {
    let script = translate(&format!("{DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(row).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<String>>(0))
        .unwrap_or_else(|e| panic!("emitted query failed: {e}\n{probe}"))
}

/// Rows surviving a translated WHERE clause, in id order.
fn rows_matching(predicate: &str) -> Vec<String> {
    let script = translate(&format!("{DDL}\nSELECT s FROM t WHERE {predicate} ORDER BY id;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};")).expect("emitted DDL");
    }
    conn.execute_batch("INSERT INTO t (id, s) VALUES (1, 'Alpha'), (2, 'alpha'), (3, 'beta');")
        .expect("seed rows");
    let probe = script.last().expect("a query");
    let mut stmt =
        conn.prepare(probe).unwrap_or_else(|e| panic!("emitted query failed: {e}\n{probe}"));
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("runs")
        .collect::<Result<_, _>>()
        .expect("decodes")
}

/// PostgreSQL's `LIKE` is case-sensitive. SQLite's is case-insensitive for
/// ASCII unless the connection sets `case_sensitive_like`, so a bare
/// passthrough silently matches rows PostgreSQL would not.
#[test]
fn like_is_case_sensitive() {
    assert_eq!(
        rows_matching("s LIKE 'a%'"),
        vec!["alpha".to_string()],
        "PostgreSQL matches only the lowercase row"
    );
}

/// `ILIKE` is the case-insensitive one, and must stay case-insensitive after
/// whatever makes `LIKE` case-sensitive.
#[test]
fn ilike_stays_case_insensitive() {
    assert_eq!(
        rows_matching("s ILIKE 'a%'"),
        vec!["Alpha".to_string(), "alpha".to_string()],
        "PostgreSQL ILIKE matches both cases"
    );
}

#[test]
fn not_like_is_case_sensitive() {
    assert_eq!(
        rows_matching("s NOT LIKE 'a%'"),
        vec!["Alpha".to_string(), "beta".to_string()],
        "PostgreSQL excludes only the lowercase match"
    );
}

/// A pattern computed at runtime cannot be rewritten at translation time, so
/// this is the case that proves the fix is not merely a literal-pattern trick.
#[test]
fn like_against_a_column_pattern_is_case_sensitive() {
    let script = translate(&format!(
        "{DDL}\nSELECT a.s FROM t AS a JOIN t AS b ON a.s LIKE b.s WHERE b.id = 2 ORDER BY a.id;"
    ));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};")).expect("emitted DDL");
    }
    conn.execute_batch("INSERT INTO t (id, s) VALUES (1, 'Alpha'), (2, 'alpha');")
        .expect("seed rows");
    let probe = script.last().expect("a query");
    let mut stmt = conn.prepare(probe).expect("emitted query parses");
    let got: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("runs")
        .collect::<Result<_, _>>()
        .expect("decodes");
    assert_eq!(got, vec!["alpha".to_string()], "only the exact-case row matches in PostgreSQL");
}

/// `left(s, n)` with negative `n` returns all but the last `|n|` characters.
/// SQLite's `substr(s, 1, n)` returns the empty string for a negative length.
#[test]
fn left_with_a_negative_count_drops_from_the_end() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'Alpha');";
    assert_eq!(eval_on(row, "left(s, 2)").as_deref(), Some("Al"));
    assert_eq!(eval_on(row, "left(s, -1)").as_deref(), Some("Alph"));
    assert_eq!(eval_on(row, "left(s, -5)").as_deref(), Some(""));
    assert_eq!(eval_on(row, "left(s, -9)").as_deref(), Some(""));
    assert_eq!(eval_on(row, "left(s, 0)").as_deref(), Some(""));
}

/// `right(s, n)` with negative `n` returns all but the first `|n|` characters.
/// SQLite's `substr(s, -n)` counts a positive offset from the start instead.
#[test]
fn right_with_a_negative_count_drops_from_the_start() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'Alpha');";
    assert_eq!(eval_on(row, "right(s, 2)").as_deref(), Some("ha"));
    assert_eq!(eval_on(row, "right(s, -1)").as_deref(), Some("lpha"));
    assert_eq!(eval_on(row, "right(s, -5)").as_deref(), Some(""));
    assert_eq!(eval_on(row, "right(s, -9)").as_deref(), Some(""));
    assert_eq!(eval_on(row, "right(s, 0)").as_deref(), Some(""));
}

/// A runtime count has to be handled too, so the fix cannot just fold literals.
#[test]
fn left_and_right_handle_a_computed_count() {
    let row = "INSERT INTO t (id, s, n) VALUES (1, 'Alpha', -1);";
    assert_eq!(eval_on(row, "left(s, n)").as_deref(), Some("Alph"));
    assert_eq!(eval_on(row, "right(s, n)").as_deref(), Some("lpha"));
}

/// PostgreSQL counts `substring` positions from one and clamps a start below
/// one, keeping `for` characters measured from the original start. SQLite reads
/// a negative start as an offset from the end of the string.
#[test]
fn substring_from_a_negative_start_clamps() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'Alpha');";
    assert_eq!(eval_on(row, "substring(s FROM 2 FOR 3)").as_deref(), Some("lph"));
    // PostgreSQL: characters at positions -2..0 do not exist, so the result is
    // empty rather than the last characters of the string.
    assert_eq!(eval_on(row, "substring(s FROM -2 FOR 3)").as_deref(), Some(""));
    assert_eq!(eval_on(row, "substring(s FROM -2 FOR 4)").as_deref(), Some("A"));
    assert_eq!(eval_on(row, "substring(s FROM 0 FOR 2)").as_deref(), Some("A"));
}
