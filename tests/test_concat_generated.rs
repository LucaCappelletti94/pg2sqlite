//! Tests for CONCAT function translation and generated columns.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Test that CONCAT(a, b, c) is translated to a || b || c.
#[test]
fn test_concat_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULL
        );
        SELECT CONCAT(first_name, ' ', last_name) as full_name FROM users;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // CONCAT should be converted to || operator
    assert!(
        select_stmt.contains("||"),
        "CONCAT should be converted to || operator, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("CONCAT"),
        "CONCAT function should not appear in output, got: {select_stmt}"
    );

    Ok(())
}

/// Test CONCAT with two arguments.
#[test]
fn test_concat_two_args() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (id SERIAL PRIMARY KEY, a TEXT, b TEXT);
        SELECT CONCAT(a, b) FROM items;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("||"), "Should use || operator, got: {select_stmt}");

    Ok(())
}

/// Test CONCAT with single argument (degenerate case).
#[test]
fn test_concat_single_arg() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (id SERIAL PRIMARY KEY, a TEXT);
        SELECT CONCAT(a) FROM items;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Single arg CONCAT should just return the argument
    assert!(
        !select_stmt.to_uppercase().contains("CONCAT"),
        "CONCAT should not appear, got: {select_stmt}"
    );

    Ok(())
}

/// Semantic test: CONCAT translation works correctly in SQLite.
#[test]
fn test_concat_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULL
        );
        SELECT CONCAT(first_name, ' ', last_name) as full_name FROM users;
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
    diesel::sql_query("INSERT INTO users (first_name, last_name) VALUES ('John', 'Doe')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO users (first_name, last_name) VALUES ('Jane', 'Smith')")
        .execute(&mut connection)?;

    // Execute the SELECT
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct FullName {
        #[diesel(sql_type = diesel::sql_types::Text)]
        full_name: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<FullName>(&mut connection)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].full_name, "John Doe");
    assert_eq!(results[1].full_name, "Jane Smith");

    Ok(())
}

/// Test CONCAT with multiple (4+) arguments.
#[test]
fn test_concat_many_args_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (id SERIAL PRIMARY KEY, a TEXT, b TEXT, c TEXT, d TEXT);
        SELECT CONCAT(a, '-', b, '-', c, '-', d) as combined FROM data;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query("INSERT INTO data (a, b, c, d) VALUES ('one', 'two', 'three', 'four')")
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct Combined {
        #[diesel(sql_type = diesel::sql_types::Text)]
        combined: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<Combined>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].combined, "one-two-three-four");

    Ok(())
}

/// Test generated column translation (GENERATED ALWAYS AS).
#[test]
fn test_generated_column_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE products (
            id SERIAL PRIMARY KEY,
            price INTEGER NOT NULL,
            quantity INTEGER NOT NULL,
            total INTEGER GENERATED ALWAYS AS (price * quantity) STORED
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let create_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::CreateTable(_)))
        .expect("Should have a CREATE TABLE statement")
        .to_string();

    // Should contain GENERATED ALWAYS AS
    assert!(
        create_stmt.contains("GENERATED ALWAYS AS"),
        "Should preserve GENERATED ALWAYS AS, got: {create_stmt}"
    );

    Ok(())
}

/// Semantic test: generated columns work correctly in SQLite.
#[test]
fn test_generated_column_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE products (
            id SERIAL PRIMARY KEY,
            price INTEGER NOT NULL,
            quantity INTEGER NOT NULL,
            total INTEGER GENERATED ALWAYS AS (price * quantity) STORED
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data (don't specify total - it's generated)
    diesel::sql_query("INSERT INTO products (price, quantity) VALUES (10, 5)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO products (price, quantity) VALUES (25, 4)")
        .execute(&mut connection)?;

    // Query the generated column
    #[derive(QueryableByName, Debug)]
    struct Product {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        total: i32,
    }

    let results = diesel::sql_query("SELECT total FROM products ORDER BY id")
        .load::<Product>(&mut connection)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].total, 50); // 10 * 5
    assert_eq!(results[1].total, 100); // 25 * 4

    Ok(())
}

/// Test generated column with expression using multiple columns.
#[test]
fn test_generated_column_expression() -> Result<(), Box<dyn std::error::Error>> {
    // PostgreSQL only supports STORED for generated columns (VIRTUAL is
    // SQLite-specific)
    let sql = r#"
        CREATE TABLE rectangles (
            id SERIAL PRIMARY KEY,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            area INTEGER GENERATED ALWAYS AS (width * height) STORED
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query("INSERT INTO rectangles (width, height) VALUES (3, 4)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO rectangles (width, height) VALUES (5, 6)")
        .execute(&mut connection)?;

    #[derive(QueryableByName, Debug)]
    struct Rect {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        area: i32,
    }

    let results = diesel::sql_query("SELECT area FROM rectangles ORDER BY id")
        .load::<Rect>(&mut connection)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].area, 12); // 3 * 4
    assert_eq!(results[1].area, 30); // 5 * 6

    Ok(())
}
