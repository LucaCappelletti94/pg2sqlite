//! Tests for EXTRACT(field FROM expr) translation to SQLite strftime.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// `EXTRACT(WEEK)` is the ISO week, which SQLite spells `%V`. `%W` is the
/// Sunday based one and disagrees at every year boundary, so emitting it would
/// be a silently wrong answer rather than a failure.
///
/// The values are asserted over the boundary dates in
/// `tests/test_iso_week.rs`. This guards the format alone, since a return to
/// `%W` would still translate and still run.
#[test]
fn extract_week_uses_the_iso_format() {
    let sql = "
        CREATE TABLE week_test (id INTEGER PRIMARY KEY, ts TEXT);
        SELECT EXTRACT(WEEK FROM ts) FROM week_test;
    ";
    let statements =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let query = statements
        .iter()
        .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .map(ToString::to_string)
        .expect("a query");

    assert!(query.contains("'%V'"), "expected the ISO week format: {query}");
    assert!(!query.contains("'%W'"), "%W is Sunday based, not ISO: {query}");
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
