//! Tests for COLLATE clause translation and validation.

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Users table for testing collation.
    users (id) {
        /// User ID.
        id -> Integer,
        /// User name.
        name -> Text,
    }
}

/// A user record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// User ID.
    id: i32,
    /// User name.
    name: String,
}

/// SQLite only knows BINARY, NOCASE, and RTRIM.
/// PostgreSQL collation names like "en_US" or "C" are invalid in SQLite.
#[test]
fn pg_collation_name_causes_error() {
    let sql = "CREATE TABLE coll_test (id INTEGER PRIMARY KEY, name TEXT);
               SELECT * FROM coll_test ORDER BY name COLLATE \"en_US\";";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Non-SQLite collation must cause a translation error");
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err.contains("collat") || err.contains("binary") || err.contains("nocase"),
        "Error must mention SQLite collation names, got: {err}"
    );
}

/// SQLite NOCASE collation must still work after the validation is in place.
#[test]
fn nocase_collation_still_works() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE nc_test (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
               SELECT * FROM nc_test ORDER BY name COLLATE NOCASE;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_uppercase().contains("NOCASE"),
        "NOCASE must be preserved, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// Test COLLATE expression translation (also added during fuzzing fixes).
#[test]
fn test_collate_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT * FROM users ORDER BY name COLLATE NOCASE;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("COLLATE"), "Should contain COLLATE clause, got: {select_stmt}");

    Ok(())
}

/// Test COLLATE semantic execution with diesel SQLite.
#[test]
fn test_collate_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name FROM users ORDER BY name COLLATE NOCASE;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with mixed case
    diesel::insert_into(users::table)
        .values(&User { id: 1, name: "alice".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 2, name: "Bob".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 3, name: "CHARLIE".to_string() })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct NameResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<NameResult>(&mut connection)?;

    assert_eq!(results.len(), 3, "Should have 3 users");
    // With NOCASE collation, ordering should be case-insensitive alphabetical
    assert_eq!(results[0].name, "alice");
    assert_eq!(results[1].name, "Bob");
    assert_eq!(results[2].name, "CHARLIE");

    Ok(())
}
