//! Tests for GROUP N: Forward function renames.
//!
//! Simple PG → SQLite function name renames.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

// ==================== btrim → trim ====================

#[test]
fn btrim_renames_to_trim() {
    let sql = "SELECT btrim('  hello  ')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("trim(") && !lower.contains("btrim"),
        "btrim should rename to trim: {result}"
    );
}

// ==================== jsonb_array_length → json_array_length
// ====================

#[test]
fn jsonb_array_length_renames_to_json_array_length() {
    let sql = "SELECT jsonb_array_length('[1,2,3]')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_array_length("),
        "jsonb_array_length should rename to json_array_length: {result}"
    );
}

// ==================== json_typeof / jsonb_typeof → json_type
// ====================

#[test]
fn json_typeof_renames_to_json_type() {
    let sql = "SELECT json_typeof('\"hello\"')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_type("), "json_typeof should rename to json_type: {result}");
}

#[test]
fn jsonb_typeof_renames_to_json_type() {
    let sql = "SELECT jsonb_typeof('\"hello\"')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_type("), "jsonb_typeof should rename to json_type: {result}");
}

// ==================== quote_nullable → quote ====================

#[test]
fn quote_nullable_renames_to_quote() {
    let sql = "SELECT quote_nullable('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("quote(") && !lower.contains("quote_nullable"),
        "quote_nullable should rename to quote: {result}"
    );
}

// ==================== version() → sqlite_version() ====================

#[test]
fn version_renames_to_sqlite_version() {
    let sql = "SELECT version()";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("sqlite_version("),
        "version() should rename to sqlite_version(): {result}"
    );
}

// ==================== Reverse: json_type → json_typeof ====================

#[test]
fn reverse_json_type_to_json_typeof() {
    let result = helpers::reverse_translate_sql("SELECT json_type('\"hello\"') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_typeof("), "json_type should reverse to json_typeof: {result}");
}

// ==================== Reverse: sqlite_version → version ====================

#[test]
fn reverse_sqlite_version_to_version() {
    let result = helpers::reverse_translate_sql("SELECT sqlite_version() FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("version(") && !lower.contains("sqlite_version"),
        "sqlite_version should reverse to version: {result}"
    );
}

// ==================== Reverse: quote → quote_nullable ====================

#[test]
fn reverse_quote_to_quote_nullable() {
    let result = helpers::reverse_translate_sql("SELECT quote('hello') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("quote_nullable("), "quote should reverse to quote_nullable: {result}");
}

// ==================== Reverse: json_array_length → jsonb_array_length
// ====================

#[test]
fn reverse_json_array_length_to_jsonb_array_length() {
    let result =
        helpers::reverse_translate_sql("SELECT json_array_length('[1,2,3]') FROM t").unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("jsonb_array_length("),
        "json_array_length should reverse to jsonb_array_length: {result}"
    );
}
