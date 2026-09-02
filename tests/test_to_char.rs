//! Tests for `to_char` translation to SQLite `strftime`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);";

// Happy path

#[test]
fn to_char_yyyy_mm_dd() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YYYY-MM-DD') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d'"),
        "to_char('YYYY-MM-DD') should produce strftime('%Y-%m-%d', ...), got: {output}"
    );
    assert!(!output.contains("to_char"), "Output should not contain to_char: {output}");
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_hh24_mi_ss() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'HH24:MI:SS') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%H:%M:%S'"),
        "to_char('HH24:MI:SS') should produce strftime('%H:%M:%S', ...), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_full_datetime() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YYYY-MM-DD HH24:MI:SS') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d %H:%M:%S'"),
        "to_char full datetime should produce strftime('%Y-%m-%d %H:%M:%S', ...), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_yyyy_only() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YYYY') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%Y'"),
        "to_char('YYYY') should produce strftime('%Y', ...), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

/// Flipped F33 pin. This asserted `strftime('%y', ...)`, which parses and then
/// answers NULL, because SQLite has no `%y` at all.
#[test]
fn to_char_yy_only() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YY') FROM t;");
    let error = translate(&sql).expect_err("the two-digit year has no SQLite specifier");
    assert!(error.contains("YY"), "{error}");
}

#[test]
fn to_char_hh12_mi() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'HH12:MI') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%I:%M'"),
        "to_char('HH12:MI') should produce strftime('%I:%M', ...), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_hh_mi_alias_for_hh12() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'HH:MI') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%I:%M'"),
        "to_char('HH:MI') should produce strftime('%I:%M', ...) (HH=HH12), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_args_swapped_format_before_column() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YYYY-MM-DD') FROM t;");
    let output = translate(&sql).unwrap();
    // strftime(format, expr) — format string must appear first
    let strftime_pos = output.find("strftime(").unwrap();
    let format_pos = output[strftime_pos..].find("'%Y-%m-%d'").unwrap();
    let col_pos = output[strftime_pos..].find("ts").unwrap();
    assert!(
        format_pos < col_pos,
        "Format string should appear before column in strftime output: {output}"
    );
    assert_all_stmts_parse_as_sqlite(&sql);
}

#[test]
fn to_char_now_also_translated() {
    // NOW() inside to_char should also be translated to datetime('now')
    let sql = "SELECT to_char(NOW(), 'YYYY-MM-DD');";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d'"),
        "to_char(NOW(), ...) should produce strftime, got: {output}"
    );
    assert!(
        output.contains("datetime('now')"),
        "NOW() inside to_char should become datetime('now'), got: {output}"
    );
    assert_all_stmts_parse_as_sqlite(sql);
}

// Error cases

#[test]
fn to_char_number_format_causes_error() {
    let sql = format!("{TABLE} SELECT to_char(id, '999') FROM t;");
    let result = translate(&sql);
    assert!(result.is_err(), "Number format '999' should produce an error");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("unsupported") || err.contains("printf"),
        "Error should mention unsupported or printf, got: {err}"
    );
}

#[test]
fn to_char_fm_prefix_causes_error() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'FMMonth') FROM t;");
    let result = translate(&sql);
    assert!(result.is_err(), "FM prefix format 'FMMonth' should produce an error");
    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Should have a non-empty error message: {err}");
}

#[test]
fn to_char_dynamic_format_causes_error() {
    // Format is a column reference, not a string literal
    let sql = "CREATE TABLE t (id INT, ts TIMESTAMP, fmt TEXT);
               SELECT to_char(ts, fmt) FROM t;";
    let result = translate(sql);
    assert!(result.is_err(), "Dynamic format should produce an error");
    let err = result.unwrap_err();
    assert!(err.to_lowercase().contains("literal"), "Error should mention 'literal', got: {err}");
}

// to_char refuses a window OVER clause, R120

/// Flipped R120 pin. `to_char(...) OVER (...)` is not PostgreSQL, which
/// accepts OVER only on a window or aggregate function, and the old
/// passthrough emitted `strftime(...) OVER (...)`, which SQLite refuses with
/// `may not be used as a window function`. The translator now refuses.
#[test]
fn to_char_with_an_over_clause_is_refused() {
    let sql = "CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP, user_id INT); \
               SELECT to_char(ts, 'YYYY-MM-DD') OVER (PARTITION BY user_id) FROM events;";
    let err = translate(sql).expect_err("OVER on to_char() is not PostgreSQL");
    assert!(
        err.contains("to_char") && err.contains("OVER"),
        "the refusal should name the function and OVER: {err}"
    );
}

#[test]
fn to_char_wrong_arg_count_causes_error() {
    let sql = "SELECT to_char(NOW());";
    let result = translate(sql);
    assert!(result.is_err(), "to_char with 1 argument should produce an error");
    let err = result.unwrap_err();
    assert!(
        err.contains('2') || err.to_lowercase().contains("argument"),
        "Error should mention argument count, got: {err}"
    );
}

#[test]
fn to_char_tz_code_causes_error() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'YYYY-MM-DD TZ') FROM t;");
    let result = translate(&sql);
    assert!(result.is_err(), "TZ timezone code should produce an error");
    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Should have a non-empty error message: {err}");
}

fn assert_all_stmts_parse_as_sqlite(pg_sql: &str) {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(pg_sql)
        .expect("parse")
        .translate(&pg2sqlite::prelude::Pg2SqliteOptions::default())
        .expect("translate")
    {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("translated statement must run in SQLite: {e}\n{stmt}"));
    }
}

/// PostgreSQL's ISO codes map exactly onto strftime specifiers: IYYY is %G,
/// IW is %V, ID is %u. Measured on PostgreSQL 18 and SQLite 3.51 at the year
/// boundary: `to_char(date '2024-12-30', 'IYYY-IW-ID')` and
/// `strftime('%G-%V-%u', '2024-12-30')` both answer `2025-01-1`, zero padding
/// included.
#[test]
fn to_char_iso_year_week_day() {
    let sql = format!("{TABLE} SELECT to_char(ts, 'IYYY-IW-ID') FROM t;");
    let output = translate(&sql).unwrap();
    assert!(
        output.contains("strftime('%G-%V-%u'"),
        "to_char('IYYY-IW-ID') should produce strftime('%G-%V-%u', ...), got: {output}"
    );

    // Execute the boundary value: 2024-12-30 is calendar 2024 but ISO
    // 2025-W01-1.
    let literal = translate("SELECT to_char(TIMESTAMP '2024-12-30 00:00:00', 'IYYY-IW-ID');")
        .expect("the ISO codes have exact strftime equivalents");
    // The SQL under test is translator output, a runtime string, so the raw
    // query interface is the correct one.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let value: String = conn.query_row(&literal, [], |row| row.get(0)).unwrap();
    assert_eq!(value, "2025-01-1", "PostgreSQL answers 2025-01-1: {literal}");
}
