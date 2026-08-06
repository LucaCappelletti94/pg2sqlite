//! Tests for DISTINCT ON rewrite to ROW_NUMBER window function.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

fn translate(sql: &str) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    Ok(Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?)
}

fn query_sql(translated: &[Statement]) -> String {
    translated
        .iter()
        .find(|stmt| matches!(stmt, Statement::Query(_)))
        .expect("expected translated SELECT query")
        .to_string()
}

fn execute_ddl(
    translated: &[Statement],
    conn: &mut SqliteConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    for stmt in translated.iter().filter(|stmt| !matches!(stmt, Statement::Query(_))) {
        diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(())
}

#[derive(Debug, QueryableByName)]
struct DistinctOnRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    user_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    ts: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    payload: String,
}

#[test]
fn distinct_on_rewrites_to_window_filter() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL,
            payload TEXT NOT NULL
        );
        SELECT DISTINCT ON (user_id) user_id, ts, payload
        FROM events
        ORDER BY user_id, ts DESC;
    ";

    let translated = translate(sql)?;
    let query = query_sql(&translated);
    let upper = query.to_uppercase();

    assert!(!upper.contains("DISTINCT ON"), "DISTINCT ON should be rewritten: {query}");
    assert!(upper.contains("ROW_NUMBER"), "Expected ROW_NUMBER rewrite: {query}");

    Ok(())
}

#[test]
fn distinct_on_semantic_highest_per_partition() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL,
            payload TEXT NOT NULL
        );
        SELECT DISTINCT ON (user_id) user_id, ts, payload
        FROM events
        ORDER BY user_id, ts DESC;
    ";

    let translated = translate(sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;

    diesel::sql_query(
        "INSERT INTO events (id, user_id, ts, payload) VALUES
         (1, 1, 10, 'u1-old'),
         (2, 1, 20, 'u1-new'),
         (3, 2, 15, 'u2-only');",
    )
    .execute(&mut conn)?;

    let rows = diesel::sql_query(query).load::<DistinctOnRow>(&mut conn)?;
    assert_eq!(rows.len(), 2, "Expected one row per user_id");

    let first = rows.iter().find(|r| r.user_id == 1).expect("missing user 1");
    assert_eq!(first.ts, 20);
    assert_eq!(first.payload, "u1-new");

    let second = rows.iter().find(|r| r.user_id == 2).expect("missing user 2");
    assert_eq!(second.ts, 15);
    assert_eq!(second.payload, "u2-only");

    Ok(())
}

#[test]
fn distinct_on_wildcard_stays_unsupported() {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL
        );
        SELECT DISTINCT ON (user_id) * FROM events ORDER BY user_id, ts DESC;
    ";

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Wildcard DISTINCT ON should remain unsupported");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("DISTINCT ON") && err.contains("projection"),
        "Expected explicit DISTINCT ON projection error, got: {err}"
    );
}

/// One reading per sensor, so the rewrite has something to discriminate.
const READINGS: &str = "
    CREATE TABLE readings (sensor TEXT NOT NULL, value INTEGER NOT NULL, ts TIMESTAMP NOT NULL);
    INSERT INTO readings VALUES
        ('a', 100, '2024-01-01'), ('a', 300, '2024-01-03'),
        ('b', 200, '2024-01-02'), ('b',  50, '2024-01-01');
";

#[derive(Debug, QueryableByName, PartialEq, Eq)]
struct Latest {
    #[diesel(sql_type = diesel::sql_types::Text)]
    sensor: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    reading: i32,
}

/// Translates `query` against `READINGS`, applies everything but the query, and
/// returns its rows. The emitted SQL is the artifact under test, so it is
/// applied as generated text.
fn latest_per_sensor(query: &str) -> Result<Vec<Latest>, Box<dyn std::error::Error>> {
    let translated = translate(&format!("{READINGS} {query};"))?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;
    Ok(diesel::sql_query(query_sql(&translated)).load(&mut conn)?)
}

fn expected() -> Vec<Latest> {
    vec![
        Latest { sensor: "a".to_owned(), reading: 300 },
        Latest { sensor: "b".to_owned(), reading: 200 },
    ]
}

/// The rewrite projects only the output columns, so an `ORDER BY` naming
/// anything else cannot resolve on the outside and the emitted SQL will not
/// even prepare. SQLite answered `no such column: ts`.
///
/// The window itself was always fine: it sits inside the derived table where
/// the real table's columns are in scope. Only the outer reference is broken,
/// so the operand has to be carried into the derived table.
#[test]
fn order_by_an_unprojected_column_still_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        latest_per_sensor(
            "SELECT DISTINCT ON (sensor) sensor, value AS reading FROM readings \
             ORDER BY sensor, ts DESC"
        )?,
        expected()
    );
    Ok(())
}

/// The same shape where the operand IS projected but under its output name, so
/// the inner name is dead on the outside. SQLite answered `no such column:
/// value`.
#[test]
fn order_by_a_renamed_column_still_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        latest_per_sensor(
            "SELECT DISTINCT ON (sensor) sensor, value AS reading FROM readings \
             ORDER BY sensor, value DESC"
        )?,
        expected()
    );
    Ok(())
}

/// The reverse trap. PostgreSQL resolves an outer `ORDER BY` against output
/// names, so ordering by the alias is legal there. Copying that alias into the
/// window is not: a column alias is invisible inside `OVER`, and SQLite was
/// right to answer `no such column: reading`. The window has to be built from
/// the underlying expression instead.
#[test]
fn order_by_an_output_alias_still_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        latest_per_sensor(
            "SELECT DISTINCT ON (sensor) sensor, value AS reading FROM readings \
             ORDER BY sensor, reading DESC"
        )?,
        expected()
    );
    Ok(())
}

/// The shape that already worked, kept as a guard: an `ORDER BY` naming a bare
/// projected column that keeps its name. This passing is what hid the other
/// three, since it is what anyone writes in a quick test.
#[test]
fn order_by_a_plain_projected_column_keeps_working() -> Result<(), Box<dyn std::error::Error>> {
    let rows = latest_per_sensor(
        "SELECT DISTINCT ON (sensor) sensor, value AS reading FROM readings ORDER BY sensor",
    )?;
    assert_eq!(rows.len(), 2, "one row per sensor: {rows:?}");
    Ok(())
}
