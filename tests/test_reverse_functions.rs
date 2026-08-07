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

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    translator
        .reverse_sql(sqlite_sql, &schema, &options)
        .expect_err("this SQLite has no PostgreSQL spelling")
        .to_string()
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);";
const EVENTS: &str =
    "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP, category TEXT);";
const VECTORS: &str = "CREATE TABLE embeddings (id INT PRIMARY KEY, vec VECTOR(3));";

#[test]
fn reverse_group_concat_to_string_agg() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(!pg.contains("group_concat"), "Should not contain group_concat: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_group_concat_with_filter() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_count_with_filter() {
    let pg = reverse(SCHEMA, "SELECT COUNT(*) FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("COUNT"), "Expected COUNT: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_hamming() {
    let pg = reverse(
        VECTORS,
        "SELECT id FROM embeddings ORDER BY vec_distance_hamming(vec, '[1,2,3]');",
    );
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
    assert!(!pg.contains("vec_distance_hamming"), "Should not contain original function: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_l2() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_L2(vec, '[1,2,3]');");
    assert!(pg.contains("<->"), "Expected <-> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_cosine() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_cosine(vec, '[1,2,3]');");
    assert!(pg.contains("<=>"), "Expected <=> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_f32_to_cast() {
    let pg = reverse(VECTORS, "SELECT vec_f32('[1,2,3]') FROM embeddings;");
    assert!(pg.contains("::vector"), "Expected ::vector cast: {pg}");
    assert!(!pg.contains("vec_f32"), "Should not contain vec_f32: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_year() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(YEAR"), "Expected EXTRACT(YEAR): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_month() {
    let pg = reverse(EVENTS, "SELECT strftime('%m', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(MONTH"), "Expected EXTRACT(MONTH): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_day() {
    let pg = reverse(EVENTS, "SELECT strftime('%d', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(DAY"), "Expected EXTRACT(DAY): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_hour() {
    let pg = reverse(EVENTS, "SELECT strftime('%H', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(HOUR"), "Expected EXTRACT(HOUR): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_minute() {
    let pg = reverse(EVENTS, "SELECT strftime('%M', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(MINUTE"), "Expected EXTRACT(MINUTE): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_second() {
    let pg = reverse(EVENTS, "SELECT strftime('%S', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(SECOND"), "Expected EXTRACT(SECOND): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_week() {
    let pg = reverse(EVENTS, "SELECT strftime('%V', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(WEEK"), "Expected EXTRACT(WEEK): {pg}");
    assert_parses_as_pg(&pg);
}

/// `%W` is the Sunday based week and has no PostgreSQL field: `EXTRACT(WEEK)`
/// is the ISO one, so reversing it that way would change the answer.
#[test]
fn reverse_strftime_sunday_week_is_not_extract_week() {
    let pg = reverse(EVENTS, "SELECT strftime('%W', created_at) FROM events;");
    assert!(!pg.contains("EXTRACT(WEEK"), "a Sunday based week is not the ISO one: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_day_of_week() {
    let pg = reverse(EVENTS, "SELECT strftime('%w', created_at) FROM events;");
    assert!(
        pg.contains("EXTRACT(DOW") || pg.contains("EXTRACT(DAYOFWEEK"),
        "Expected DOW extract: {pg}"
    );
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_day_of_year() {
    let pg = reverse(EVENTS, "SELECT strftime('%j', created_at) FROM events;");
    assert!(
        pg.contains("EXTRACT(DOY") || pg.contains("EXTRACT(DAYOFYEAR"),
        "Expected DOY extract: {pg}"
    );
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_datetime_now() {
    let pg = reverse(EVENTS, "SELECT * FROM events WHERE created_at > datetime('now');");
    assert!(pg.contains("NOW()"), "Expected NOW(): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_schema_qualified_datetime_now() {
    let pg = reverse(EVENTS, "SELECT * FROM events WHERE created_at > main.datetime('now');");
    assert!(pg.contains("NOW()"), "Expected NOW() from schema-qualified datetime: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_datetime_utc_to_at_time_zone() {
    let pg = reverse(EVENTS, "SELECT datetime(created_at, 'utc') FROM events;");
    assert!(pg.contains("AT TIME ZONE"), "Expected AT TIME ZONE: {pg}");
    assert!(pg.contains("'UTC'"), "Expected UTC literal: {pg}");
    assert!(!pg.contains("datetime("), "Should not contain datetime call: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_datetime_fixed_offset_to_at_time_zone() {
    let pg = reverse(EVENTS, "SELECT datetime(created_at, '+02:30') FROM events;");
    assert!(pg.contains("AT TIME ZONE"), "Expected AT TIME ZONE: {pg}");
    assert!(pg.contains("'+02:30'"), "Expected fixed offset literal: {pg}");
    assert!(!pg.contains("datetime("), "Should not contain datetime call: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_instr_to_position() {
    let pg = reverse(SCHEMA, "SELECT INSTR(name, 'a') FROM users;");
    assert!(pg.contains("POSITION"), "Expected POSITION: {pg}");
    assert!(!pg.contains("INSTR"), "Should not contain INSTR: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_char_to_chr() {
    let pg = reverse(SCHEMA, "SELECT char(65) FROM users;");
    assert!(pg.contains("chr("), "Expected chr: {pg}");
    assert!(!pg.contains("char("), "Should not contain char: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_min_two_args_to_least() {
    let pg = reverse(SCHEMA, "SELECT MIN(id, age) FROM users;");
    assert!(pg.contains("LEAST"), "Expected LEAST: {pg}");
    assert!(!pg.contains("MIN("), "Should not contain scalar MIN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_max_two_args_to_greatest() {
    let pg = reverse(SCHEMA, "SELECT MAX(id, age) FROM users;");
    assert!(pg.contains("GREATEST"), "Expected GREATEST: {pg}");
    assert!(!pg.contains("MAX("), "Should not contain scalar MAX: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_min_single_arg_stays_min() {
    let pg = reverse(SCHEMA, "SELECT MIN(age) FROM users;");
    assert!(pg.contains("MIN("), "Single-arg aggregate MIN should stay MIN: {pg}");
    assert!(!pg.contains("LEAST"), "Single-arg MIN should not become LEAST: {pg}");
    assert_parses_as_pg(&pg);
}

/// Inverts the R98 pin. PostgreSQL has no `datetime` function, so a modifier
/// that is neither a time zone nor `unixepoch` has nothing to reverse onto,
/// and passing the call through emitted SQL PostgreSQL refuses.
#[test]
fn an_untranslatable_datetime_modifier_is_refused() {
    for modifier in ["+1 day", "start of month", "weekday 0"] {
        let error =
            reverse_err(EVENTS, &format!("SELECT datetime(created_at, '{modifier}') FROM events;"));
        assert!(error.contains(modifier), "the refusal must name the modifier: {error}");
    }
}

/// A modifier chain has no single zone to reverse onto either, and naming
/// only its first modifier would mislead, so the refusal is the generic one.
#[test]
fn a_datetime_modifier_chain_is_refused() {
    let error = reverse_err(EVENTS, "SELECT datetime(created_at, 'utc', '+1 day') FROM events;");
    assert!(error.contains("datetime"), "the refusal must name the function: {error}");
}

/// Guards the fix. The zone and epoch spellings keep their reversals.
#[test]
fn zone_and_epoch_spellings_still_reverse() {
    let zone = reverse(EVENTS, "SELECT datetime(created_at, 'utc') FROM events;");
    assert!(zone.contains("AT TIME ZONE"), "the zone reversal must survive: {zone}");
    let epoch = reverse(EVENTS, "SELECT datetime(id, 'unixepoch') FROM events;");
    assert!(epoch.contains("to_timestamp"), "the epoch reversal must survive: {epoch}");
    assert_parses_as_pg(&zone);
    assert_parses_as_pg(&epoch);
}

#[test]
fn reverse_strftime_unsupported_format() {
    // %X is not a recognized simple format
    let pg = reverse(EVENTS, "SELECT strftime('%X', created_at) FROM events;");
    // Should pass through as strftime since format isn't recognized
    assert!(pg.contains("strftime"), "Expected strftime passthrough: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_hamming_columns() {
    let pg = reverse(VECTORS, "SELECT vec_distance_hamming(vec, vec) FROM embeddings;");
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_json_group_array_to_json_agg() {
    let pg = reverse(SCHEMA, "SELECT json_group_array(name) FROM users;");
    assert!(pg.contains("json_agg"), "Expected json_agg: {pg}");
    assert!(!pg.contains("json_group_array"), "Should not contain json_group_array: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_json_group_object_to_json_object_agg() {
    let pg = reverse(SCHEMA, "SELECT json_group_object(name, age) FROM users;");
    assert!(pg.contains("json_object_agg"), "Expected json_object_agg: {pg}");
    assert!(!pg.contains("json_group_object"), "Should not contain json_group_object: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_fractional_second() {
    // The forward translator emits strftime('%f', expr) for EXTRACT(SECOND FROM
    // expr). The reverse parser must recognise %f (not just %S) to complete the
    // round-trip.
    let pg = reverse(EVENTS, "SELECT strftime('%f', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(SECOND"), "Expected EXTRACT(SECOND) from %f: {pg}");
    assert!(!pg.contains("strftime"), "Should not contain strftime after round-trip: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_strftime_epoch() {
    // The forward translator emits strftime('%s', expr) for EXTRACT(EPOCH FROM
    // expr).
    let pg = reverse(EVENTS, "SELECT strftime('%s', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(EPOCH"), "Expected EXTRACT(EPOCH) from %s: {pg}");
    assert!(!pg.contains("strftime"), "Should not contain strftime after round-trip: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_f16_to_halfvec_cast() {
    // Forward translates halfvec casts to vec_f16(); the reverse must restore
    // ::halfvec.
    let schema_sql = "CREATE TABLE embeddings (id INT PRIMARY KEY, vec halfvec(3));";
    let pg = reverse(schema_sql, "SELECT vec_f16('[1,2,3]') FROM embeddings;");
    assert!(pg.contains("::halfvec"), "Expected ::halfvec cast: {pg}");
    assert!(!pg.contains("vec_f16"), "Should not contain vec_f16 after round-trip: {pg}");
    assert_parses_as_pg(&pg);
}

fn assert_parses_as_pg(sql: &str) {
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{sql}"));
}
