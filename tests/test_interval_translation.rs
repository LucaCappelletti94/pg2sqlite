//! Red tests for PG INTERVAL arithmetic to SQLite date modifiers.
//!
//! `ts + INTERVAL 'N unit'` becomes `datetime(ts, '+N unit')`, and minus
//! becomes `'-N unit'`. Multi-unit intervals emit one modifier per unit.
//! Standalone INTERVAL stays unsupported (no SQLite target).

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map_or_else(
            |e| panic!("translation failed: {e}"),
            |stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"),
        )
}

fn try_translate(sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map(|stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
}

// Phase 1: single-unit arithmetic

#[test]
fn p1_now_plus_seven_days() {
    let out = translate("SELECT NOW() + INTERVAL '7 days' AS deadline;");
    assert!(out.contains("datetime(") && out.contains("+7 day"), "{out}");
    execute_translated("SELECT NOW() + INTERVAL '7 days' AS deadline;");
}

#[test]
fn p1_now_minus_one_hour() {
    let out = translate("SELECT NOW() - INTERVAL '1 hour' AS ago;");
    assert!(out.contains("datetime(") && out.contains("-1 hour"), "{out}");
    execute_translated("SELECT NOW() - INTERVAL '1 hour' AS ago;");
}

#[test]
fn p1_current_timestamp_plus_minutes() {
    let out = translate("SELECT CURRENT_TIMESTAMP + INTERVAL '30 minutes' AS later;");
    assert!(out.contains("datetime(") && out.contains("+30 minute"), "{out}");
    execute_translated("SELECT CURRENT_TIMESTAMP + INTERVAL '30 minutes' AS later;");
}

#[test]
fn p1_timestamp_column_plus_seconds() {
    let out = translate(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, ts TIMESTAMP NOT NULL);\n\
         SELECT ts + INTERVAL '5 seconds' AS later FROM events;",
    );
    assert!(out.contains("datetime(") && out.contains("+5 second"), "{out}");
    execute_translated(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, ts TIMESTAMP NOT NULL);\n\
         SELECT ts + INTERVAL '5 seconds' AS later FROM events;",
    );
}

#[test]
fn p1_default_now_plus_interval() {
    let out = translate(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, expires_at TIMESTAMP NOT NULL DEFAULT (NOW() + INTERVAL '14 days'));",
    );
    assert!(out.contains("datetime(") && out.contains("+14 day"), "{out}");
    execute_translated(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, expires_at TIMESTAMP NOT NULL DEFAULT (NOW() + INTERVAL '14 days'));",
    );
}

#[test]
fn p1_apply_roundtrip_seven_days_default() {
    use rusqlite::Connection;
    let script = translate(
        "CREATE TABLE jobs (id INTEGER PRIMARY KEY, deadline TIMESTAMP NOT NULL DEFAULT (NOW() + INTERVAL '7 days'));\n\
         INSERT INTO jobs (id) VALUES (1);",
    );
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&script).expect("apply");
    let days: f64 = conn
        .query_row(
            "SELECT julianday(deadline) - julianday('now') FROM jobs WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!((6.9..7.1).contains(&days), "deadline ~7 days, got {days}");
}

// Phase 2: multi-unit intervals (one modifier per unit)

#[test]
fn p2_year_and_months() {
    let out = translate("SELECT NOW() + INTERVAL '1 year 2 months' AS future;");
    assert!(out.contains("+1 year") && out.contains("+2 month"), "{out}");
    execute_translated("SELECT NOW() + INTERVAL '1 year 2 months' AS future;");
}

#[test]
fn p2_days_and_hours() {
    let out = translate("SELECT NOW() + INTERVAL '3 days 4 hours' AS future;");
    assert!(out.contains("+3 day") && out.contains("+4 hour"), "{out}");
    execute_translated("SELECT NOW() + INTERVAL '3 days 4 hours' AS future;");
}

#[test]
fn p2_minus_propagates_to_each_modifier() {
    let out = translate("SELECT NOW() - INTERVAL '1 day 2 hours' AS past;");
    assert!(out.contains("-1 day") && out.contains("-2 hour"), "{out}");
    execute_translated("SELECT NOW() - INTERVAL '1 day 2 hours' AS past;");
}

// Other parse shapes the helper handles

#[test]
fn leading_field_form_uses_unit_from_field() {
    let out = translate("SELECT NOW() + INTERVAL '7' DAY AS deadline;");
    assert!(out.contains("datetime(") && out.contains("+7 day"), "{out}");
    execute_translated("SELECT NOW() + INTERVAL '7' DAY AS deadline;");
}

#[test]
fn parenthesized_interval_is_unwrapped() {
    let out = translate("SELECT NOW() + (INTERVAL '5 days') AS later;");
    assert!(out.contains("datetime(") && out.contains("+5 day"), "{out}");
    execute_translated("SELECT NOW() + (INTERVAL '5 days') AS later;");
}

#[test]
fn non_literal_interval_value_stays_unsupported() {
    // `INTERVAL (col || 'days')` is a non-literal body; the helper returns
    // None and the standalone-INTERVAL error path takes over.
    let res = try_translate("SELECT NOW() + INTERVAL (col) DAY FROM t;");
    assert!(res.is_err(), "non-literal interval body should fall through, got: {res:?}");
}

// Guards (green now, must stay green)

#[test]
fn standalone_interval_stays_unsupported() {
    assert!(try_translate("SELECT INTERVAL '1 day' AS d;").is_err());
}

#[test]
fn interval_column_stays_text() {
    let out = translate("CREATE TABLE durations (id INTEGER PRIMARY KEY, span INTERVAL NOT NULL);");
    assert!(out.contains("span TEXT"), "{out}");
    execute_translated("CREATE TABLE durations (id INTEGER PRIMARY KEY, span INTERVAL NOT NULL);");
}

/// Translates `pg` and executes every emitted statement against in-memory
/// SQLite. Translated DDL/DQL cannot be expressed via diesel's typed DSL, so
/// sql_query is used here to prove the emitted SQL is accepted by SQLite.
fn execute_translated(pg: &str) {
    use diesel::prelude::*;
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("in-memory connection");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated statement must execute: {e}\n{s}"));
    }
}
