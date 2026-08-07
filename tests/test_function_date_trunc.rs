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

/// Measured on PostgreSQL 16. These are the units where the calendar rule is
/// not a format string. `week` truncates to the Monday of the ISO week, so a
/// Sunday belongs to the previous one and a Monday stays put. `decade` floors
/// the year, while `century` and `millennium` count from year 1, so 2000 sits
/// in the century starting 1901 and the millennium starting 1001.
#[test]
fn every_coarse_granularity_returns_the_postgres_value() {
    for (stamp, unit, expected) in [
        ("2024-03-15 13:45:30", "week", "2024-03-11 00:00:00"),
        ("2023-01-01 13:45:30", "week", "2022-12-26 00:00:00"),
        ("2024-12-30 13:45:30", "week", "2024-12-30 00:00:00"),
        ("2021-01-01 13:45:30", "week", "2020-12-28 00:00:00"),
        ("2024-03-15 13:45:30", "quarter", "2024-01-01 00:00:00"),
        ("2024-12-30 13:45:30", "quarter", "2024-10-01 00:00:00"),
        ("2000-06-15 13:45:30", "quarter", "2000-04-01 00:00:00"),
        ("2024-03-15 13:45:30", "decade", "2020-01-01 00:00:00"),
        ("1999-06-15 13:45:30", "decade", "1990-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "decade", "2000-01-01 00:00:00"),
        ("2024-03-15 13:45:30", "century", "2001-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "century", "1901-01-01 00:00:00"),
        ("2001-01-01 00:00:00", "century", "2001-01-01 00:00:00"),
        ("2024-03-15 13:45:30", "millennium", "2001-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "millennium", "1001-01-01 00:00:00"),
    ] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '{stamp}');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &Pg2SqliteOptions::default(),
        );
        assert_eq!(rows, vec![Some(expected.to_string())], "date_trunc('{unit}', '{stamp}')");
    }
}

/// The coarse units take the same plural spellings as the rest.
#[test]
fn a_coarse_granularity_accepts_its_plural() {
    for unit in ["weeks", "quarters", "decades", "centuries", "millennia"] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '2024-03-15 13:45:30');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &Pg2SqliteOptions::default(),
        );
        assert_eq!(rows.len(), 1, "date_trunc('{unit}', ts) should return a row");
    }
}

/// The granularity is matched case insensitively, which the coarse units have
/// to honour as well as the rest.
#[test]
fn date_trunc_ignores_the_case_of_the_granularity() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-12-30 13:45:30');
         SELECT date_trunc('QUARTER', ts) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2024-10-01 00:00:00".to_string())]);
}

/// A granularity PostgreSQL does not have is still refused, and the message
/// lists what it accepts. It used to claim the coarse units have no strftime
/// equivalent, which was never true.
#[test]
fn date_trunc_unsupported_granularity_produces_helpful_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               SELECT date_trunc('fortnight', ts) FROM t;";
    let result = translate(sql);
    assert!(result.is_err(), "date_trunc('fortnight', ...) should produce an error");
    let err = result.unwrap_err();
    for granularity in ["second", "week", "quarter", "century", "millennium"] {
        assert!(err.contains(granularity), "error should list {granularity}, got: {err}");
    }
}

/// Flipped R120 pin. `date_trunc(...) OVER (...)` is not PostgreSQL, which
/// accepts OVER only on a window or aggregate function, and the old
/// passthrough emitted `strftime(...) OVER (...)`, which SQLite refuses with
/// `may not be used as a window function`. The translator now refuses.
#[test]
fn date_trunc_with_an_over_clause_is_refused() {
    let options = Pg2SqliteOptions::default();
    let err = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .expect_err("OVER on date_trunc() is not PostgreSQL");
    assert!(
        err.contains("date_trunc") && err.contains("OVER"),
        "the refusal should name the function and OVER: {err}"
    );
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

/// The guard the flipped pin above leaves behind: a real window aggregate in
/// the same shape keeps its OVER clause and the emitted SQL prepares.
#[test]
fn a_window_aggregate_over_partition_still_translates() {
    let options = Pg2SqliteOptions::default();
    let sql =
        translate_sql("SELECT COUNT(created_at) OVER (PARTITION BY user_id) FROM events", &options)
            .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should survive: {sql}");
    assert!(lower.contains("user_id"), "partition column should survive: {sql}");
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INT PRIMARY KEY, user_id INT, created_at TEXT);",
        )
        .unwrap();
        conn.prepare(&sql).expect("COUNT OVER must prepare in SQLite");
    }
}
