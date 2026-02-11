//! Tests for expression translation from PostgreSQL to SQLite.
//!
//! These tests verify that various SQL expressions are properly translated,
//! including IN lists, BETWEEN, CASE, subqueries, and EXTRACT.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// IN List Tests
// ============================================================================

/// Test that IN list expressions are translated correctly.
#[test]
fn test_in_list_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            category TEXT NOT NULL
        );
        SELECT * FROM items WHERE category IN ('electronics', 'books', 'toys');
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("IN ("), "Should contain IN clause, got: {select_stmt}");

    Ok(())
}

/// Test IN list semantic execution.
#[test]
fn test_in_list_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            category TEXT NOT NULL
        );
        SELECT id FROM items WHERE category IN ('electronics', 'books');
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO items (id, category) VALUES (1, 'electronics')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO items (id, category) VALUES (2, 'books')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO items (id, category) VALUES (3, 'clothing')")
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

// ============================================================================
// IN Subquery Tests
// ============================================================================

/// Test that IN subquery expressions are translated correctly.
#[test]
fn test_in_subquery_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL
        );
        SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);
    "#;

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

    Ok(())
}

/// Test IN subquery semantic execution.
#[test]
fn test_in_subquery_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL
        );
        SELECT name FROM users WHERE id IN (SELECT user_id FROM orders);
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO users (id, name) VALUES (2, 'Bob')").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO users (id, name) VALUES (3, 'Carol')")
        .execute(&mut connection)?;
    // Only Alice and Bob have orders
    diesel::sql_query("INSERT INTO orders (id, user_id) VALUES (1, 1)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (id, user_id) VALUES (2, 2)").execute(&mut connection)?;

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

// ============================================================================
// BETWEEN Tests
// ============================================================================

/// Test that BETWEEN expressions are translated correctly.
#[test]
fn test_between_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            price INTEGER NOT NULL
        );
        SELECT * FROM products WHERE price BETWEEN 10 AND 50;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("BETWEEN"), "Should contain BETWEEN, got: {select_stmt}");

    Ok(())
}

/// Test BETWEEN semantic execution.
#[test]
fn test_between_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            price INTEGER NOT NULL
        );
        SELECT id FROM products WHERE price BETWEEN 10 AND 50;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO products (id, price) VALUES (1, 5)").execute(&mut connection)?; // too low
    diesel::sql_query("INSERT INTO products (id, price) VALUES (2, 10)")
        .execute(&mut connection)?; // boundary
    diesel::sql_query("INSERT INTO products (id, price) VALUES (3, 30)")
        .execute(&mut connection)?; // in range
    diesel::sql_query("INSERT INTO products (id, price) VALUES (4, 50)")
        .execute(&mut connection)?; // boundary
    diesel::sql_query("INSERT INTO products (id, price) VALUES (5, 100)")
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

// ============================================================================
// CASE Expression Tests
// ============================================================================

/// Test that CASE expressions are translated correctly.
#[test]
fn test_case_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
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
    "#;

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

    Ok(())
}

/// Test searched CASE expression (without operand).
#[test]
fn test_case_searched_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
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
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO scores (id, score) VALUES (1, 95)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (id, score) VALUES (2, 85)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (id, score) VALUES (3, 75)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (id, score) VALUES (4, 50)").execute(&mut connection)?;

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

// ============================================================================
// Scalar Subquery Tests
// ============================================================================

/// Test that scalar subqueries are translated correctly.
#[test]
fn test_scalar_subquery_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            amount INTEGER NOT NULL
        );
        SELECT (SELECT MAX(amount) FROM orders) as max_amount;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("(SELECT"), "Should contain scalar subquery, got: {select_stmt}");

    Ok(())
}

/// Test scalar subquery semantic execution.
#[test]
fn test_scalar_subquery_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
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
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO users (id, name) VALUES (2, 'Bob')").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (id, user_id, amount) VALUES (1, 1, 100)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (id, user_id, amount) VALUES (2, 1, 50)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (id, user_id, amount) VALUES (3, 2, 200)")
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

// ============================================================================
// EXTRACT Tests
// ============================================================================

/// Test that EXTRACT is translated to strftime.
#[test]
fn test_extract_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL
        );
        SELECT id, EXTRACT(YEAR FROM event_date) as year FROM events;
    "#;

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

    Ok(())
}

/// Test EXTRACT with various date fields.
#[test]
fn test_extract_fields_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL
        );
        SELECT
            EXTRACT(YEAR FROM event_date) as year,
            EXTRACT(MONTH FROM event_date) as month,
            EXTRACT(DAY FROM event_date) as day
        FROM events;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with ISO date format
    diesel::sql_query("INSERT INTO events (id, event_date) VALUES (1, '2024-03-15')")
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

/// Test EXTRACT with time fields.
#[test]
fn test_extract_time_fields_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE logs (
            id INTEGER PRIMARY KEY,
            timestamp TEXT NOT NULL
        );
        SELECT
            EXTRACT(HOUR FROM timestamp) as hour,
            EXTRACT(MINUTE FROM timestamp) as minute,
            EXTRACT(SECOND FROM timestamp) as second
        FROM logs;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with ISO datetime format
    diesel::sql_query("INSERT INTO logs (id, timestamp) VALUES (1, '2024-03-15 14:30:45')")
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

/// Test EXTRACT in WHERE clause.
#[test]
fn test_extract_in_where_clause() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_date TEXT NOT NULL,
            name TEXT NOT NULL
        );
        SELECT name FROM events WHERE EXTRACT(YEAR FROM event_date) = 2024;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query(
        "INSERT INTO events (id, event_date, name) VALUES (1, '2024-01-15', 'Event A')",
    )
    .execute(&mut connection)?;
    diesel::sql_query(
        "INSERT INTO events (id, event_date, name) VALUES (2, '2023-06-20', 'Event B')",
    )
    .execute(&mut connection)?;
    diesel::sql_query(
        "INSERT INTO events (id, event_date, name) VALUES (3, '2024-12-25', 'Event C')",
    )
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

// ============================================================================
// Combined Expression Tests
// ============================================================================

/// Test combining multiple expression types in a single query.
#[test]
fn test_combined_expressions() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
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
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query(
        "INSERT INTO orders (id, status, amount, order_date) VALUES (1, 'pending', 500, '2024-03-15')",
    )
    .execute(&mut connection)?;
    diesel::sql_query(
        "INSERT INTO orders (id, status, amount, order_date) VALUES (2, 'shipped', 1500, '2024-06-20')",
    )
    .execute(&mut connection)?;
    diesel::sql_query(
        "INSERT INTO orders (id, status, amount, order_date) VALUES (3, 'cancelled', 200, '2024-01-10')",
    )
    .execute(&mut connection)?; // excluded by status
    diesel::sql_query(
        "INSERT INTO orders (id, status, amount, order_date) VALUES (4, 'pending', 10, '2024-02-01')",
    )
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
