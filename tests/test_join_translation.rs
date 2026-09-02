//! Tests for JOIN condition translation from PostgreSQL to SQLite.
//!
//! These tests verify that expressions in JOIN ON clauses are properly
//! translated (e.g., ILIKE -> LIKE, function translations, etc.).

#![allow(dead_code)]

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Test table for authors (used in JOIN tests).
    authors (id) {
        /// Author ID.
        id -> Integer,
        /// Author name.
        name -> Text,
    }
}

diesel::table! {
    /// Test table for books (used in JOIN tests).
    books (id) {
        /// Book ID.
        id -> Integer,
        /// Author name.
        author_name -> Text,
        /// Book title.
        title -> Text,
    }
}

diesel::table! {
    /// Test table for ranges (used in LEAST/GREATEST tests).
    ranges (id) {
        /// Range ID.
        id -> Integer,
        /// Low value.
        low -> Integer,
        /// High value.
        high -> Integer,
    }
}

diesel::table! {
    /// Test table for numeric values (used in range tests).
    nums (id) {
        /// Number ID.
        id -> Integer,
        /// Numeric value.
        val -> Integer,
    }
}

diesel::table! {
    /// Test table for departments (used in LEFT JOIN tests).
    departments (id) {
        /// Department ID.
        id -> Integer,
        /// Department name.
        name -> Text,
    }
}

diesel::table! {
    /// Test table for employees (used in LEFT JOIN tests).
    employees (id) {
        /// Employee ID.
        id -> Integer,
        /// Department name.
        dept_name -> Text,
        /// Employee name.
        name -> Text,
    }
}

diesel::table! {
    /// Test table for orders (used in derived table tests).
    orders (id) {
        /// Order ID.
        id -> Integer,
        /// Customer ID.
        customer_id -> Integer,
        /// Order amount.
        amount -> Integer,
    }
}

diesel::table! {
    /// Test table for customers (used in derived table tests).
    customers (id) {
        /// Customer ID.
        id -> Integer,
        /// Customer name.
        name -> Text,
    }
}

/// An author.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = authors)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Author {
    /// Author ID.
    id: i32,
    /// Author name.
    name: String,
}

/// A book.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = books)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Book {
    /// Book ID.
    id: i32,
    /// Author name.
    author_name: String,
    /// Book title.
    title: String,
}

/// A range with low and high values.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = ranges)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Range {
    /// Range ID.
    id: i32,
    /// Low value.
    low: i32,
    /// High value.
    high: i32,
}

/// A numeric value.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = nums)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Num {
    /// Number ID.
    id: i32,
    /// Numeric value.
    val: i32,
}

/// A department.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = departments)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Department {
    /// Department ID.
    id: i32,
    /// Department name.
    name: String,
}

/// An employee.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = employees)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Employee {
    /// Employee ID.
    id: i32,
    /// Department name.
    dept_name: String,
    /// Employee name.
    name: String,
}

/// An order.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Order {
    /// Order ID.
    id: i32,
    /// Customer ID.
    customer_id: i32,
    /// Order amount.
    amount: i32,
}

/// A customer.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = customers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Customer {
    /// Customer ID.
    id: i32,
    /// Customer name.
    name: String,
}

#[test]
fn test_join_ilike_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE posts (
            id SERIAL PRIMARY KEY,
            author_name TEXT NOT NULL,
            title TEXT NOT NULL
        );
        SELECT u.id, p.title
        FROM users u
        JOIN posts p ON p.author_name ILIKE u.name;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain LIKE, not ILIKE
    assert!(
        select_stmt.to_uppercase().contains(" LIKE "),
        "ILIKE in JOIN should translate to LIKE, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("ILIKE"),
        "Should not contain ILIKE, got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_join_now_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            event_time TIMESTAMP NOT NULL
        );
        CREATE TABLE schedules (
            id SERIAL PRIMARY KEY,
            start_time TIMESTAMP NOT NULL,
            end_time TIMESTAMP NOT NULL
        );
        SELECT e.name, s.id
        FROM events e
        JOIN schedules s ON e.event_time > NOW();
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain datetime('now'), not NOW()
    assert!(
        select_stmt.contains("datetime('now')"),
        "NOW() in JOIN should translate to datetime('now'), got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("NOW()"),
        "Should not contain NOW(), got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_join_least_greatest_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE ranges (
            id SERIAL PRIMARY KEY,
            range_start INTEGER NOT NULL,
            range_end INTEGER NOT NULL
        );
        CREATE TABLE values_table (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT r.id, v.value
        FROM ranges r
        JOIN values_table v ON v.value >= LEAST(r.range_start, r.range_end)
                            AND v.value <= GREATEST(r.range_start, r.range_end);
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain MIN/MAX, not LEAST/GREATEST
    assert!(
        select_stmt.to_uppercase().contains("MIN("),
        "LEAST in JOIN should translate to MIN, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("MAX("),
        "GREATEST in JOIN should translate to MAX, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("LEAST"),
        "Should not contain LEAST, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("GREATEST"),
        "Should not contain GREATEST, got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_join_subquery_function_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE categories (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            category_id INTEGER NOT NULL,
            tags TEXT NOT NULL
        );
        SELECT c.name, sub.all_tags
        FROM categories c
        JOIN (
            SELECT category_id, string_agg(tags, ', ') as all_tags
            FROM items
            GROUP BY category_id
        ) sub ON sub.category_id = c.id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain group_concat, not string_agg
    assert!(
        select_stmt.to_lowercase().contains("group_concat"),
        "string_agg in JOIN subquery should translate to group_concat, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_lowercase().contains("string_agg"),
        "Should not contain string_agg, got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_multiple_joins_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TIMESTAMP NOT NULL
        );
        CREATE TABLE posts (
            id SERIAL PRIMARY KEY,
            author_name TEXT NOT NULL,
            title TEXT NOT NULL
        );
        CREATE TABLE comments (
            id SERIAL PRIMARY KEY,
            post_id INTEGER NOT NULL,
            commenter_name TEXT NOT NULL
        );
        SELECT u.id, p.title, c.id as comment_id
        FROM users u
        JOIN posts p ON p.author_name ILIKE u.name
        JOIN comments c ON c.commenter_name ILIKE u.name AND c.post_id = p.id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Count LIKE occurrences (should be 2, translated from ILIKE)
    let like_count = select_stmt.to_uppercase().matches(" LIKE ").count();
    assert_eq!(
        like_count, 2,
        "Should have 2 LIKE clauses (from ILIKE), got {like_count} in: {select_stmt}"
    );

    // Should not contain any ILIKE
    assert!(
        !select_stmt.to_uppercase().contains("ILIKE"),
        "Should not contain ILIKE, got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_left_join_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE posts (
            id SERIAL PRIMARY KEY,
            author_name TEXT NOT NULL,
            title TEXT NOT NULL
        );
        SELECT u.id, p.title
        FROM users u
        LEFT JOIN posts p ON p.author_name ILIKE u.name;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should preserve LEFT JOIN and translate ILIKE
    assert!(
        select_stmt.to_uppercase().contains("LEFT JOIN"),
        "Should preserve LEFT JOIN, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains(" LIKE "),
        "ILIKE should translate to LIKE, got: {select_stmt}"
    );
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_)))
        {
            conn.execute_batch(&format!("{stmt};")).unwrap();
        }
        conn.prepare(&select_stmt).unwrap();
    }

    Ok(())
}

#[test]
fn test_join_ilike_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE authors (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE books (
            id INTEGER PRIMARY KEY,
            author_name TEXT NOT NULL,
            title TEXT NOT NULL
        );
        SELECT a.id, b.title
        FROM authors a
        JOIN books b ON b.author_name ILIKE a.name;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM - author with lowercase name
    diesel::insert_into(authors::table)
        .values(&Author { id: 1, name: "alice".to_string() })
        .execute(&mut connection)?;
    // Book with uppercase author name should match via case-insensitive LIKE
    diesel::insert_into(books::table)
        .values(&Book { id: 1, author_name: "ALICE".to_string(), title: "Book One".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(books::table)
        .values(&Book { id: 2, author_name: "Bob".to_string(), title: "Book Two".to_string() })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct JoinResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        title: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<JoinResult>(&mut connection)?;

    // Should match 'alice' with 'ALICE' (case-insensitive)
    assert_eq!(results.len(), 1, "Should match 1 row via case-insensitive join");
    assert_eq!(results[0].title, "Book One");

    Ok(())
}

#[test]
fn test_join_least_greatest_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE ranges (
            id INTEGER PRIMARY KEY,
            low INTEGER NOT NULL,
            high INTEGER NOT NULL
        );
        CREATE TABLE nums (
            id INTEGER PRIMARY KEY,
            val INTEGER NOT NULL
        );
        SELECT r.id as range_id, n.val
        FROM ranges r
        JOIN nums n ON n.val >= LEAST(r.low, r.high) AND n.val <= GREATEST(r.low, r.high);
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Range where low > high (to test that LEAST/GREATEST work correctly) using
    // Diesel ORM
    diesel::insert_into(ranges::table)
        .values(&Range { id: 1, low: 10, high: 5 })
        .execute(&mut connection)?;
    // Values: 3 is out, 5 is in, 7 is in, 10 is in, 12 is out
    diesel::insert_into(nums::table).values(&Num { id: 1, val: 3 }).execute(&mut connection)?;
    diesel::insert_into(nums::table).values(&Num { id: 2, val: 5 }).execute(&mut connection)?;
    diesel::insert_into(nums::table).values(&Num { id: 3, val: 7 }).execute(&mut connection)?;
    diesel::insert_into(nums::table).values(&Num { id: 4, val: 10 }).execute(&mut connection)?;
    diesel::insert_into(nums::table).values(&Num { id: 5, val: 12 }).execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct RangeResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        range_id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        val: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<RangeResult>(&mut connection)?;

    // Should match values 5, 7, 10 (within range [5, 10])
    assert_eq!(results.len(), 3, "Should match 3 values in range [5,10], got: {:?}", results);
    let vals: Vec<i32> = results.iter().map(|r| r.val).collect();
    assert!(vals.contains(&5));
    assert!(vals.contains(&7));
    assert!(vals.contains(&10));

    Ok(())
}

#[test]
fn test_left_join_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE departments (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            dept_name TEXT NOT NULL,
            name TEXT NOT NULL
        );
        SELECT d.name as dept, e.name as emp
        FROM departments d
        LEFT JOIN employees e ON e.dept_name ILIKE d.name;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Two departments using Diesel ORM
    diesel::insert_into(departments::table)
        .values(&Department { id: 1, name: "engineering".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(departments::table)
        .values(&Department { id: 2, name: "sales".to_string() })
        .execute(&mut connection)?;

    // Only one employee in engineering (case mismatch to test ILIKE
    // translation)
    diesel::insert_into(employees::table)
        .values(&Employee {
            id: 1,
            dept_name: "ENGINEERING".to_string(),
            name: "Alice".to_string(),
        })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct DeptEmp {
        #[diesel(sql_type = diesel::sql_types::Text)]
        dept: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        emp: Option<String>,
    }

    let results = diesel::sql_query(&select_stmt).load::<DeptEmp>(&mut connection)?;

    // Should have 2 rows: engineering with Alice, sales with NULL
    assert_eq!(results.len(), 2, "Should have 2 rows for LEFT JOIN");

    let eng = results.iter().find(|r| r.dept == "engineering").expect("Should have engineering");
    assert_eq!(eng.emp, Some("Alice".to_string()));

    let sales = results.iter().find(|r| r.dept == "sales").expect("Should have sales");
    assert_eq!(sales.emp, None);

    Ok(())
}

#[test]
fn test_join_derived_table_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            customer_id INTEGER NOT NULL,
            amount INTEGER NOT NULL
        );
        CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT c.name, totals.total_amount
        FROM customers c
        JOIN (
            SELECT customer_id, GREATEST(SUM(amount), 0) as total_amount
            FROM orders
            GROUP BY customer_id
        ) totals ON totals.customer_id = c.id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Setup data using Diesel ORM
    diesel::insert_into(customers::table)
        .values(&Customer { id: 1, name: "Alice".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(customers::table)
        .values(&Customer { id: 2, name: "Bob".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 1, customer_id: 1, amount: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 2, customer_id: 1, amount: 50 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 3, customer_id: 2, amount: 200 })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct CustomerTotal {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        total_amount: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<CustomerTotal>(&mut connection)?;

    assert_eq!(results.len(), 2);
    let alice = results.iter().find(|r| r.name == "Alice").expect("Should have Alice");
    assert_eq!(alice.total_amount, 150);
    let bob = results.iter().find(|r| r.name == "Bob").expect("Should have Bob");
    assert_eq!(bob.total_amount, 200);

    Ok(())
}
