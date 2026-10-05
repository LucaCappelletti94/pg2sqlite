//! Tests for CONCAT function translation and generated columns.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Table for testing CONCAT function.
    users (id) {
        /// Primary key.
        id -> Integer,
        /// First name.
        first_name -> Text,
        /// Last name.
        last_name -> Text,
    }
}

diesel::table! {
    /// Table for testing CONCAT with multiple columns.
    items (id) {
        /// Primary key.
        id -> Integer,
        /// Column A.
        a -> Nullable<Text>,
        /// Column B.
        b -> Nullable<Text>,
    }
}

diesel::table! {
    /// Table for testing CONCAT with many arguments.
    data (id) {
        /// Primary key.
        id -> Integer,
        /// Column A.
        a -> Nullable<Text>,
        /// Column B.
        b -> Nullable<Text>,
        /// Column C.
        c -> Nullable<Text>,
        /// Column D.
        d -> Nullable<Text>,
    }
}

diesel::table! {
    /// Table for testing generated columns.
    products (id) {
        /// Primary key.
        id -> Integer,
        /// Product price.
        price -> Integer,
        /// Product quantity.
        quantity -> Integer,
        /// Total value (generated column).
        total -> Integer,
    }
}

diesel::table! {
    /// Table for testing generated column expressions.
    rectangles (id) {
        /// Primary key.
        id -> Integer,
        /// Width of rectangle.
        width -> Integer,
        /// Height of rectangle.
        height -> Integer,
        /// Area (generated column).
        area -> Integer,
    }
}

/// A user record for testing CONCAT.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// Primary key.
    id: i32,
    /// First name.
    first_name: String,
    /// Last name.
    last_name: String,
}

/// An item record for testing CONCAT.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = items)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Item {
    /// Primary key.
    id: i32,
    /// Column A.
    a: Option<String>,
    /// Column B.
    b: Option<String>,
}

/// A data record for testing CONCAT with many arguments.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = data)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Data {
    /// Primary key.
    id: i32,
    /// Column A.
    a: Option<String>,
    /// Column B.
    b: Option<String>,
    /// Column C.
    c: Option<String>,
    /// Column D.
    d: Option<String>,
}

/// A product record (for insertion only, excluding generated column).
#[derive(Insertable)]
#[diesel(table_name = products)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NewProduct {
    /// Primary key.
    id: i32,
    /// Product price.
    price: i32,
    /// Product quantity.
    quantity: i32,
}

/// A rectangle record (for insertion only, excluding generated column).
#[derive(Insertable)]
#[diesel(table_name = rectangles)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NewRectangle {
    /// Primary key.
    id: i32,
    /// Width of rectangle.
    width: i32,
    /// Height of rectangle.
    height: i32,
}

#[test]
fn test_concat_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULL
        );
        SELECT CONCAT(first_name, ' ', last_name) as full_name FROM users;
    ";

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
    let mut conn = diesel::SqliteConnection::establish(":memory:")?;
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn)?;
    }

    Ok(())
}

#[test]
fn test_concat_two_args() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (id SERIAL PRIMARY KEY, a TEXT, b TEXT);
        SELECT CONCAT(a, b) FROM items;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("||"), "Should use || operator, got: {select_stmt}");
    let mut conn = diesel::SqliteConnection::establish(":memory:")?;
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn)?;
    }

    Ok(())
}

#[test]
fn test_concat_single_arg() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (id SERIAL PRIMARY KEY, a TEXT);
        SELECT CONCAT(a) FROM items;
    ";

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
    let mut conn = diesel::SqliteConnection::establish(":memory:")?;
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn)?;
    }

    Ok(())
}

/// Semantic test: CONCAT translation works correctly in SQLite.
#[test]
fn test_concat_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULL
        );
        SELECT CONCAT(first_name, ' ', last_name) as full_name FROM users;
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
        .values(&User { id: 1, first_name: "John".to_string(), last_name: "Doe".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 2, first_name: "Jane".to_string(), last_name: "Smith".to_string() })
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

#[test]
fn test_concat_many_args_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE data (id SERIAL PRIMARY KEY, a TEXT, b TEXT, c TEXT, d TEXT);
        SELECT CONCAT(a, '-', b, '-', c, '-', d) as combined FROM data;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(data::table)
        .values(&Data {
            id: 1,
            a: Some("one".to_string()),
            b: Some("two".to_string()),
            c: Some("three".to_string()),
            d: Some("four".to_string()),
        })
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

#[test]
fn test_generated_column_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id SERIAL PRIMARY KEY,
            price INTEGER NOT NULL,
            quantity INTEGER NOT NULL,
            total INTEGER GENERATED ALWAYS AS (price * quantity) STORED
        );
    ";

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
    let mut conn = diesel::SqliteConnection::establish(":memory:")?;
    for s in &translated {
        diesel::sql_query(s.to_string()).execute(&mut conn)?;
    }

    Ok(())
}

/// Semantic test: generated columns work correctly in SQLite.
#[test]
fn test_generated_column_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id SERIAL PRIMARY KEY,
            price INTEGER NOT NULL,
            quantity INTEGER NOT NULL,
            total INTEGER GENERATED ALWAYS AS (price * quantity) STORED
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM (don't specify total - it's generated)
    diesel::insert_into(products::table)
        .values(&NewProduct { id: 1, price: 10, quantity: 5 })
        .execute(&mut connection)?;
    diesel::insert_into(products::table)
        .values(&NewProduct { id: 2, price: 25, quantity: 4 })
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

#[test]
fn test_generated_column_expression() -> Result<(), Box<dyn std::error::Error>> {
    // PostgreSQL only supports STORED for generated columns (VIRTUAL is
    // SQLite-specific)
    let sql = "
        CREATE TABLE rectangles (
            id SERIAL PRIMARY KEY,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            area INTEGER GENERATED ALWAYS AS (width * height) STORED
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(rectangles::table)
        .values(&NewRectangle { id: 1, width: 3, height: 4 })
        .execute(&mut connection)?;
    diesel::insert_into(rectangles::table)
        .values(&NewRectangle { id: 2, width: 5, height: 6 })
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
