//! Tests for expression translation from PostgreSQL to SQLite.
//!
//! These tests verify that various SQL expressions are properly translated,
//! including IN lists, BETWEEN, CASE, subqueries, and EXTRACT.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

diesel::table! {
    /// Test table for items with categories (used in IN list tests).
    items (id) {
        /// Item ID.
        id -> Integer,
        /// Item category.
        category -> Text,
    }
}

diesel::table! {
    /// Test table for users (used in subquery tests).
    users (id) {
        /// User ID.
        id -> Integer,
        /// User name.
        name -> Text,
    }
}

diesel::table! {
    /// Test table for orders (used in multiple tests with varying schemas).
    /// Different tests create this table with different combinations of columns.
    orders (id) {
        /// Order ID.
        id -> Integer,
        /// User ID who made the order (nullable - not present in all tests).
        user_id -> Nullable<Integer>,
        /// Order amount (nullable - not present in all tests).
        amount -> Nullable<Integer>,
        /// Order status (nullable - not present in all tests).
        status -> Nullable<Text>,
        /// Order date (nullable - not present in all tests).
        order_date -> Nullable<Text>,
    }
}

diesel::table! {
    /// Test table for products (used in BETWEEN tests).
    products (id) {
        /// Product ID.
        id -> Integer,
        /// Product price.
        price -> Integer,
    }
}

diesel::table! {
    /// Test table for scores (used in CASE tests).
    scores (id) {
        /// Score ID.
        id -> Integer,
        /// Score value.
        score -> Integer,
    }
}

diesel::table! {
    /// Test table for events (used in EXTRACT tests).
    events (id) {
        /// Event ID.
        id -> Integer,
        /// Event date.
        event_date -> Text,
        /// Event name (nullable in some tests).
        name -> Nullable<Text>,
    }
}

diesel::table! {
    /// Test table for logs (used in EXTRACT time tests).
    logs (id) {
        /// Log ID.
        id -> Integer,
        /// Log timestamp.
        timestamp -> Text,
    }
}

/// An item with a category.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = items)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Item {
    /// Item ID.
    id: i32,
    /// Item category.
    category: String,
}

/// A user.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// User ID.
    id: i32,
    /// User name.
    name: String,
}

/// An order.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Order {
    /// Order ID.
    id: i32,
    /// User ID.
    user_id: Option<i32>,
    /// Order amount.
    amount: Option<i32>,
    /// Order status.
    status: Option<String>,
    /// Date of the order.
    #[diesel(column_name = "order_date")]
    date: Option<String>,
}

/// A product.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = products)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Product {
    /// Product ID.
    id: i32,
    /// Product price.
    price: i32,
}

/// A score.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = scores)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Score {
    /// Score ID.
    id: i32,
    /// Score value.
    score: i32,
}

/// An event.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Event {
    /// Event ID.
    id: i32,
    /// Date of the event.
    #[diesel(column_name = "event_date")]
    date: String,
    /// Event name.
    name: Option<String>,
}

/// A log entry.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = logs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Log {
    /// Log ID.
    id: i32,
    /// Log timestamp.
    timestamp: String,
}

#[test]
fn test_in_list_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            category TEXT NOT NULL
        );
        SELECT * FROM items WHERE category IN ('electronics', 'books', 'toys');
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("IN ("), "Should contain IN clause, got: {select_stmt}");

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_in_list_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            category TEXT NOT NULL
        );
        SELECT id FROM items WHERE category IN ('electronics', 'books');
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(items::table)
        .values(&Item { id: 1, category: "electronics".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 2, category: "books".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 3, category: "clothing".to_string() })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct IdResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<IdResult>(&mut connection)?;

    assert_eq!(results.len(), 2, "Should match 2 items");
    let ids: Vec<i32> = results.iter().map(|r| r.id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(!ids.contains(&3));

    Ok(())
}

#[test]
fn test_in_subquery_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL
        );
        SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("IN (SELECT"),
        "Should contain IN (SELECT ...), got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_in_subquery_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL
        );
        SELECT name FROM users WHERE id IN (SELECT user_id FROM orders);
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(users::table)
        .values(&User { id: 1, name: "Alice".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 2, name: "Bob".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 3, name: "Carol".to_string() })
        .execute(&mut connection)?;
    // Only Alice and Bob have orders (using DSL to insert only specific
    // columns)
    use crate::orders::dsl::*;
    diesel::insert_into(orders).values((id.eq(1), user_id.eq(Some(1)))).execute(&mut connection)?;
    diesel::insert_into(orders).values((id.eq(2), user_id.eq(Some(2)))).execute(&mut connection)?;

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

    assert_eq!(results.len(), 2, "Should match 2 users with orders");
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"Alice"));
    assert!(names.contains(&"Bob"));
    assert!(!names.contains(&"Carol"));

    Ok(())
}

#[test]
fn test_between_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            price INTEGER NOT NULL
        );
        SELECT * FROM products WHERE price BETWEEN 10 AND 50;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("BETWEEN"), "Should contain BETWEEN, got: {select_stmt}");

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_between_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            price INTEGER NOT NULL
        );
        SELECT id FROM products WHERE price BETWEEN 10 AND 50;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(products::table)
        .values(&Product { id: 1, price: 5 })
        .execute(&mut connection)?; // too low
    diesel::insert_into(products::table)
        .values(&Product { id: 2, price: 10 })
        .execute(&mut connection)?; // boundary
    diesel::insert_into(products::table)
        .values(&Product { id: 3, price: 30 })
        .execute(&mut connection)?; // in range
    diesel::insert_into(products::table)
        .values(&Product { id: 4, price: 50 })
        .execute(&mut connection)?; // boundary
    diesel::insert_into(products::table)
        .values(&Product { id: 5, price: 100 })
        .execute(&mut connection)?; // too high

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct IdResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<IdResult>(&mut connection)?;

    assert_eq!(results.len(), 3, "Should match 3 products in range [10, 50]");
    let ids: Vec<i32> = results.iter().map(|r| r.id).collect();
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(ids.contains(&4));

    Ok(())
}

#[test]
fn test_case_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL
        );
        SELECT id, CASE status
            WHEN 'active' THEN 'Active'
            WHEN 'pending' THEN 'Pending'
            ELSE 'Unknown'
        END as status_label
        FROM items;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("CASE"), "Should contain CASE, got: {select_stmt}");
    assert!(select_stmt.contains("WHEN"), "Should contain WHEN, got: {select_stmt}");
    assert!(select_stmt.contains("END"), "Should contain END, got: {select_stmt}");

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_case_searched_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE scores (
            id INTEGER PRIMARY KEY,
            score INTEGER NOT NULL
        );
        SELECT id, CASE
            WHEN score >= 90 THEN 'A'
            WHEN score >= 80 THEN 'B'
            WHEN score >= 70 THEN 'C'
            ELSE 'F'
        END as grade
        FROM scores;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(scores::table)
        .values(&Score { id: 1, score: 95 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 2, score: 85 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 3, score: 75 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 4, score: 50 })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct GradeResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        grade: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<GradeResult>(&mut connection)?;

    assert_eq!(results.len(), 4);
    let grade_map: std::collections::HashMap<i32, &str> =
        results.iter().map(|r| (r.id, r.grade.as_str())).collect();
    assert_eq!(grade_map.get(&1), Some(&"A"));
    assert_eq!(grade_map.get(&2), Some(&"B"));
    assert_eq!(grade_map.get(&3), Some(&"C"));
    assert_eq!(grade_map.get(&4), Some(&"F"));

    Ok(())
}

#[test]
fn test_scalar_subquery_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            amount INTEGER NOT NULL
        );
        SELECT (SELECT MAX(amount) FROM orders) as max_amount;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("(SELECT"), "Should contain scalar subquery, got: {select_stmt}");

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_scalar_subquery_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            amount INTEGER NOT NULL
        );
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name, (SELECT SUM(amount) FROM orders WHERE user_id = users.id) as total
        FROM users;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(users::table)
        .values(&User { id: 1, name: "Alice".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 2, name: "Bob".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 1, user_id: Some(1), amount: Some(100), status: None, date: None })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 2, user_id: Some(1), amount: Some(50), status: None, date: None })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 3, user_id: Some(2), amount: Some(200), status: None, date: None })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct UserTotal {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        total: Option<i32>,
    }

    let results = diesel::sql_query(&select_stmt).load::<UserTotal>(&mut connection)?;

    assert_eq!(results.len(), 2);
    let alice = results.iter().find(|r| r.name == "Alice").expect("Should have Alice");
    assert_eq!(alice.total, Some(150));
    let bob = results.iter().find(|r| r.name == "Bob").expect("Should have Bob");
    assert_eq!(bob.total, Some(200));

    Ok(())
}

#[test]
fn test_extract_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL
        );
        SELECT id, EXTRACT(YEAR FROM event_date) as year FROM events;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain strftime, not EXTRACT
    assert!(
        select_stmt.contains("strftime"),
        "EXTRACT should translate to strftime, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("EXTRACT"),
        "Should not contain EXTRACT, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the
    // translated SQL.
    {
        let conn = SqliteConn::open_in_memory()?;
        let ddl = translated
            .iter()
            .filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&ddl)?;
        conn.prepare(&select_stmt)?;
    }
    Ok(())
}

#[test]
fn test_extract_fields_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL
        );
        SELECT
            EXTRACT(YEAR FROM event_date) as year,
            EXTRACT(MONTH FROM event_date) as month,
            EXTRACT(DAY FROM event_date) as day
        FROM events;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with ISO date format using Diesel ORM
    use crate::events::dsl::*;
    diesel::insert_into(events)
        .values((id.eq(1), event_date.eq("2024-03-15")))
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct DateParts {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        year: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        month: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        day: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<DateParts>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].year, 2024);
    assert_eq!(results[0].month, 3);
    assert_eq!(results[0].day, 15);

    Ok(())
}

#[test]
fn test_extract_time_fields_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE logs (
            id INTEGER PRIMARY KEY,
            timestamp TEXT NOT NULL
        );
        SELECT
            EXTRACT(HOUR FROM timestamp) as hour,
            EXTRACT(MINUTE FROM timestamp) as minute,
            EXTRACT(SECOND FROM timestamp) as second
        FROM logs;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with ISO datetime format using Diesel ORM
    diesel::insert_into(logs::table)
        .values(&Log { id: 1, timestamp: "2024-03-15 14:30:45".to_string() })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct TimeParts {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        hour: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        minute: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        second: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<TimeParts>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].hour, 14);
    assert_eq!(results[0].minute, 30);
    assert_eq!(results[0].second, 45);

    Ok(())
}

#[test]
fn test_extract_in_where_clause() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL,
            name TEXT NOT NULL
        );
        SELECT name FROM events WHERE EXTRACT(YEAR FROM event_date) = 2024;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(events::table)
        .values(&Event { id: 1, date: "2024-01-15".to_string(), name: Some("Event A".to_string()) })
        .execute(&mut connection)?;
    diesel::insert_into(events::table)
        .values(&Event { id: 2, date: "2023-06-20".to_string(), name: Some("Event B".to_string()) })
        .execute(&mut connection)?;
    diesel::insert_into(events::table)
        .values(&Event { id: 3, date: "2024-12-25".to_string(), name: Some("Event C".to_string()) })
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

    assert_eq!(results.len(), 2, "Should match 2 events from 2024");
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"Event A"));
    assert!(names.contains(&"Event C"));
    assert!(!names.contains(&"Event B"));

    Ok(())
}

#[test]
fn test_combined_expressions() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            amount INTEGER NOT NULL,
            order_date TEXT NOT NULL
        );
        SELECT
            id,
            CASE
                WHEN amount >= 1000 THEN 'large'
                WHEN amount >= 100 THEN 'medium'
                ELSE 'small'
            END as size,
            EXTRACT(MONTH FROM order_date) as month
        FROM orders
        WHERE status IN ('pending', 'shipped')
          AND amount BETWEEN 50 AND 5000;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(orders::table)
        .values(&Order {
            id: 1,
            user_id: None,
            status: Some("pending".to_string()),
            amount: Some(500),
            date: Some("2024-03-15".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order {
            id: 2,
            user_id: None,
            status: Some("shipped".to_string()),
            amount: Some(1500),
            date: Some("2024-06-20".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order {
            id: 3,
            user_id: None,
            status: Some("cancelled".to_string()),
            amount: Some(200),
            date: Some("2024-01-10".to_string()),
        })
        .execute(&mut connection)?; // excluded by status
    diesel::insert_into(orders::table)
        .values(&Order {
            id: 4,
            user_id: None,
            status: Some("pending".to_string()),
            amount: Some(10),
            date: Some("2024-02-01".to_string()),
        })
        .execute(&mut connection)?; // excluded by amount

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct OrderResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        size: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        month: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<OrderResult>(&mut connection)?;

    assert_eq!(results.len(), 2, "Should match 2 orders");

    let order1 = results.iter().find(|r| r.id == 1).expect("Should have order 1");
    assert_eq!(order1.size, "medium");
    assert_eq!(order1.month, 3);

    let order2 = results.iter().find(|r| r.id == 2).expect("Should have order 2");
    assert_eq!(order2.size, "large");
    assert_eq!(order2.month, 6);

    Ok(())
}

mod at_time_zone_schema {
    diesel::table! {
        time_samples (id) {
            id -> Integer,
            ts -> Text,
        }
    }
}

use at_time_zone_schema::time_samples;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = time_samples)]
struct TimeSample {
    id: i32,
    ts: String,
}

#[derive(Debug, QueryableByName)]
struct ShiftedTimeRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    shifted: String,
}

/// `ts` is declared TIMESTAMP rather than TEXT because the two `AT TIME ZONE`
/// directions are chosen by the operand's declared type, and a bare TEXT column
/// says nothing. The expected values are the naive direction: PostgreSQL reads
/// the string `'+02:00'` as a POSIX zone, so a naive operand gains the offset.
#[test]
fn test_at_time_zone_fixed_offset_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE time_samples (
            id INTEGER PRIMARY KEY,
            ts TIMESTAMP NOT NULL
        );
        SELECT id, ts AT TIME ZONE '+02:00' AS shifted
        FROM time_samples
        ORDER BY id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        !select_stmt.to_uppercase().contains("AT TIME ZONE"),
        "AT TIME ZONE should be rewritten, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_lowercase().contains("datetime"),
        "Expected datetime(...) rewrite, got: {select_stmt}"
    );

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(time_samples::table)
        .values(&[
            TimeSample { id: 1, ts: "2024-03-15 10:00:00".to_string() },
            TimeSample { id: 2, ts: "2024-03-15 23:30:00".to_string() },
        ])
        .execute(&mut connection)?;

    let rows: Vec<ShiftedTimeRow> = diesel::sql_query(select_stmt).load(&mut connection)?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].shifted, "2024-03-15 12:00:00");
    assert_eq!(rows[1].id, 2);
    assert_eq!(rows[1].shifted, "2024-03-16 01:30:00");

    Ok(())
}

/// EXTRACT refusal must list WEEK, ISODOW, ISOYEAR as supported fields.
/// They are implemented but the old error text omitted them. R2-LOW.
#[test]
fn extract_unsupported_field_message_lists_week_isodow_isoyear() {
    let err = Pg2Sqlite::default()
        .sql("SELECT EXTRACT(TIMEZONE FROM TIMESTAMP '2024-01-01 12:00:00')")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("EXTRACT(TIMEZONE) must fail")
        .to_string();
    assert!(err.contains("WEEK"), "EXTRACT refusal must list WEEK, got: {err}");
    assert!(err.contains("ISODOW"), "EXTRACT refusal must list ISODOW, got: {err}");
    assert!(err.contains("ISOYEAR"), "EXTRACT refusal must list ISOYEAR, got: {err}");
}

/// to_char with MS code must name "MS" in the refusal, not just "M". R2-LOW.
#[test]
fn to_char_ms_code_names_ms_in_refusal() {
    let err = Pg2Sqlite::default()
        .sql("SELECT to_char(NOW(), 'HH24:MI:SS.MS')")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("to_char with MS must fail")
        .to_string();
    assert!(err.contains("MS"), "refusal must name 'MS', got: {err}");
    assert!(!err.contains("contains 'M'"), "must name MS not just M, got: {err}");
}

/// to_char with US code must name "US" in the refusal. R2-LOW.
#[test]
fn to_char_us_code_names_us_in_refusal() {
    let err = Pg2Sqlite::default()
        .sql("SELECT to_char(NOW(), 'HH24:MI:SS.US')")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("to_char with US must fail")
        .to_string();
    assert!(err.contains("US"), "refusal must name 'US', got: {err}");
}
