//! Tests for GROUP N: Forward function renames.
//!
//! Simple PG → SQLite function name renames.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

#[test]
fn btrim_renames_to_trim() {
    let sql = "SELECT btrim('  hello  ')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("trim(") && !lower.contains("btrim"),
        "btrim should rename to trim: {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn jsonb_array_length_renames_to_json_array_length() {
    let sql = "SELECT jsonb_array_length('[1,2,3]')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_array_length("),
        "jsonb_array_length should rename to json_array_length: {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn json_typeof_renames_to_json_type() {
    let sql = "SELECT json_typeof('\"hello\"')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_type("), "json_typeof should rename to json_type: {result}");
    sqlite_accepts(&result);
}

#[test]
fn jsonb_typeof_renames_to_json_type() {
    let sql = "SELECT jsonb_typeof('\"hello\"')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_type("), "jsonb_typeof should rename to json_type: {result}");
    sqlite_accepts(&result);
}

#[test]
fn quote_nullable_renames_to_quote() {
    let sql = "SELECT quote_nullable('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("quote(") && !lower.contains("quote_nullable"),
        "quote_nullable should rename to quote: {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn version_renames_to_sqlite_version() {
    let sql = "SELECT version()";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("sqlite_version("),
        "version() should rename to sqlite_version(): {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn reverse_json_type_to_json_typeof() {
    let result = helpers::reverse_translate_sql("SELECT json_type('\"hello\"') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_typeof("), "json_type should reverse to json_typeof: {result}");
    pg_parses(&result);
}

#[test]
fn reverse_sqlite_version_to_version() {
    let result = helpers::reverse_translate_sql("SELECT sqlite_version() FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("version(") && !lower.contains("sqlite_version"),
        "sqlite_version should reverse to version: {result}"
    );
    pg_parses(&result);
}

#[test]
fn reverse_quote_to_quote_nullable() {
    let result = helpers::reverse_translate_sql("SELECT quote('hello') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("quote_nullable("), "quote should reverse to quote_nullable: {result}");
    pg_parses(&result);
}

/// PostgreSQL has json_array_length natively for the json type, and the
/// jsonb spelling rejects non-jsonb arguments, so a literal argument must
/// pass through under the json name. Only a column declared jsonb picks the
/// jsonb spelling.
#[test]
fn reverse_json_array_length_passes_through_for_non_jsonb_arguments() {
    let result =
        helpers::reverse_translate_sql("SELECT json_array_length('[1,2,3]') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_array_length(") && !lower.contains("jsonb_array_length("),
        "a literal argument must keep the json spelling: {result}"
    );
    pg_parses(&result);
}

/// Execute the renamed SQLite output against an in-memory connection.
fn sqlite_accepts(sql: &str) {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(sql)
        .unwrap_or_else(|e| panic!("SQLite rejected renamed output: {e}\n{sql}"));
}

/// Parse the PostgreSQL reverse-translation output to prove it is valid PG
/// syntax.
fn pg_parses(sql: &str) {
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{sql}"));
}

/// PostgreSQL's `ascii('')` answers 0, measured on 18. The plain rename to
/// `unicode` answers NULL for the same input, measured on SQLite 3.51, so the
/// empty string needs its own treatment.
#[test]
fn ascii_of_the_empty_string_is_zero() {
    let result = translate_sql("SELECT ascii('')", &default_opts()).unwrap();
    // The SQL under test is translator output, a runtime string, so the raw
    // query interface is the correct one.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let value: Option<i64> = conn.query_row(&result, [], |row| row.get(0)).unwrap();
    assert_eq!(
        value,
        Some(0),
        "PostgreSQL answers 0 for ascii(''), the translation must too: {result}"
    );
}

/// The inputs the rename already gets right, pinned so the empty-string fix
/// cannot trade them away: NULL propagates and a non-empty string answers its
/// first code point, `ascii('ab')` being 97 on both engines.
#[test]
fn ascii_keeps_null_and_first_character_semantics() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for (pg_sql, expected) in [
        ("SELECT ascii(NULL)", None),
        ("SELECT ascii('a')", Some(97)),
        ("SELECT ascii('ab')", Some(97)),
    ] {
        let result = translate_sql(pg_sql, &default_opts()).unwrap();
        let value: Option<i64> = conn.query_row(&result, [], |row| row.get(0)).unwrap();
        assert_eq!(value, expected, "{pg_sql} translated to: {result}");
    }
}
