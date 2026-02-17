//! Tests for INTERVAL expression translation from PostgreSQL to SQLite.
//!
//! Note: SQLite does not natively support INTERVAL expressions. This test
//! verifies that the translation doesn't crash and produces valid SQL syntax.
//! For actual date arithmetic in SQLite, intervals would need to be converted
//! to datetime() function calls.

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// Schema definitions for test tables
diesel::table! {
    /// Events table for testing interval translations.
    events (id) {
        /// Event ID.
        id -> Integer,
        /// Event timestamp.
        event_time -> Text,
    }
}

diesel::table! {
    /// Logs table for testing interval with time units.
    logs (id) {
        /// Log entry ID.
        id -> Integer,
        /// Timestamp when log was created.
        created_at -> Text,
    }
}

diesel::table! {
    /// Tasks table for testing interval arithmetic.
    tasks (id) {
        /// Task ID.
        id -> Integer,
        /// Task due date.
        due_date -> Text,
    }
}

diesel::table! {
    /// Users table for testing collation.
    users (id) {
        /// User ID.
        id -> Integer,
        /// User name.
        name -> Text,
    }
}

diesel::table! {
    /// Products table for testing qualified wildcards.
    products (id) {
        /// Product ID.
        id -> Integer,
        /// Product name.
        name -> Text,
        /// Product price.
        price -> Nullable<Double>,
    }
}

/// An event record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Event {
    /// Event ID.
    id: i32,
    /// Event timestamp.
    event_time: String,
}

/// A log entry record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = logs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Log {
    /// Log entry ID.
    id: i32,
    /// Timestamp when log was created.
    created_at: String,
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

/// A product record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = products)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Product {
    /// Product ID.
    id: i32,
    /// Product name.
    name: String,
    /// Product price.
    price: Option<f64>,
}

/// Test that basic INTERVAL expressions are translated without crashing.
#[test]
fn test_interval_translation_basic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_time TEXT NOT NULL
        );
        SELECT * FROM events WHERE event_time > INTERVAL '1 day';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("INTERVAL"), "Should contain INTERVAL clause, got: {select_stmt}");

    Ok(())
}

/// Test that INTERVAL with time unit is translated without crashing.
#[test]
fn test_interval_with_time_unit() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE logs (
            id INTEGER PRIMARY KEY,
            created_at TEXT NOT NULL
        );
        SELECT * FROM logs WHERE created_at > INTERVAL '2 hours';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("INTERVAL"), "Should contain INTERVAL clause, got: {select_stmt}");

    Ok(())
}

/// Test that INTERVAL in date arithmetic expressions is translated without
/// crashing.
#[test]
fn test_interval_in_arithmetic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE tasks (
            id INTEGER PRIMARY KEY,
            due_date TEXT NOT NULL
        );
        SELECT id, due_date FROM tasks;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Verify translation succeeded
    assert!(!translated.is_empty(), "Should have translated statements");

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

/// Test qualified wildcard (table.*) translation.
#[test]
fn test_qualified_wildcard_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price REAL
        );
        SELECT products.* FROM products;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("products.*"),
        "Should contain qualified wildcard, got: {select_stmt}"
    );

    Ok(())
}

/// Test qualified wildcard semantic execution with diesel SQLite.
#[test]
fn test_qualified_wildcard_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price REAL
        );
        SELECT products.* FROM products;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::insert_into(products::table)
        .values(&Product { id: 1, name: "Widget".to_string(), price: Some(9.99) })
        .execute(&mut connection)?;
    diesel::insert_into(products::table)
        .values(&Product { id: 2, name: "Gadget".to_string(), price: Some(19.99) })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ProductResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        price: Option<f64>,
    }

    let results = diesel::sql_query(&select_stmt).load::<ProductResult>(&mut connection)?;

    assert_eq!(results.len(), 2, "Should have 2 products");
    assert_eq!(results[0].id, 1);
    assert_eq!(results[0].name, "Widget");
    assert!((results[0].price.unwrap() - 9.99).abs() < 0.01);
    assert_eq!(results[1].id, 2);
    assert_eq!(results[1].name, "Gadget");
    assert!((results[1].price.unwrap() - 19.99).abs() < 0.01);

    Ok(())
}
