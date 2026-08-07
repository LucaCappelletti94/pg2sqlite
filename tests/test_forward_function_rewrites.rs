//! Tests for GROUP O: Forward function rewrites.
//!
//! These PG functions need expression transformation, not just renaming.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

#[test]
fn localtimestamp_to_datetime_localtime() {
    let sql = "SELECT localtimestamp";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("datetime(") && lower.contains("localtime"),
        "localtimestamp should become datetime('now', 'localtime'): {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn localtime_to_time_localtime() {
    let sql = "SELECT localtime";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("time(") && lower.contains("localtime"),
        "localtime should become time('now', 'localtime'): {result}"
    );
    sqlite_accepts(&result);
}

/// Inverted from `to_json_renames_to_json`. The rename emitted `json('hello')`,
/// which fails with `malformed JSON` because `json()` reads its argument as
/// JSON rather than converting it. Behaviour is proven by execution in
/// `tests/test_to_json.rs`, so this only pins the shape.
#[test]
fn to_json_converts_rather_than_reinterprets() {
    let sql = "SELECT to_json('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_quote(") && !lower.contains("to_json"),
        "to_json should convert through json_quote: {result}"
    );
    sqlite_accepts(&result);
}

/// Inverted from `to_jsonb_renames_to_json`, for the same reason.
#[test]
fn to_jsonb_converts_rather_than_reinterprets() {
    let sql = "SELECT to_jsonb('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_quote(") && !lower.contains("to_jsonb"),
        "to_jsonb should convert through json_quote: {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn jsonb_set_renames_to_json_set() {
    let sql = "SELECT jsonb_set('{\"a\": 1}', '{a}', '2')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_set("), "jsonb_set should rename to json_set: {result}");
    sqlite_accepts(&result);
}

#[test]
fn jsonb_insert_renames_to_json_insert() {
    let sql = "SELECT jsonb_insert('{\"a\": 1}', '{b}', '2')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("json_insert("), "jsonb_insert should rename to json_insert: {result}");
    sqlite_accepts(&result);
}

/// Flipped R121 pin. The scalar rename emitted `json_each(...)` in a SELECT
/// list, which SQLite refuses with `no such function: json_each`, because its
/// json_each exists only as a table in FROM. The family now refuses scalar
/// position naming the FROM rewrite, whose translation
/// `tests/test_set_returning_functions.rs` proves by execution.
#[test]
fn jsonb_each_in_a_select_list_is_refused() {
    let err = translate_sql("SELECT jsonb_each('{\"a\": 1}')", &default_opts())
        .expect_err("a set returning function in a SELECT list cannot become a scalar");
    let message = err;
    assert!(
        message.contains("jsonb_each") && message.contains("FROM"),
        "the refusal should name the function and the FROM rewrite: {message}"
    );
}

/// Flipped R121 pin, the `json_each_text` spelling.
#[test]
fn json_each_text_in_a_select_list_is_refused() {
    let err = translate_sql("SELECT json_each_text('{\"a\": 1}')", &default_opts())
        .expect_err("a set returning function in a SELECT list cannot become a scalar");
    let message = err;
    assert!(
        message.contains("json_each_text") && message.contains("FROM"),
        "the refusal should name the function and the FROM rewrite: {message}"
    );
}

/// Flipped R121 pin, the `jsonb_each_text` spelling.
#[test]
fn jsonb_each_text_in_a_select_list_is_refused() {
    let err = translate_sql("SELECT jsonb_each_text('{\"a\": 1}')", &default_opts())
        .expect_err("a set returning function in a SELECT list cannot become a scalar");
    let message = err;
    assert!(
        message.contains("jsonb_each_text") && message.contains("FROM"),
        "the refusal should name the function and the FROM rewrite: {message}"
    );
}

/// The fourth door into the same defect: `json_each` is a PostgreSQL function
/// too, and it sits in the SQLite builtin list, so scalar position passed it
/// through rather than renaming it, failing identically at run time.
#[test]
fn json_each_in_a_select_list_is_refused() {
    let err = translate_sql("SELECT json_each('{\"a\": 1}')", &default_opts())
        .expect_err("a set returning function in a SELECT list cannot become a scalar");
    let message = err;
    assert!(
        message.contains("json_each") && message.contains("FROM"),
        "the refusal should name the function and the FROM rewrite: {message}"
    );
}

#[test]
fn quote_literal_renames_to_quote() {
    let sql = "SELECT quote_literal('hello')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("quote(") && !lower.contains("quote_literal"),
        "quote_literal should rename to quote: {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn mod_to_modulo_operator() {
    let sql = "SELECT mod(10, 3)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    assert!(result.contains('%'), "mod(a, b) should become (a % b): {result}");
    sqlite_accepts(&result);
}

#[test]
fn div_to_integer_division() {
    let sql = "SELECT div(10, 3)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("cast(") && lower.contains("/ ") && lower.contains("integer"),
        "div(a, b) should become CAST(a / b AS INTEGER): {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn trunc_single_arg_to_cast_integer() {
    let sql = "SELECT trunc(3.7)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("cast(") && lower.contains("integer"),
        "trunc(x) should become CAST(x AS INTEGER): {result}"
    );
    sqlite_accepts(&result);
}

#[test]
fn make_date_to_printf() {
    let sql = "SELECT make_date(2024, 1, 15)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_date should become printf: {result}");
    sqlite_accepts(&result);
}

#[test]
fn make_time_to_printf() {
    let sql = "SELECT make_time(12, 30, 45)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_time should become printf: {result}");
    sqlite_accepts(&result);
}

#[test]
fn make_timestamp_to_printf() {
    let sql = "SELECT make_timestamp(2024, 1, 15, 12, 30, 45)";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(lower.contains("printf("), "make_timestamp should become printf: {result}");
    sqlite_accepts(&result);
}

#[test]
fn json_extract_path_to_json_extract() {
    let sql = "SELECT json_extract_path('{\"a\": {\"b\": 1}}', 'a', 'b')";
    let result = translate_sql(sql, &default_opts()).unwrap();
    let lower = result.to_lowercase();
    assert!(
        lower.contains("json_extract("),
        "json_extract_path should become json_extract: {result}"
    );
    sqlite_accepts(&result);
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
    sqlite_accepts(&result);
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
    sqlite_accepts(&result);
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
    sqlite_accepts(&result);
}

/// Executes `sql` against an in-memory SQLite to prove the translator's output
/// is valid. The translated SQL is dynamically generated by the translator.
fn sqlite_accepts(sql: &str) {
    rusqlite::Connection::open_in_memory()
        .expect("in-memory SQLite")
        .execute_batch(sql)
        .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{sql}"));
}
