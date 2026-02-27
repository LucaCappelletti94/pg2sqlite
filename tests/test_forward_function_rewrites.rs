//! Tests for GROUP O: Forward function rewrites.
//!
//! These PG functions need expression transformation, not just renaming.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

// ==================== localtimestamp → datetime('now', 'localtime')
// ====================

#[test]
fn localtimestamp_to_datetime_localtime() {
    let sql = "SELECT localtimestamp";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("datetime(") && lower.contains("localtime"),
        "localtimestamp should become datetime('now', 'localtime'): {result}"
    );
}

// ==================== localtime → time('now', 'localtime')
// ====================

#[test]
fn localtime_to_time_localtime() {
    let sql = "SELECT localtime";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("time(") && lower.contains("localtime"),
        "localtime should become time('now', 'localtime'): {result}"
    );
}

// ==================== to_json / to_jsonb → json() ====================

#[test]
fn to_json_renames_to_json() {
    let sql = "SELECT to_json('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json(") && !lower.contains("to_json"),
        "to_json should rename to json: {result}"
    );
}

#[test]
fn to_jsonb_renames_to_json() {
    let sql = "SELECT to_jsonb('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json(") && !lower.contains("to_jsonb"),
        "to_jsonb should rename to json: {result}"
    );
}

// ==================== jsonb_set → json_set ====================

#[test]
fn jsonb_set_renames_to_json_set() {
    let sql = "SELECT jsonb_set('{\"a\": 1}', '{a}', '2')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_set("), "jsonb_set should rename to json_set: {result}");
}

// ==================== jsonb_insert → json_insert ====================

#[test]
fn jsonb_insert_renames_to_json_insert() {
    let sql = "SELECT jsonb_insert('{\"a\": 1}', '{b}', '2')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_insert("), "jsonb_insert should rename to json_insert: {result}");
}

// ==================== jsonb_each → json_each ====================

#[test]
fn jsonb_each_renames_to_json_each() {
    let sql = "SELECT jsonb_each('{\"a\": 1}')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_each("), "jsonb_each should rename to json_each: {result}");
}

// ==================== json_each_text / jsonb_each_text → json_each
// ====================

#[test]
fn json_each_text_renames_to_json_each() {
    let sql = "SELECT json_each_text('{\"a\": 1}')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_each("), "json_each_text should rename to json_each: {result}");
}

#[test]
fn jsonb_each_text_renames_to_json_each() {
    let sql = "SELECT jsonb_each_text('{\"a\": 1}')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_each("), "jsonb_each_text should rename to json_each: {result}");
}

// ==================== quote_literal → quote ====================

#[test]
fn quote_literal_renames_to_quote() {
    let sql = "SELECT quote_literal('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("quote(") && !lower.contains("quote_literal"),
        "quote_literal should rename to quote: {result}"
    );
}

// ==================== mod(a, b) → (a % b) ====================

#[test]
fn mod_to_modulo_operator() {
    let sql = "SELECT mod(10, 3)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    assert!(result.contains('%'), "mod(a, b) should become (a % b): {result}");
}

// ==================== div(a, b) → CAST(a / b AS INTEGER) ====================

#[test]
fn div_to_integer_division() {
    let sql = "SELECT div(10, 3)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("cast(") && lower.contains("/ ") && lower.contains("integer"),
        "div(a, b) should become CAST(a / b AS INTEGER): {result}"
    );
}

// ==================== trunc(x) → CAST(x AS INTEGER) ====================

#[test]
fn trunc_single_arg_to_cast_integer() {
    let sql = "SELECT trunc(3.7)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("cast(") && lower.contains("integer"),
        "trunc(x) should become CAST(x AS INTEGER): {result}"
    );
}

// ==================== trunc(x, n) → round(x, n) ====================

#[test]
fn trunc_two_arg_to_round() {
    let sql = "SELECT trunc(3.14159, 2)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("round("), "trunc(x, n) should become round(x, n): {result}");
}

// ==================== make_date → printf('%04d-%02d-%02d', ...)
// ====================

#[test]
fn make_date_to_printf() {
    let sql = "SELECT make_date(2024, 1, 15)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_date should become printf: {result}");
}

// ==================== make_time → printf('%02d:%02d:%02d', ...)
// ====================

#[test]
fn make_time_to_printf() {
    let sql = "SELECT make_time(12, 30, 45)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_time should become printf: {result}");
}

// ==================== make_timestamp → combined printf ====================

#[test]
fn make_timestamp_to_printf() {
    let sql = "SELECT make_timestamp(2024, 1, 15, 12, 30, 45)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_timestamp should become printf: {result}");
}

// ==================== json_extract_path → json_extract ====================

#[test]
fn json_extract_path_to_json_extract() {
    let sql = "SELECT json_extract_path('{\"a\": {\"b\": 1}}', 'a', 'b')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_extract("),
        "json_extract_path should become json_extract: {result}"
    );
}

#[test]
fn jsonb_extract_path_to_json_extract() {
    let sql = "SELECT jsonb_extract_path('{\"a\": 1}', 'a')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_extract("),
        "jsonb_extract_path should become json_extract: {result}"
    );
}

#[test]
fn json_extract_path_text_to_json_extract() {
    let sql = "SELECT json_extract_path_text('{\"a\": 1}', 'a')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_extract("),
        "json_extract_path_text should become json_extract: {result}"
    );
}

#[test]
fn jsonb_extract_path_text_to_json_extract() {
    let sql = "SELECT jsonb_extract_path_text('{\"a\": 1}', 'a')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_extract("),
        "jsonb_extract_path_text should become json_extract: {result}"
    );
}
