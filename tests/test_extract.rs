//! Tests for EXTRACT(field FROM expr) translation to SQLite strftime.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as RusqliteConnection;

/// SQLite's `strftime('%W', ...)` uses Sunday-based week numbers, while
/// PostgreSQL's `EXTRACT(WEEK)` uses ISO 8601 Monday-based week numbers.
/// The translator must refuse to emit `%W` for WEEK extraction.
#[test]
fn extract_week_does_not_emit_percent_w() {
    let sql = "
        CREATE TABLE week_test (id INTEGER PRIMARY KEY, ts TEXT);
        SELECT EXTRACT(WEEK FROM ts) FROM week_test;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());

    match result {
        Err(e) => {
            // An error is the correct and preferred outcome.
            let msg = e.to_string().to_lowercase();
            assert!(msg.contains("week"), "Error must mention WEEK, got: {e}");
        }
        Ok(stmts) => {
            // If it succeeds it must NOT use %W (Sunday-based).
            let out = stmts
                .iter()
                .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
                .map(|s| s.to_string())
                .unwrap_or_default();
            assert!(
                !out.contains("'%W'"),
                "Must not emit strftime('%W') — that is Sunday-based, not ISO 8601: {out}"
            );
        }
    }
}

/// Verify that the wrong result would have occurred with the old `%W` approach.
/// On 2023-01-01 (Sunday), PG returns week 52 (ISO) but SQLite %W returns 00.
#[test]
fn extract_week_old_percent_w_gives_wrong_result() -> Result<(), Box<dyn std::error::Error>> {
    // This test demonstrates the semantic difference directly via rusqlite,
    // confirming WHY %W is wrong and an error is the right translation.
    let conn = RusqliteConnection::open_in_memory()?;
    let week_w: i64 =
        conn.query_row("SELECT CAST(strftime('%W', '2023-01-01') AS INTEGER)", [], |r| r.get(0))?;
    // strftime('%W', '2023-01-01') = 0 (Sunday-based: week hasn't started yet)
    // PostgreSQL EXTRACT(WEEK FROM '2023-01-01') = 52 (ISO: belongs to week 52 of
    // 2022) They differ — %W is the wrong mapping.
    assert_ne!(
        week_w, 52,
        "strftime('%W') gives {week_w}, not 52 — confirming it is not ISO week numbering"
    );
    Ok(())
}

/// The EXTRACT(SECOND) translation must produce runnable SQLite.
#[test]
fn extract_second_produces_valid_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE sec2_test (id INTEGER PRIMARY KEY, ts TEXT NOT NULL);
               SELECT EXTRACT(SECOND FROM ts) AS secs FROM sec2_test;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_lowercase().contains("strftime"),
        "EXTRACT(SECOND) must use strftime, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO sec2_test VALUES (1, '2024-01-15 10:30:45.123')")
        .execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        secs: Option<f64>,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert!(rows[0].secs.is_some(), "Should have extracted seconds");
    let secs = rows[0].secs.unwrap();
    assert!((secs - 45.123).abs() < 0.001, "Expected ~45.123 seconds, got {secs}");
    Ok(())
}
