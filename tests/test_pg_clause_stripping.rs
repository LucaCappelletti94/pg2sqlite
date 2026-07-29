//! Tests for PostgreSQL clause stripping (FOR UPDATE/SHARE, NULLS FIRST/LAST).

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// `FOR UPDATE` is a PostgreSQL row-locking hint that SQLite does not support.
/// The translator must strip it rather than emitting invalid SQLite SQL.
#[test]
fn for_update_stripped_from_select() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE lock_test (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        SELECT * FROM lock_test FOR UPDATE;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    assert!(
        !select_sql.to_uppercase().contains("FOR UPDATE"),
        "FOR UPDATE must be stripped from translated output, got: {select_sql}"
    );

    // Confirm the translated DDL + DML actually run in SQLite without error.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// `FOR SHARE` is also a PostgreSQL row-locking hint; same treatment required.
#[test]
fn for_share_stripped_from_select() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE lock_test2 (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        SELECT * FROM lock_test2 FOR SHARE;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    assert!(
        !select_sql.to_uppercase().contains("FOR SHARE"),
        "FOR SHARE must be stripped from translated output, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// `NULLS FIRST` is not valid syntax in SQLite < 3.30 and should be stripped.
/// The translated query must still execute and return rows in the correct
/// order.
#[test]
fn nulls_first_stripped_from_order_by() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE nulls_test (id INTEGER PRIMARY KEY, val INTEGER);
        SELECT val FROM nulls_test ORDER BY val ASC NULLS FIRST;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    let upper = select_sql.to_uppercase();
    assert!(
        !upper.contains("NULLS FIRST") && !upper.contains("NULLS LAST"),
        "NULLS FIRST must be stripped from ORDER BY, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO nulls_test VALUES (1, 10), (2, 5)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        val: Option<i32>,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert_eq!(rows[0].val, Some(5), "Expected ascending sort; first row should be 5");
    Ok(())
}

/// `NULLS LAST` must also be stripped.
#[test]
fn nulls_last_stripped_from_order_by() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE nulls_test2 (id INTEGER PRIMARY KEY, val INTEGER);
        SELECT val FROM nulls_test2 ORDER BY val DESC NULLS LAST;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    let upper = select_sql.to_uppercase();
    assert!(
        !upper.contains("NULLS FIRST") && !upper.contains("NULLS LAST"),
        "NULLS LAST must be stripped from ORDER BY, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}
