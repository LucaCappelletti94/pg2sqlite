//! Semantic fidelity tests for date/time and string functions translated from
//! PostgreSQL to SQLite.
//!
//! Each test executes translated SQL against an in-memory SQLite connection
//! and compares the result to the value documented by PostgreSQL semantics.
//! Where SQLite cannot express a construct faithfully the translation is
//! expected to return a typed translation refusal rather
//! than silently returning a wrong answer.
//!
//! The SQL executed here is the output of the translator, which is not known
//! at compile time. That is why this file uses rusqlite to execute the
//! translated strings directly rather than diesel's typed DSL (which requires
//! a fixed schema at compile time). Every other test file in this suite that
//! exercises translated SQL follows the same pattern.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
// rusqlite is used here because the SQL being executed is dynamically generated
// by the translator at test runtime. diesel's typed DSL requires a schema known
// at compile time and cannot execute an arbitrary translated SQL string.
use rusqlite::Connection;

/// DDL for date/time tests: a TIMESTAMP and a DATE column.
const DT_DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, d DATE);";
/// Seed row used by all date/time tests.
/// 2024-03-05 is a Tuesday (DOW 2, 0=Sunday), day-of-year 65 in a leap year.
const DT_ROW: &str = "INSERT INTO t (id, ts, d) VALUES (1, '2024-03-05 14:07:09', '2024-03-05');";

/// DDL for string tests: a TEXT column and an INT column (used for NULL tests).
const STR_DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT);";

/// Translate a PostgreSQL SQL fragment through `Pg2Sqlite`. Panics on either a
/// parse error or a translation error.
fn translate_sql(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translates")
}

/// Translate a PostgreSQL SQL fragment and return the error string. Handles
/// both parse errors (from `sql()`) and translation errors (from
/// `translate_to_sql()`). Panics when the fragment translates successfully
/// because the caller expected a rejection.
fn translate_err(pg: &str) -> String {
    let parsed = match Pg2Sqlite::default().sql(pg) {
        Err(e) => return e.to_string(),
        Ok(t) => t,
    };
    parsed
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err(&format!("expected a translation error for: {pg}"))
        .to_string()
}

/// Run the translated form of `select_list` against the date/time table.
/// Returns the first column of the single result row as a string.
fn eval_dt(select_list: &str) -> Option<String> {
    let script = translate_sql(&format!("{DT_DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(DT_ROW).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<String>>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{probe}"))
}

/// Like `eval_dt` but returns the first column as a 64-bit integer.
fn eval_dt_i64(select_list: &str) -> Option<i64> {
    let script = translate_sql(&format!("{DT_DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(DT_ROW).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<i64>>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{probe}"))
}

/// Like `eval_dt` but returns the first column as an f64.
fn eval_dt_f64(select_list: &str) -> Option<f64> {
    let script = translate_sql(&format!("{DT_DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(DT_ROW).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<f64>>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{probe}"))
}

/// Run the translated form of `select_list` against the string table, seeded
/// with `row` (a raw SQLite INSERT statement). Returns the first column as a
/// string.
fn eval_str(row: &str, select_list: &str) -> Option<String> {
    let script = translate_sql(&format!("{STR_DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(row).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<String>>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{probe}"))
}

/// Like `eval_str` but returns the first column as a 64-bit integer. Used for
/// functions that return INTEGER in SQLite (e.g. INSTR, length).
fn eval_str_i64(row: &str, select_list: &str) -> Option<i64> {
    let script = translate_sql(&format!("{STR_DDL}\nSELECT {select_list} FROM t WHERE id = 1;"));
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &script[..script.len() - 1] {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn.execute_batch(row).expect("seed row");
    let probe = script.last().expect("a query");
    conn.query_row(probe, [], |r| r.get::<_, Option<i64>>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{probe}"))
}

// ── EXTRACT
// ───────────────────────────────────────────────────────────────────

/// PostgreSQL EXTRACT(YEAR FROM TIMESTAMP '2024-03-05 14:07:09') = 2024.
/// Translated to CAST(strftime('%Y', ts) AS INTEGER).
#[test]
fn extract_year() {
    assert_eq!(eval_dt_i64("EXTRACT(YEAR FROM ts)"), Some(2024));
}

/// PostgreSQL EXTRACT(MONTH FROM TIMESTAMP '2024-03-05 14:07:09') = 3.
#[test]
fn extract_month() {
    assert_eq!(eval_dt_i64("EXTRACT(MONTH FROM ts)"), Some(3));
}

/// PostgreSQL EXTRACT(DAY FROM TIMESTAMP '2024-03-05 14:07:09') = 5.
#[test]
fn extract_day() {
    assert_eq!(eval_dt_i64("EXTRACT(DAY FROM ts)"), Some(5));
}

/// PostgreSQL EXTRACT(HOUR FROM TIMESTAMP '2024-03-05 14:07:09') = 14.
#[test]
fn extract_hour() {
    assert_eq!(eval_dt_i64("EXTRACT(HOUR FROM ts)"), Some(14));
}

/// PostgreSQL EXTRACT(MINUTE FROM TIMESTAMP '2024-03-05 14:07:09') = 7.
#[test]
fn extract_minute() {
    assert_eq!(eval_dt_i64("EXTRACT(MINUTE FROM ts)"), Some(7));
}

/// PostgreSQL EXTRACT(SECOND FROM TIMESTAMP '2024-03-05 14:07:09') = 9.0.
/// Translated to CAST(strftime('%f', ts) AS REAL); '%f' yields '09.000' which
/// casts to 9.0.
#[test]
fn extract_second() {
    assert_eq!(eval_dt_f64("EXTRACT(SECOND FROM ts)"), Some(9.0));
}

/// PostgreSQL DOW is 0=Sunday. 2024-03-05 is a Tuesday, so DOW = 2.
/// SQLite strftime('%w') uses the same convention: 0=Sunday, 2=Tuesday.
#[test]
fn extract_dow() {
    // PostgreSQL EXTRACT(DOW FROM TIMESTAMP '2024-03-05 ...') = 2 (Tuesday).
    assert_eq!(eval_dt_i64("EXTRACT(DOW FROM ts)"), Some(2));
}

/// PostgreSQL EXTRACT(DOY FROM TIMESTAMP '2024-03-05 14:07:09') = 65.
/// Jan=31, Feb=29 (2024 is a leap year), Mar 1-5 = 31+29+5 = 65.
#[test]
fn extract_doy() {
    assert_eq!(eval_dt_i64("EXTRACT(DOY FROM ts)"), Some(65));
}

/// PostgreSQL EXTRACT(EPOCH FROM TIMESTAMP '2024-03-05 14:07:09') =
/// 1709647629.0 when the session treats the timestamp as UTC.
///
/// Derivation (UTC):
/// 2024-01-01 00:00:00 UTC = 1704067200 (54 years, 13 leap years in 1970-2023).
/// 2024 is a leap year: Jan=31 + Feb=29 + Mar 1-4 = 64 days = 5529600 s.
/// Time component: 14*3600 + 7*60 + 9 = 50829 s. Total: 1709647629.
///
/// SQLite strftime('%s', ...) also treats the value as UTC, so both agree.
#[test]
fn extract_epoch() {
    assert_eq!(eval_dt_f64("EXTRACT(EPOCH FROM ts)"), Some(1_709_647_629.0));
}

/// EXTRACT(QUARTER FROM ...) has no faithful strftime equivalent in SQLite.
/// The translation must reject it rather than silently emit a wrong value.
#[test]
fn extract_quarter_is_rejected() {
    let err = translate_err("SELECT EXTRACT(QUARTER FROM ts) FROM t");
    assert!(
        err.contains("QUARTER") || err.contains("not supported") || err.contains("EXTRACT"),
        "expected rejection for QUARTER, got: {err}"
    );
}

// ── date_part
// ─────────────────────────────────────────────────────────────────

/// PostgreSQL date_part always returns double precision. The translated form
/// uses CAST(strftime('%Y', ts) AS INTEGER). The integer 2024 equals 2024.0.
#[test]
fn date_part_year() {
    // PostgreSQL date_part('year', ...) = 2024.0 (double precision).
    assert_eq!(eval_dt_i64("date_part('year', ts)"), Some(2024));
}

/// PostgreSQL date_part('month', ...) = 3.0.
#[test]
fn date_part_month() {
    assert_eq!(eval_dt_i64("date_part('month', ts)"), Some(3));
}

/// PostgreSQL date_part('day', ...) = 5.0.
#[test]
fn date_part_day() {
    assert_eq!(eval_dt_i64("date_part('day', ts)"), Some(5));
}

/// PostgreSQL date_part('hour', ...) = 14.0.
#[test]
fn date_part_hour() {
    assert_eq!(eval_dt_i64("date_part('hour', ts)"), Some(14));
}

/// PostgreSQL date_part('minute', ...) = 7.0.
#[test]
fn date_part_minute() {
    assert_eq!(eval_dt_i64("date_part('minute', ts)"), Some(7));
}

/// PostgreSQL date_part('second', ...) = 9.0 (double precision).
/// strftime('%f', ts) = '09.000'; CAST AS REAL = 9.0.
#[test]
fn date_part_second() {
    assert_eq!(eval_dt_f64("date_part('second', ts)"), Some(9.0));
}

/// PostgreSQL date_part('dow', ...) = 2.0 (Tuesday, 0=Sunday).
#[test]
fn date_part_dow() {
    assert_eq!(eval_dt_i64("date_part('dow', ts)"), Some(2));
}

/// PostgreSQL date_part('doy', ...) = 65.0.
#[test]
fn date_part_doy() {
    assert_eq!(eval_dt_i64("date_part('doy', ts)"), Some(65));
}

/// PostgreSQL date_part('epoch', ...) = 1709647629.0 (UTC seconds since Unix
/// epoch).
#[test]
fn date_part_epoch() {
    assert_eq!(eval_dt_f64("date_part('epoch', ts)"), Some(1_709_647_629.0));
}

// ── date_trunc
// ────────────────────────────────────────────────────────────────

/// PostgreSQL date_trunc('second', '2024-03-05 14:07:09') = '2024-03-05
/// 14:07:09'. No sub-second component to remove; strftime('%Y-%m-%d %H:%M:%S',
/// ts) reproduces the same string.
#[test]
fn date_trunc_second() {
    assert_eq!(eval_dt("date_trunc('second', ts)").as_deref(), Some("2024-03-05 14:07:09"));
}

/// PostgreSQL date_trunc('minute', '2024-03-05 14:07:09') = '2024-03-05
/// 14:07:00'.
#[test]
fn date_trunc_minute() {
    assert_eq!(eval_dt("date_trunc('minute', ts)").as_deref(), Some("2024-03-05 14:07:00"));
}

/// PostgreSQL date_trunc('hour', '2024-03-05 14:07:09') = '2024-03-05
/// 14:00:00'.
#[test]
fn date_trunc_hour() {
    assert_eq!(eval_dt("date_trunc('hour', ts)").as_deref(), Some("2024-03-05 14:00:00"));
}

/// PostgreSQL date_trunc('day', '2024-03-05 14:07:09') = '2024-03-05 00:00:00'.
#[test]
fn date_trunc_day() {
    assert_eq!(eval_dt("date_trunc('day', ts)").as_deref(), Some("2024-03-05 00:00:00"));
}

/// PostgreSQL date_trunc('month', '2024-03-05 14:07:09') = '2024-03-01
/// 00:00:00'.
#[test]
fn date_trunc_month() {
    assert_eq!(eval_dt("date_trunc('month', ts)").as_deref(), Some("2024-03-01 00:00:00"));
}

/// PostgreSQL date_trunc('year', '2024-03-05 14:07:09') = '2024-01-01
/// 00:00:00'.
#[test]
fn date_trunc_year() {
    assert_eq!(eval_dt("date_trunc('year', ts)").as_deref(), Some("2024-01-01 00:00:00"));
}

/// PostgreSQL date_trunc('quarter', '2024-03-05 14:07:09') = '2024-01-01
/// 00:00:00'. The first day of the quarter, not a week number, so this is
/// month arithmetic rather than a format string.
#[test]
fn date_trunc_quarter() {
    assert_eq!(eval_dt("date_trunc('quarter', ts)").as_deref(), Some("2024-01-01 00:00:00"));
}

/// PostgreSQL date_trunc('week', '2024-03-05 14:07:09') = '2024-03-04
/// 00:00:00', the Monday of the ISO week. The old rejection reasoned about
/// `strftime('%W')`, which answers a Sunday based week NUMBER and was never
/// the right shape for a truncation.
#[test]
fn date_trunc_week() {
    assert_eq!(eval_dt("date_trunc('week', ts)").as_deref(), Some("2024-03-04 00:00:00"));
}

/// PostgreSQL date_trunc('decade', '2024-03-05 14:07:09') = '2020-01-01
/// 00:00:00'. A decade floors the year, unlike a century, which counts from
/// year 1.
#[test]
fn date_trunc_decade() {
    assert_eq!(eval_dt("date_trunc('decade', ts)").as_deref(), Some("2020-01-01 00:00:00"));
}

// ── to_char
// ───────────────────────────────────────────────────────────────────

/// PostgreSQL to_char(TIMESTAMP '2024-03-05 14:07:09', 'YYYY-MM-DD') =
/// '2024-03-05'. Mapped to strftime('%Y-%m-%d', ts).
#[test]
fn to_char_date_format() {
    assert_eq!(eval_dt("to_char(ts, 'YYYY-MM-DD')").as_deref(), Some("2024-03-05"));
}

/// PostgreSQL to_char(TIMESTAMP '2024-03-05 14:07:09', 'HH24:MI:SS') =
/// '14:07:09'. HH24 -> %H, MI -> %M, SS -> %S; strftime('%H:%M:%S', ts) =
/// '14:07:09'.
#[test]
fn to_char_time_format() {
    assert_eq!(eval_dt("to_char(ts, 'HH24:MI:SS')").as_deref(), Some("14:07:09"));
}

/// PostgreSQL to_char(TIMESTAMP '2024-03-05 14:07:09', 'YYYY-MM-DD HH24:MI:SS')
/// = '2024-03-05 14:07:09'.
#[test]
fn to_char_datetime_format() {
    assert_eq!(
        eval_dt("to_char(ts, 'YYYY-MM-DD HH24:MI:SS')").as_deref(),
        Some("2024-03-05 14:07:09")
    );
}

/// PostgreSQL supports 'Day' (full weekday name) but strftime has no such code.
/// The translation must reject unsupported format codes rather than emit an
/// empty or incorrect result.
#[test]
fn to_char_unsupported_format_is_rejected() {
    // 'Day' is a valid PostgreSQL to_char format code that strftime cannot express.
    let err = translate_err("SELECT to_char(ts, 'Day') FROM t");
    assert!(
        err.contains("to_char") || err.contains("Day") || err.contains("unsupported"),
        "expected rejection for 'Day' format, got: {err}"
    );
}

// ── misc date/time functions
// ──────────────────────────────────────────────────

/// age(ts) returns an interval; SQLite has no interval type.
/// The translation must reject it with a clear message.
#[test]
fn age_is_rejected() {
    let err = translate_err("SELECT age(ts) FROM t");
    assert!(
        err.contains("age") || err.contains("not supported"),
        "expected rejection for age(), got: {err}"
    );
}

/// now() translates to datetime('now'). The result must not be NULL and must
/// look like a timestamp string (at least 10 characters for YYYY-MM-DD).
#[test]
fn now_returns_nonnull_timestamp() {
    let result = eval_dt("now()");
    assert!(result.is_some(), "now() returned NULL");
    let s = result.unwrap();
    assert!(s.len() >= 10, "now() result too short: {s}");
}

/// CURRENT_DATE is a SQL keyword that both PostgreSQL and SQLite support.
/// The result must not be NULL.
#[test]
fn current_date_is_nonnull() {
    assert!(eval_dt("CURRENT_DATE").is_some(), "CURRENT_DATE returned NULL");
}

/// CURRENT_TIMESTAMP is a SQL keyword that both PostgreSQL and SQLite support.
/// The result must not be NULL.
#[test]
fn current_timestamp_is_nonnull() {
    assert!(eval_dt("CURRENT_TIMESTAMP").is_some(), "CURRENT_TIMESTAMP returned NULL");
}

/// localtimestamp translates to datetime('now', 'localtime') and must not be
/// NULL.
#[test]
fn localtimestamp_is_nonnull() {
    assert!(eval_dt("localtimestamp").is_some(), "localtimestamp returned NULL");
}

/// to_timestamp(0) translates to datetime(0, 'unixepoch') = '1970-01-01
/// 00:00:00'. PostgreSQL to_timestamp(0) = 1970-01-01 00:00:00+00 (Unix epoch).
#[test]
fn to_timestamp_epoch_zero() {
    assert_eq!(eval_dt("to_timestamp(0)").as_deref(), Some("1970-01-01 00:00:00"));
}

/// ts + INTERVAL '1 day' translates to datetime(ts, '+1 day').
/// PostgreSQL: TIMESTAMP '2024-03-05 14:07:09' + INTERVAL '1 day'
///           = TIMESTAMP '2024-03-06 14:07:09'.
#[test]
fn interval_add_one_day() {
    assert_eq!(eval_dt("ts + INTERVAL '1 day'").as_deref(), Some("2024-03-06 14:07:09"));
}

// ── String functions
// ──────────────────────────────────────────────────────────

const ROW_HELLO_WORLD: &str = "INSERT INTO t (id, s) VALUES (1, 'Hello World');";

/// PostgreSQL position('World' IN 'Hello World') = 7.
/// Translated to INSTR(s, 'World') with argument order swapped.
#[test]
fn position_finds_substring() {
    // PostgreSQL position() is 1-based and returns 0 when not found.
    // INSTR returns INTEGER in SQLite, so compare as i64.
    assert_eq!(eval_str_i64(ROW_HELLO_WORLD, "position('World' IN s)"), Some(7));
}

/// PostgreSQL position(x IN y) = 0 when the substring is absent.
#[test]
fn position_returns_zero_when_absent() {
    assert_eq!(eval_str_i64(ROW_HELLO_WORLD, "position('xyz' IN s)"), Some(0));
}

/// strpos is renamed to INSTR; both are 1-based and return 0 on no match.
/// PostgreSQL strpos('Hello World', 'World') = 7.
#[test]
fn strpos_finds_substring() {
    assert_eq!(eval_str_i64(ROW_HELLO_WORLD, "strpos(s, 'World')"), Some(7));
}

/// SQLite replace() and PostgreSQL replace() are identical.
/// PostgreSQL replace('Hello World', 'World', 'SQLite') = 'Hello SQLite'.
#[test]
fn replace_all_occurrences() {
    assert_eq!(
        eval_str(ROW_HELLO_WORLD, "replace(s, 'World', 'SQLite')").as_deref(),
        Some("Hello SQLite")
    );
}

/// lpad is not available in standard SQLite; the translation rejects it.
#[test]
fn lpad_is_rejected() {
    let err = translate_err("SELECT lpad(s, 10, ' ') FROM t");
    assert!(
        err.contains("lpad") || err.contains("not available"),
        "expected rejection for lpad, got: {err}"
    );
}

/// rpad is not available in standard SQLite; the translation rejects it.
#[test]
fn rpad_is_rejected() {
    let err = translate_err("SELECT rpad(s, 10, ' ') FROM t");
    assert!(
        err.contains("rpad") || err.contains("not available"),
        "expected rejection for rpad, got: {err}"
    );
}

/// split_part is not available in standard SQLite; the translation rejects it.
#[test]
fn split_part_is_rejected() {
    let err = translate_err("SELECT split_part(s, ' ', 1) FROM t");
    assert!(
        err.contains("split_part") || err.contains("not supported"),
        "expected rejection for split_part, got: {err}"
    );
}

/// initcap is not available in standard SQLite; the translation rejects it.
#[test]
fn initcap_is_rejected() {
    let err = translate_err("SELECT initcap(s) FROM t");
    assert!(
        err.contains("initcap") || err.contains("not available"),
        "expected rejection for initcap, got: {err}"
    );
}

/// reverse() is not available in standard SQLite. The translation must reject
/// it so callers see a clear error rather than a runtime crash from an unknown
/// function.
#[test]
fn reverse_is_rejected() {
    let err = translate_err("SELECT reverse(s) FROM t");
    assert!(
        err.contains("reverse") || err.contains("not available"),
        "expected rejection for reverse(), got: {err}"
    );
}

/// repeat is not available in standard SQLite; the translation rejects it.
#[test]
fn repeat_is_rejected() {
    let err = translate_err("SELECT repeat(s, 3) FROM t");
    assert!(
        err.contains("repeat") || err.contains("not available"),
        "expected rejection for repeat, got: {err}"
    );
}

/// md5 is not available in standard SQLite; the translation rejects it.
#[test]
fn md5_is_rejected() {
    let err = translate_err("SELECT md5(s) FROM t");
    assert!(
        err.contains("md5") || err.contains("not available"),
        "expected rejection for md5, got: {err}"
    );
}

/// translate() (character-level replacement) is not available in standard
/// SQLite; the translation rejects it.
#[test]
fn translate_fn_is_rejected() {
    let err = translate_err("SELECT translate(s, 'l', 'r') FROM t");
    assert!(
        err.contains("translate") || err.contains("not available"),
        "expected rejection for translate(), got: {err}"
    );
}

/// btrim maps to SQLite trim(); both remove leading and trailing whitespace.
/// PostgreSQL btrim('  hello  ') = 'hello'.
#[test]
fn btrim_strips_whitespace() {
    let row = "INSERT INTO t (id, s) VALUES (1, '  hello  ');";
    assert_eq!(eval_str(row, "btrim(s)").as_deref(), Some("hello"));
}

/// btrim with a character set removes those chars from both ends.
/// PostgreSQL btrim('xxxhelloxxx', 'x') = 'hello'.
/// SQLite trim(s, 'x') is identical.
#[test]
fn btrim_with_char_set() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'xxxhelloxxx');";
    assert_eq!(eval_str(row, "btrim(s, 'x')").as_deref(), Some("hello"));
}

/// ltrim removes leading whitespace. PostgreSQL ltrim('  hello  ') = 'hello  '.
/// SQLite's ltrim() agrees.
#[test]
fn ltrim_strips_leading_whitespace() {
    let row = "INSERT INTO t (id, s) VALUES (1, '  hello  ');";
    assert_eq!(eval_str(row, "ltrim(s)").as_deref(), Some("hello  "));
}

/// ltrim with a character set removes those chars from the left.
/// PostgreSQL ltrim('xxhelloxx', 'x') = 'helloxx'. SQLite ltrim(X, Y) agrees.
#[test]
fn ltrim_with_char_set() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'xxhelloxx');";
    assert_eq!(eval_str(row, "ltrim(s, 'x')").as_deref(), Some("helloxx"));
}

/// rtrim removes trailing whitespace. PostgreSQL rtrim('  hello  ') = '
/// hello'. SQLite's rtrim() agrees.
#[test]
fn rtrim_strips_trailing_whitespace() {
    let row = "INSERT INTO t (id, s) VALUES (1, '  hello  ');";
    assert_eq!(eval_str(row, "rtrim(s)").as_deref(), Some("  hello"));
}

/// rtrim with a character set removes those chars from the right.
/// PostgreSQL rtrim('xxhelloxx', 'x') = 'xxhello'. SQLite rtrim(X, Y) agrees.
#[test]
fn rtrim_with_char_set() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'xxhelloxx');";
    assert_eq!(eval_str(row, "rtrim(s, 'x')").as_deref(), Some("xxhello"));
}

/// PostgreSQL concat() skips NULL arguments; '||' propagates NULL.
/// PostgreSQL concat('Hello', NULL, ' World') = 'Hello World'.
/// Translated to COALESCE(s, '') || COALESCE(n, '') || COALESCE(' World', '').
#[test]
fn concat_skips_null_args() {
    // n is a NULL integer; concat() must skip it and produce 'Hello World'.
    let row = "INSERT INTO t (id, s, n) VALUES (1, 'Hello', NULL);";
    assert_eq!(eval_str(row, "concat(s, n, ' World')").as_deref(), Some("Hello World"));
}

/// The '||' operator propagates NULL; it differs from concat().
/// PostgreSQL: 'Hello' || NULL::text || ' World' = NULL.
#[test]
fn concat_operator_propagates_null() {
    // CAST(n AS TEXT) = NULL; the whole expression is NULL.
    let row = "INSERT INTO t (id, s, n) VALUES (1, 'Hello', NULL);";
    assert_eq!(eval_str(row, "s || CAST(n AS TEXT) || ' World'"), None);
}

/// PostgreSQL concat_ws(', ', 'a', NULL, 'b') = 'a, b'.
/// NULL value arguments are skipped and the separator is only inserted between
/// non-NULL values.
#[test]
fn concat_ws_skips_null_value_args() {
    // n is NULL; concat_ws skips it, inserting the separator only around 'b'.
    let row = "INSERT INTO t (id, s, n) VALUES (1, 'a', NULL);";
    assert_eq!(eval_str(row, "concat_ws(', ', s, n, 'b')").as_deref(), Some("a, b"));
}

/// upper() on ASCII input is faithful.
/// PostgreSQL upper('hello') = 'HELLO'. SQLite's upper() agrees for ASCII.
#[test]
fn upper_ascii_input() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'hello');";
    assert_eq!(eval_str(row, "upper(s)").as_deref(), Some("HELLO"));
}

/// lower() on ASCII input is faithful.
/// PostgreSQL lower('HELLO') = 'hello'. SQLite's lower() agrees for ASCII.
#[test]
fn lower_ascii_input() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'HELLO');";
    assert_eq!(eval_str(row, "lower(s)").as_deref(), Some("hello"));
}

/// DIVERGENCE: SQLite's upper() only case-folds ASCII letters (U+0041-U+005A).
///
/// PostgreSQL upper('straße') with an ICU-enabled locale returns 'STRASSE'
/// because the German sharp-s (U+00DF) maps to two characters 'SS'. Without ICU
/// (the default C locale), PostgreSQL also returns 'STRAßE' because 'ß' has no
/// single-character uppercase mapping. SQLite's built-in upper() always returns
/// 'STRAßE' because 'ß' is not in the ASCII range.
///
/// This test pins the SQLite output. Callers that need full Unicode
/// case-folding must load an ICU extension rather than relying on the built-in
/// upper().
#[test]
fn upper_non_ascii_sqlite_behavior() {
    let row = "INSERT INTO t (id, s) VALUES (1, 'straße');";
    // SQLite folds only the ASCII prefix 's','t','r','a' -> 'S','T','R','A'.
    // 'ß' (U+00DF) is left unchanged because it is outside the ASCII range.
    // This matches PostgreSQL without ICU. Callers needing 'STRASSE' must use ICU.
    assert_eq!(eval_str(row, "upper(s)").as_deref(), Some("STRAßE"));
}
