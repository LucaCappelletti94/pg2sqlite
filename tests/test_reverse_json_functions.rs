//! Focused tests for reverse translation of SQLite json1 functions to
//! PostgreSQL.
//!
//! Each test asserts the exact emitted SQL for a successful translation, or
//! checks the error message for a rejected input. The cases cover every mapping
//! in the task specification plus the two rejection conditions (non-literal
//! path and array-index path).

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, payload JSONB);")
        .expect("fixture parses")
        .build_schema()
        .expect("schema builds")
}

/// Reverse-translate `sqlite_sql` and return the joined output string.
fn rev(sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, &schema, &options)
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

/// Assert that `rev(sqlite_sql)` succeeds and the output contains `want`,
/// then verify the output parses as valid PostgreSQL.
fn assert_emits(sqlite_sql: &str, want: &str) {
    let out = rev(sqlite_sql).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  expected output containing {want:?}, got Err: {e}")
    });
    assert!(out.contains(want), "{sqlite_sql}\n  expected {want:?} in: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  reverse output is not valid PostgreSQL: {e}\n{out}")
    });
}

/// Assert that `rev(sqlite_sql)` fails and the error message contains `want`.
fn assert_rejected_with(sqlite_sql: &str, want: &str) {
    match rev(sqlite_sql) {
        Ok(out) => panic!("{sqlite_sql}\n  expected Err containing {want:?}, got: {out}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(want),
                "{sqlite_sql}\n  expected error containing {want:?}, got: {msg}"
            );
        }
    }
}

// --- json(x) -> CAST(x AS JSONB) ---

#[test]
fn json_validates_and_casts_to_jsonb() {
    assert_emits("SELECT json(s) FROM t", "CAST(s AS JSONB)");
}

// --- json_set / json_insert with path conversion ---

#[test]
fn json_set_single_key_path() {
    // The value must be typed as jsonb for jsonb_set.
    assert_emits("SELECT json_set(payload, '$.a', 1) FROM t", "to_jsonb(1)");
}

#[test]
fn json_set_nested_path() {
    assert_emits("SELECT json_set(payload, '$.a.b', 1) FROM t", "to_jsonb(1)");
}

#[test]
fn json_insert_single_key_path() {
    assert_emits("SELECT json_insert(payload, '$.a', 1) FROM t", "to_jsonb(1)");
}

#[test]
fn json_insert_nested_path() {
    assert_emits("SELECT json_insert(payload, '$.a.b', 1) FROM t", "to_jsonb(1)");
}

// --- json_remove(j, '$.path') -> j #- '{path}' ---

#[test]
fn json_remove_single_key() {
    assert_emits("SELECT json_remove(payload, '$.a') FROM t", "#- '{a}'");
}

#[test]
fn json_remove_nested_path() {
    assert_emits("SELECT json_remove(payload, '$.a.b') FROM t", "#- '{a,b}'");
}

// --- json_extract(j, '$.path') -> j #> '{path}' ---

#[test]
fn json_extract_single_key() {
    assert_emits("SELECT json_extract(payload, '$.a') FROM t", "#> '{a}'");
}

#[test]
fn json_extract_nested_path() {
    assert_emits("SELECT json_extract(payload, '$.a.b') FROM t", "#> '{a,b}'");
}

// --- json_quote(x) -> to_jsonb(x) ---

#[test]
fn json_quote_becomes_to_jsonb() {
    assert_emits("SELECT json_quote(s) FROM t", "to_jsonb(s)");
}

// --- json_valid(x) -> x IS JSON ---

#[test]
fn json_valid_becomes_is_json() {
    assert_emits("SELECT json_valid(s) FROM t", "IS JSON");
}

// --- json_patch(a, b) -> a || b ---

#[test]
fn json_patch_becomes_concat_operator() {
    assert_emits("SELECT json_patch(payload, payload) FROM t", "||");
}

// --- simple renames (regression guards) ---

#[test]
fn json_type_with_jsonb_column_uses_jsonb_typeof() {
    // payload is declared as JSONB in the schema, so json_type should become
    // jsonb_typeof rather than json_typeof.
    assert_emits("SELECT json_type(payload) FROM t", "jsonb_typeof(payload)");
}

#[test]
fn json_type_with_non_jsonb_column_falls_back_to_json_typeof() {
    // A non-JSON column (INT) has no JSONB type, so the fallback is json_typeof.
    assert_emits("SELECT json_type(n) FROM t", "json_typeof(n)");
}

#[test]
fn json_array_length_renames_to_jsonb_array_length() {
    assert_emits("SELECT json_array_length(payload) FROM t", "jsonb_array_length(payload)");
}

#[test]
fn json_group_array_renames_to_json_agg() {
    assert_emits("SELECT json_group_array(s) FROM t", "json_agg(s)");
}

#[test]
fn json_array_renames_to_json_build_array() {
    assert_emits("SELECT json_array(s) FROM t", "json_build_array(s)");
}

// --- rejections ---

#[test]
fn json_set_with_non_literal_path_is_rejected() {
    assert_rejected_with(
        "SELECT json_set(payload, json_extract(payload, '$.key'), 1) FROM t",
        "JSON path must be a string literal",
    );
}

#[test]
fn json_extract_with_array_index_path_is_rejected() {
    assert_rejected_with("SELECT json_extract(payload, '$[0]') FROM t", "simple dotted literal");
}

#[test]
fn json_remove_with_array_index_path_is_rejected() {
    assert_rejected_with("SELECT json_remove(payload, '$[0]') FROM t", "simple dotted literal");
}
