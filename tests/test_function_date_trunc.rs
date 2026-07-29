//! Tests for `date_trunc` translation to SQLite `strftime`.

mod helpers;

use diesel::{Connection, RunQueryDsl};
use helpers::translate_sql;
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

#[test]
fn date_trunc_preserves_window_over() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("strftime"), "expected strftime: {sql}");
    assert!(lower.contains("over"), "expected OVER clause preserved: {sql}");
}

#[test]
fn date_trunc_without_over_semantic() -> Result<(), Box<dyn std::error::Error>> {
    // Test date_trunc semantics work correctly (without OVER, since strftime
    // is not a valid SQLite window function).
    let pg_sql = "
        CREATE TABLE events (id SERIAL PRIMARY KEY, user_id INTEGER NOT NULL, created_at TEXT NOT NULL);
        SELECT date_trunc('day', created_at) AS truncated FROM events;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::sql_query(
        "INSERT INTO events (id, user_id, created_at) VALUES (1, 1, '2024-03-15 10:30:00'), (2, 1, '2024-03-15 14:00:00')",
    )
    .execute(&mut conn)?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    #[derive(diesel::QueryableByName, Debug)]
    struct TruncResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        truncated: String,
    }

    let results = diesel::sql_query(&select_sql).load::<TruncResult>(&mut conn)?;
    assert_eq!(results.len(), 2);
    // Both rows for the same day should produce the same truncated date
    assert_eq!(results[0].truncated, results[1].truncated);
    // Day truncation should produce YYYY-MM-DD 00:00:00
    assert!(
        results[0].truncated.contains("2024-03-15"),
        "expected day-truncated date, got: {}",
        results[0].truncated
    );

    Ok(())
}

#[test]
fn date_trunc_over_partition_translation_preserves_structure() {
    // strftime is not a valid SQLite window function, but the translation
    // should preserve the OVER clause structure for functions that are.
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should be preserved: {sql}");
    assert!(lower.contains("user_id"), "partition column should be preserved: {sql}");
}
