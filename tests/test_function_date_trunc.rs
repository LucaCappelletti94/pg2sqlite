//! Tests for `date_trunc` translation to SQLite `strftime`.

mod helpers;
#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use diesel::{Connection, RunQueryDsl};
use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

/// Measured on PostgreSQL 16 over `2024-03-15 10:20:30.456`. Every unit
/// returns a full timestamp, which is the point of the item: the three coarse
/// ones used to return a bare date.
#[test]
fn every_granularity_returns_the_postgres_value() {
    for (unit, expected) in [
        ("second", "2024-03-15 10:20:30"),
        ("minute", "2024-03-15 10:20:00"),
        ("hour", "2024-03-15 10:00:00"),
        ("day", "2024-03-15 00:00:00"),
        ("month", "2024-03-01 00:00:00"),
        ("year", "2024-01-01 00:00:00"),
    ] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '2024-03-15 10:20:30');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &Pg2SqliteOptions::default(),
        );
        assert_eq!(rows, vec![Some(expected.to_string())], "date_trunc('{unit}', ts)");
    }
}

/// The consequence the item names: this crate stores a timestamp as TEXT
/// `YYYY-MM-DD HH:MM:SS`, so a truncated value that drops the time never
/// matches one, and a comparison or a join on it silently returns nothing.
#[test]
fn a_truncated_value_compares_against_a_stored_timestamp() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-03-15 10:20:30');
         SELECT count(*) FROM t WHERE date_trunc('month', ts) = '2024-03-01 00:00:00';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
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
    assert_eq!(results[0].truncated, "2024-03-15 00:00:00");

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
