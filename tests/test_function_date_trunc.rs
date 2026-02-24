//! Tests for `date_trunc` translation to SQLite `strftime`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn date_trunc_day_translates_to_strftime() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('day', ts) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d'"),
        "date_trunc('day', ...) should use strftime('%Y-%m-%d', ...), got: {output}"
    );
}

#[test]
fn date_trunc_month_uses_first_day() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('month', ts) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-01'"),
        "date_trunc('month', ...) should use strftime('%Y-%m-01', ...), got: {output}"
    );
}

#[test]
fn date_trunc_year_uses_first_month_day() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('year', ts) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-01-01'"),
        "date_trunc('year', ...) should use strftime('%Y-01-01', ...), got: {output}"
    );
}

#[test]
fn date_trunc_hour_zeros_minutes_seconds() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('hour', ts) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d %H:00:00'"),
        "date_trunc('hour', ...) should use strftime('%Y-%m-%d %H:00:00', ...), got: {output}"
    );
}

#[test]
fn date_trunc_minute_zeros_seconds() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('minute', ts) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("strftime('%Y-%m-%d %H:%M:00'"),
        "date_trunc('minute', ...) should use strftime('%Y-%m-%d %H:%M:00', ...), got: {output}"
    );
}

#[test]
fn date_trunc_quarter_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('quarter', ts) FROM t;";
    let result = translate(sql);
    assert!(result.is_err(), "date_trunc('quarter', ...) should produce an error");
    let err = result.unwrap_err();
    assert!(err.contains("quarter"), "Error should mention 'quarter', got: {err}");
}

#[test]
fn date_trunc_unsupported_granularity_produces_helpful_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('century', ts) FROM t;";
    let result = translate(sql);
    assert!(result.is_err(), "date_trunc('century', ...) should produce an error");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("not supported"),
        "Error should mention 'not supported', got: {err}"
    );
}
