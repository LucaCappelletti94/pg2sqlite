//! Tests for recursive translation of sub-expressions inside window specs.
//!
//! Categories covered:
//! - Inline window spec OVER (PARTITION BY / ORDER BY) expressions
//! - WHERE clause expressions in subqueries inside aggregates
//! - CTE (WITH clause) body expressions

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

const SCHEMA: &str = "CREATE TABLE events (id INT PRIMARY KEY, val FLOAT, created_at TIMESTAMP);";

fn translate_ok(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find(|s| matches!(s, Statement::Query(_)))
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Inline OVER (PARTITION BY <pg-expr>) — partition_by expressions must be
// translated, not left as PG syntax.
// ---------------------------------------------------------------------------

/// date_trunc inside PARTITION BY of an inline window spec must be translated
/// to strftime (the SQLite equivalent) — not left as `date_trunc(...)`.
#[test]
fn window_partition_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (PARTITION BY date_trunc('month', created_at)) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("strftime"),
        "date_trunc in PARTITION BY must be translated to strftime: {out}"
    );
    assert!(
        !out.to_lowercase().contains("date_trunc"),
        "date_trunc must not appear verbatim in output: {out}"
    );
}

/// NOW() inside ORDER BY of an inline window spec must become datetime('now').
#[test]
fn window_order_by_now_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT ROW_NUMBER() OVER (ORDER BY NOW()) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("datetime"),
        "NOW() in window ORDER BY must become datetime('now'): {out}"
    );
    assert!(
        !out.to_uppercase().contains("NOW()"),
        "NOW() must not appear verbatim in window ORDER BY output: {out}"
    );
}

/// date_trunc inside ORDER BY of an inline window spec must be translated.
#[test]
fn window_order_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT RANK() OVER (ORDER BY date_trunc('day', created_at)) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("strftime"),
        "date_trunc in window ORDER BY must be translated to strftime: {out}"
    );
    assert!(
        !out.to_lowercase().contains("date_trunc"),
        "date_trunc must not appear in output: {out}"
    );
}

/// date_trunc in both PARTITION BY and ORDER BY of the same window spec.
#[test]
fn window_partition_and_order_by_date_trunc_both_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (
             PARTITION BY date_trunc('month', created_at)
             ORDER BY date_trunc('day', created_at)
         ) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("strftime"),
        "date_trunc in both PARTITION BY and ORDER BY must translate to strftime: {out}"
    );
    assert!(!out.to_lowercase().contains("date_trunc"), "No date_trunc should remain: {out}");
}

// ---------------------------------------------------------------------------
// Named WINDOW clause — already covered by translate_named_windows, but
// confirm the same PG expressions inside named windows are translated.
// ---------------------------------------------------------------------------

/// date_trunc in a named WINDOW clause's PARTITION BY must be translated.
#[test]
fn named_window_partition_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER w FROM events
         WINDOW w AS (PARTITION BY date_trunc('month', created_at));"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("strftime"),
        "date_trunc in named WINDOW must be translated to strftime: {out}"
    );
    assert!(
        !out.to_lowercase().contains("date_trunc"),
        "date_trunc must not remain in named WINDOW: {out}"
    );
}

// ---------------------------------------------------------------------------
// Aggregate function args that contain PG-specific expressions — fixed in
// the previous round but confirming with a window-function variant.
// ---------------------------------------------------------------------------

/// date_trunc as the expression argument to a window aggregate must translate.
#[test]
fn window_aggregate_arg_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT FIRST_VALUE(date_trunc('hour', created_at)) OVER (ORDER BY id) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.to_lowercase().contains("strftime"),
        "date_trunc in window aggregate arg must become strftime: {out}"
    );
    assert!(
        !out.to_lowercase().contains("date_trunc"),
        "date_trunc must not remain in window aggregate arg: {out}"
    );
}
