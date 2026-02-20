//! Tests for reverse function translation in
//! `src/impls/reverse_translator_impls/function.rs`.
//!
//! Covers: filter clause on group_concat (Rename path), filter clause on
//! passthrough, vec_distance_hamming, vec_f32, strftime with various formats,
//! named function args, and scalar MIN/MAX reversal.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty());
    stmts[0].to_string()
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);";
const EVENTS: &str =
    "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP, category TEXT);";
const VECTORS: &str = "CREATE TABLE embeddings (id INT PRIMARY KEY, vec VECTOR(3));";

// ==================== group_concat -> string_agg (Rename path)
// ====================

#[test]
fn reverse_group_concat_to_string_agg() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(!pg.contains("group_concat"), "Should not contain group_concat: {pg}");
}

#[test]
fn reverse_group_concat_with_filter() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
}

// ==================== Passthrough with filter ====================

#[test]
fn reverse_count_with_filter() {
    let pg = reverse(SCHEMA, "SELECT COUNT(*) FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("COUNT"), "Expected COUNT: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
}

// ==================== Vector functions ====================

#[test]
fn reverse_vec_distance_hamming() {
    let pg = reverse(
        VECTORS,
        "SELECT id FROM embeddings ORDER BY vec_distance_hamming(vec, '[1,2,3]');",
    );
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
    assert!(!pg.contains("vec_distance_hamming"), "Should not contain original function: {pg}");
}

#[test]
fn reverse_vec_distance_l2() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_L2(vec, '[1,2,3]');");
    assert!(pg.contains("<->"), "Expected <-> operator: {pg}");
}

#[test]
fn reverse_vec_distance_cosine() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_cosine(vec, '[1,2,3]');");
    assert!(pg.contains("<=>"), "Expected <=> operator: {pg}");
}

#[test]
fn reverse_vec_f32_to_cast() {
    let pg = reverse(VECTORS, "SELECT vec_f32('[1,2,3]') FROM embeddings;");
    assert!(pg.contains("::vector"), "Expected ::vector cast: {pg}");
    assert!(!pg.contains("vec_f32"), "Should not contain vec_f32: {pg}");
}

// ==================== strftime -> EXTRACT ====================

#[test]
fn reverse_strftime_year() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(YEAR"), "Expected EXTRACT(YEAR): {pg}");
}

#[test]
fn reverse_strftime_month() {
    let pg = reverse(EVENTS, "SELECT strftime('%m', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(MONTH"), "Expected EXTRACT(MONTH): {pg}");
}

#[test]
fn reverse_strftime_day() {
    let pg = reverse(EVENTS, "SELECT strftime('%d', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(DAY"), "Expected EXTRACT(DAY): {pg}");
}

#[test]
fn reverse_strftime_hour() {
    let pg = reverse(EVENTS, "SELECT strftime('%H', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(HOUR"), "Expected EXTRACT(HOUR): {pg}");
}

#[test]
fn reverse_strftime_minute() {
    let pg = reverse(EVENTS, "SELECT strftime('%M', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(MINUTE"), "Expected EXTRACT(MINUTE): {pg}");
}

#[test]
fn reverse_strftime_second() {
    let pg = reverse(EVENTS, "SELECT strftime('%S', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(SECOND"), "Expected EXTRACT(SECOND): {pg}");
}

#[test]
fn reverse_strftime_week() {
    let pg = reverse(EVENTS, "SELECT strftime('%W', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(WEEK"), "Expected EXTRACT(WEEK): {pg}");
}

#[test]
fn reverse_strftime_day_of_week() {
    let pg = reverse(EVENTS, "SELECT strftime('%w', created_at) FROM events;");
    assert!(
        pg.contains("EXTRACT(DOW") || pg.contains("EXTRACT(DAYOFWEEK"),
        "Expected DOW extract: {pg}"
    );
}

#[test]
fn reverse_strftime_day_of_year() {
    let pg = reverse(EVENTS, "SELECT strftime('%j', created_at) FROM events;");
    assert!(
        pg.contains("EXTRACT(DOY") || pg.contains("EXTRACT(DAYOFYEAR"),
        "Expected DOY extract: {pg}"
    );
}

// ==================== datetime('now') -> NOW() ====================

#[test]
fn reverse_datetime_now() {
    let pg = reverse(EVENTS, "SELECT * FROM events WHERE created_at > datetime('now');");
    assert!(pg.contains("NOW()"), "Expected NOW(): {pg}");
}

#[test]
fn reverse_datetime_utc_to_at_time_zone() {
    let pg = reverse(EVENTS, "SELECT datetime(created_at, 'utc') FROM events;");
    assert!(pg.contains("AT TIME ZONE"), "Expected AT TIME ZONE: {pg}");
    assert!(pg.contains("'UTC'"), "Expected UTC literal: {pg}");
    assert!(!pg.contains("datetime("), "Should not contain datetime call: {pg}");
}

#[test]
fn reverse_datetime_fixed_offset_to_at_time_zone() {
    let pg = reverse(EVENTS, "SELECT datetime(created_at, '+02:30') FROM events;");
    assert!(pg.contains("AT TIME ZONE"), "Expected AT TIME ZONE: {pg}");
    assert!(pg.contains("'+02:30'"), "Expected fixed offset literal: {pg}");
    assert!(!pg.contains("datetime("), "Should not contain datetime call: {pg}");
}

// ==================== INSTR -> POSITION ====================

#[test]
fn reverse_instr_to_position() {
    let pg = reverse(SCHEMA, "SELECT INSTR(name, 'a') FROM users;");
    assert!(pg.contains("POSITION"), "Expected POSITION: {pg}");
    assert!(!pg.contains("INSTR"), "Should not contain INSTR: {pg}");
}

// ==================== char -> chr ====================

#[test]
fn reverse_char_to_chr() {
    let pg = reverse(SCHEMA, "SELECT char(65) FROM users;");
    assert!(pg.contains("chr("), "Expected chr: {pg}");
    assert!(!pg.contains("char("), "Should not contain char: {pg}");
}

// ==================== scalar MIN/MAX -> LEAST/GREATEST ====================

#[test]
fn reverse_min_two_args_to_least() {
    let pg = reverse(SCHEMA, "SELECT MIN(id, age) FROM users;");
    assert!(pg.contains("LEAST"), "Expected LEAST: {pg}");
    assert!(!pg.contains("MIN("), "Should not contain scalar MIN: {pg}");
}

#[test]
fn reverse_max_two_args_to_greatest() {
    let pg = reverse(SCHEMA, "SELECT MAX(id, age) FROM users;");
    assert!(pg.contains("GREATEST"), "Expected GREATEST: {pg}");
    assert!(!pg.contains("MAX("), "Should not contain scalar MAX: {pg}");
}

#[test]
fn reverse_min_single_arg_stays_min() {
    let pg = reverse(SCHEMA, "SELECT MIN(age) FROM users;");
    assert!(pg.contains("MIN("), "Single-arg aggregate MIN should stay MIN: {pg}");
    assert!(!pg.contains("LEAST"), "Single-arg MIN should not become LEAST: {pg}");
}

// ==================== datetime passthrough (non-'now') ====================

#[test]
fn reverse_datetime_passthrough() {
    let pg = reverse(EVENTS, "SELECT datetime(created_at, '+1 day') FROM events;");
    assert!(pg.contains("datetime"), "Expected datetime passthrough: {pg}");
}

// ==================== strftime unsupported format passthrough
// ====================

#[test]
fn reverse_strftime_unsupported_format() {
    // %X is not a recognized simple format
    let pg = reverse(EVENTS, "SELECT strftime('%X', created_at) FROM events;");
    // Should pass through as strftime since format isn't recognized
    assert!(pg.contains("strftime"), "Expected strftime passthrough: {pg}");
}

// ==================== vec_distance_hamming with columns ====================

#[test]
fn reverse_vec_distance_hamming_columns() {
    let pg = reverse(VECTORS, "SELECT vec_distance_hamming(vec, vec) FROM embeddings;");
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
}
