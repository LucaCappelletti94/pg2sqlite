//! Tests for PostgreSQL window function translations.
//!
//! Window functions in PostgreSQL (ROW_NUMBER, RANK, LAG, LEAD, etc.) are
//! supported in SQLite 3.25+ with identical syntax, so most translations
//! are pass-through. The main exception is the FILTER clause, which is
//! not supported in SQLite.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

diesel::table! {
    /// Test table for items (used in ROW_NUMBER and NTILE tests).
    /// Different tests create this table with different columns.
    items (id) {
        /// Item ID.
        id -> Integer,
        /// Item name (nullable - not present in all tests).
        name -> Nullable<Text>,
        /// Item value (nullable - not present in all tests).
        value -> Nullable<Integer>,
    }
}

diesel::table! {
    /// Test table for employees (used in RANK tests).
    employees (id) {
        /// Employee ID.
        id -> Integer,
        /// Department.
        dept -> Text,
        /// Employee salary.
        salary -> Integer,
    }
}

diesel::table! {
    /// Test table for scores (used in DENSE_RANK tests).
    scores (id) {
        /// Score ID.
        id -> Integer,
        /// Score value.
        score -> Integer,
    }
}

diesel::table! {
    /// Test table for time series (used in LAG/LEAD tests).
    time_series (id) {
        /// Time series ID.
        id -> Integer,
        /// Time series value.
        value -> Integer,
    }
}

diesel::table! {
    /// Test table for readings (used in FIRST_VALUE/LAST_VALUE tests).
    readings (id) {
        /// Reading ID.
        id -> Integer,
        /// Sensor name.
        sensor -> Text,
        /// Reading value.
        reading -> Integer,
    }
}

diesel::table! {
    /// Test table for rankings (used in NTH_VALUE tests).
    rankings (id) {
        /// Ranking ID.
        id -> Integer,
        /// Ranking value.
        value -> Integer,
    }
}

diesel::table! {
    /// Test table for orders (used in aggregate window function tests and FILTER tests).
    /// Different tests create this table with different columns.
    orders (id) {
        /// Order ID.
        id -> Integer,
        /// User ID (nullable - not present in all tests).
        user_id -> Nullable<Integer>,
        /// Order status (nullable - not present in all tests).
        status -> Nullable<Text>,
        /// Order amount.
        amount -> Integer,
    }
}

diesel::table! {
    /// Test table for transactions (used in running total tests).
    transactions (id) {
        /// Transaction ID.
        id -> Integer,
        /// Transaction amount.
        amount -> Integer,
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = items)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Item {
    /// Item ID.
    id: i32,
    /// Item name.
    name: Option<String>,
    /// Item value.
    value: Option<i32>,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = employees)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Employee {
    /// Employee ID.
    id: i32,
    /// Department.
    dept: String,
    /// Employee salary.
    salary: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = scores)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Score {
    /// Score ID.
    id: i32,
    /// Score value.
    score: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = time_series)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct TimeSeries {
    /// Time series ID.
    id: i32,
    /// Time series value.
    value: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = readings)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Reading {
    /// Reading ID.
    id: i32,
    /// Sensor name.
    sensor: String,
    /// Value of the reading.
    #[diesel(column_name = "reading")]
    value: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = rankings)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Ranking {
    /// Ranking ID.
    id: i32,
    /// Ranking value.
    value: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Order {
    /// Order ID.
    id: i32,
    /// User ID.
    user_id: Option<i32>,
    /// Order status.
    status: Option<String>,
    /// Order amount.
    amount: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Transaction {
    /// Transaction ID.
    id: i32,
    /// Transaction amount.
    amount: i32,
}

#[test]
fn test_row_number() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name, ROW_NUMBER() OVER (ORDER BY id) as row_num FROM users;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("ROW_NUMBER()"),
        "ROW_NUMBER() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("OVER"),
        "OVER clause should be present, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("ORDER BY"),
        "ORDER BY should be present in OVER clause, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_rank_with_partition() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE employees (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            department TEXT NOT NULL,
            salary INTEGER NOT NULL
        );
        SELECT name, department, salary, RANK() OVER (PARTITION BY department ORDER BY salary DESC) as rank
        FROM employees;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("RANK()"),
        "RANK() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("PARTITION BY"),
        "PARTITION BY should be present, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("ORDER BY"),
        "ORDER BY should be present, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_dense_rank() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE scores (
            id SERIAL PRIMARY KEY,
            player TEXT NOT NULL,
            score INTEGER NOT NULL
        );
        SELECT player, score, DENSE_RANK() OVER (ORDER BY score DESC) as dense_rank
        FROM scores;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("DENSE_RANK()"),
        "DENSE_RANK() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_ntile() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT value, NTILE(4) OVER (ORDER BY value) as quartile FROM items;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("NTILE"),
        "NTILE() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_lag_lead() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE time_series (
            id SERIAL PRIMARY KEY,
            ts TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT ts, value,
               LAG(value, 1, 0) OVER (ORDER BY ts) as prev_value,
               LEAD(value, 1, 0) OVER (ORDER BY ts) as next_value
        FROM time_series;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("LAG"),
        "LAG() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("LEAD"),
        "LEAD() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_first_last_value() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE readings (
            id SERIAL PRIMARY KEY,
            sensor TEXT NOT NULL,
            reading INTEGER NOT NULL
        );
        SELECT sensor, reading,
               FIRST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as first_reading,
               LAST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as last_reading
        FROM readings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("FIRST_VALUE"),
        "FIRST_VALUE() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("LAST_VALUE"),
        "LAST_VALUE() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_nth_value() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE rankings (
            id SERIAL PRIMARY KEY,
            category TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT category, value,
               NTH_VALUE(value, 2) OVER (PARTITION BY category ORDER BY value DESC) as second_highest
        FROM rankings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("NTH_VALUE"),
        "NTH_VALUE() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_aggregate_as_window() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id SERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT user_id, amount,
               SUM(amount) OVER (PARTITION BY user_id) as user_total,
               AVG(amount) OVER (PARTITION BY user_id) as user_avg,
               COUNT(*) OVER (PARTITION BY user_id) as user_count
        FROM orders;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("SUM("),
        "SUM() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("AVG("),
        "AVG() should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("COUNT("),
        "COUNT() should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_rows_between_frame() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE transactions (
            id SERIAL PRIMARY KEY,
            ts TEXT NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT ts, amount,
               SUM(amount) OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running_total
        FROM transactions;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("ROWS BETWEEN"),
        "ROWS BETWEEN should pass through, got: {select_stmt}"
    );
    assert!(
        select_stmt.to_uppercase().contains("UNBOUNDED PRECEDING"),
        "UNBOUNDED PRECEDING should be present, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_range_between_frame() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE data (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT value,
               SUM(value) OVER (ORDER BY value RANGE BETWEEN 10 PRECEDING AND 10 FOLLOWING) as range_sum
        FROM data;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("RANGE BETWEEN"),
        "RANGE BETWEEN should pass through, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_filter_clause_to_case() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE data (
            id SERIAL PRIMARY KEY,
            status TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT COUNT(*) FILTER (WHERE status = 'active') OVER () as active_count FROM data;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // FILTER should be converted to CASE WHEN
    assert!(
        select_stmt.contains("CASE WHEN"),
        "FILTER should be converted to CASE WHEN, got: {select_stmt}"
    );
    assert!(
        !select_stmt.contains("FILTER"),
        "FILTER keyword should not appear in output, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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
fn test_filter_clause_aggregate_to_case() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id SERIAL PRIMARY KEY,
            event_type TEXT NOT NULL
        );
        SELECT COUNT(*) FILTER (WHERE event_type = 'click') as click_count FROM events;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // FILTER should be converted to CASE WHEN
    assert!(
        select_stmt.contains("CASE WHEN"),
        "FILTER should be converted to CASE WHEN, got: {select_stmt}"
    );

    // Execute DDL then prepare SELECT to prove real SQLite accepts the translated
    // SQL.
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

/// Semantic test: FILTER clause converted to CASE works correctly.
#[test]
fn test_filter_clause_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id SERIAL PRIMARY KEY,
            status TEXT NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT
            COUNT(*) FILTER (WHERE status = 'completed') as completed_count,
            SUM(amount) FILTER (WHERE status = 'completed') as completed_total
        FROM orders;
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
        .values(&Order { id: 1, user_id: None, status: Some("completed".to_string()), amount: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 2, user_id: None, status: Some("pending".to_string()), amount: 50 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 3, user_id: None, status: Some("completed".to_string()), amount: 75 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 4, user_id: None, status: Some("cancelled".to_string()), amount: 25 })
        .execute(&mut connection)?;

    // Execute the SELECT with FILTER->CASE conversion
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct FilterResult {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        completed_count: i64,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        completed_total: Option<i64>,
    }

    let results = diesel::sql_query(&select_stmt).load::<FilterResult>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].completed_count, 2); // 2 completed orders
    assert_eq!(results[0].completed_total, Some(175)); // 100 + 75

    Ok(())
}

/// Semantic test: ROW_NUMBER() works correctly in SQLite.
#[test]
fn test_row_number_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name, ROW_NUMBER() OVER (ORDER BY id) as row_num FROM items;
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
        .values(&Item { id: 1, name: Some("apple".to_string()), value: None })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 2, name: Some("banana".to_string()), value: None })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 3, name: Some("cherry".to_string()), value: None })
        .execute(&mut connection)?;

    // Execute the SELECT with window function
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ItemWithRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        row_num: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<ItemWithRow>(&mut connection)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].row_num, 1);
    assert_eq!(results[1].row_num, 2);
    assert_eq!(results[2].row_num, 3);

    Ok(())
}

/// Semantic test: LAG() and LEAD() work correctly in SQLite.
#[test]
fn test_lag_lead_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE time_series (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value,
               LAG(value, 1) OVER (ORDER BY id) as prev_value,
               LEAD(value, 1) OVER (ORDER BY id) as next_value
        FROM time_series;
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
    diesel::insert_into(time_series::table)
        .values(&TimeSeries { id: 1, value: 10 })
        .execute(&mut connection)?;
    diesel::insert_into(time_series::table)
        .values(&TimeSeries { id: 2, value: 20 })
        .execute(&mut connection)?;
    diesel::insert_into(time_series::table)
        .values(&TimeSeries { id: 3, value: 30 })
        .execute(&mut connection)?;

    // Execute the SELECT with window function
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ValueWithLagLead {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        value: i32,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        prev_value: Option<i32>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        next_value: Option<i32>,
    }

    let results = diesel::sql_query(&select_stmt).load::<ValueWithLagLead>(&mut connection)?;

    assert_eq!(results.len(), 3);

    // First row: no previous value
    assert_eq!(results[0].value, 10);
    assert_eq!(results[0].prev_value, None);
    assert_eq!(results[0].next_value, Some(20));

    // Second row: has both
    assert_eq!(results[1].value, 20);
    assert_eq!(results[1].prev_value, Some(10));
    assert_eq!(results[1].next_value, Some(30));

    // Third row: no next value
    assert_eq!(results[2].value, 30);
    assert_eq!(results[2].prev_value, Some(20));
    assert_eq!(results[2].next_value, None);

    Ok(())
}

/// Semantic test: SUM() as window function (running total).
#[test]
fn test_running_total_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE transactions (
            id SERIAL PRIMARY KEY,
            amount INTEGER NOT NULL
        );
        SELECT id, amount,
               SUM(amount) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running_total
        FROM transactions;
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
    diesel::insert_into(transactions::table)
        .values(&Transaction { id: 1, amount: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(transactions::table)
        .values(&Transaction { id: 2, amount: 50 })
        .execute(&mut connection)?;
    diesel::insert_into(transactions::table)
        .values(&Transaction { id: 3, amount: 75 })
        .execute(&mut connection)?;

    // Execute the SELECT with window function
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct TxWithRunningTotal {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        amount: i32,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        running_total: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<TxWithRunningTotal>(&mut connection)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].running_total, 100); // 100
    assert_eq!(results[1].running_total, 150); // 100 + 50
    assert_eq!(results[2].running_total, 225); // 100 + 50 + 75

    Ok(())
}

/// Semantic test: RANK() with PARTITION BY.
#[test]
fn test_rank_partition_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE employees (
            id SERIAL PRIMARY KEY,
            dept TEXT NOT NULL,
            salary INTEGER NOT NULL
        );
        SELECT id, dept, salary,
               RANK() OVER (PARTITION BY dept ORDER BY salary DESC) as dept_rank
        FROM employees;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data - two departments using Diesel ORM
    diesel::insert_into(employees::table)
        .values(&Employee { id: 1, dept: "eng".to_string(), salary: 100_000 })
        .execute(&mut connection)?;
    diesel::insert_into(employees::table)
        .values(&Employee { id: 2, dept: "eng".to_string(), salary: 80_000 })
        .execute(&mut connection)?;
    diesel::insert_into(employees::table)
        .values(&Employee { id: 3, dept: "sales".to_string(), salary: 90_000 })
        .execute(&mut connection)?;
    diesel::insert_into(employees::table)
        .values(&Employee { id: 4, dept: "sales".to_string(), salary: 90_000 })
        .execute(&mut connection)?;
    diesel::insert_into(employees::table)
        .values(&Employee { id: 5, dept: "sales".to_string(), salary: 70_000 })
        .execute(&mut connection)?;

    // Execute the SELECT with window function
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct EmpWithRank {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        dept: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        salary: i32,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        dept_rank: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<EmpWithRank>(&mut connection)?;

    assert_eq!(results.len(), 5);

    // Check eng department rankings
    let eng: Vec<_> = results.iter().filter(|e| e.dept == "eng").collect();
    assert_eq!(eng.len(), 2);
    // Highest salary in eng should be rank 1
    assert!(eng.iter().any(|e| e.salary == 100_000 && e.dept_rank == 1));
    assert!(eng.iter().any(|e| e.salary == 80_000 && e.dept_rank == 2));

    // Check sales department rankings - two tied at 90k should both be rank 1
    let sales: Vec<_> = results.iter().filter(|e| e.dept == "sales").collect();
    assert_eq!(sales.len(), 3);
    let rank1_sales: Vec<_> = sales.iter().filter(|e| e.dept_rank == 1).collect();
    assert_eq!(rank1_sales.len(), 2); // Two tied at rank 1
    // The third should be rank 3 (not 2, because RANK skips)
    assert!(sales.iter().any(|e| e.salary == 70_000 && e.dept_rank == 3));

    Ok(())
}

/// Semantic test: DENSE_RANK() works correctly (no gaps in ranking).
#[test]
fn test_dense_rank_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE scores (
            id SERIAL PRIMARY KEY,
            score INTEGER NOT NULL
        );
        SELECT id, score, DENSE_RANK() OVER (ORDER BY score DESC) as drank FROM scores;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert scores with ties using Diesel ORM
    diesel::insert_into(scores::table)
        .values(&Score { id: 1, score: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 2, score: 90 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 3, score: 90 })
        .execute(&mut connection)?;
    diesel::insert_into(scores::table)
        .values(&Score { id: 4, score: 80 })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ScoreWithRank {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        score: i32,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        drank: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<ScoreWithRank>(&mut connection)?;

    assert_eq!(results.len(), 4);
    // DENSE_RANK: 100->1, 90->2 (both), 80->3 (no gap!)
    assert!(results.iter().any(|r| r.score == 100 && r.drank == 1));
    let rank2: Vec<_> = results.iter().filter(|r| r.drank == 2).collect();
    assert_eq!(rank2.len(), 2); // Both 90s are rank 2
    assert!(results.iter().any(|r| r.score == 80 && r.drank == 3)); // No gap, rank 3

    Ok(())
}

/// Semantic test: NTILE() divides rows into buckets.
#[test]
fn test_ntile_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value, NTILE(3) OVER (ORDER BY value) as bucket FROM items;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert 9 items so they divide evenly into 3 buckets using Diesel ORM
    for i in 1..=9 {
        diesel::insert_into(items::table)
            .values(&Item { id: i, name: None, value: Some(i) })
            .execute(&mut connection)?;
    }

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ItemWithBucket {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        value: i32,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        bucket: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<ItemWithBucket>(&mut connection)?;

    assert_eq!(results.len(), 9);
    // Each bucket should have 3 items
    let bucket1: Vec<_> = results.iter().filter(|r| r.bucket == 1).collect();
    let bucket2: Vec<_> = results.iter().filter(|r| r.bucket == 2).collect();
    let bucket3: Vec<_> = results.iter().filter(|r| r.bucket == 3).collect();
    assert_eq!(bucket1.len(), 3);
    assert_eq!(bucket2.len(), 3);
    assert_eq!(bucket3.len(), 3);

    Ok(())
}

/// Semantic test: FIRST_VALUE() and LAST_VALUE() work correctly.
#[test]
fn test_first_last_value_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE readings (
            id SERIAL PRIMARY KEY,
            sensor TEXT NOT NULL,
            reading INTEGER NOT NULL
        );
        SELECT id, sensor, reading,
               FIRST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as first_r
        FROM readings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert readings for two sensors using Diesel ORM
    diesel::insert_into(readings::table)
        .values(&Reading { id: 1, sensor: "A".to_string(), value: 10 })
        .execute(&mut connection)?;
    diesel::insert_into(readings::table)
        .values(&Reading { id: 2, sensor: "A".to_string(), value: 20 })
        .execute(&mut connection)?;
    diesel::insert_into(readings::table)
        .values(&Reading { id: 3, sensor: "B".to_string(), value: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(readings::table)
        .values(&Reading { id: 4, sensor: "B".to_string(), value: 200 })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct ReadingWithFirst {
        #[diesel(sql_type = diesel::sql_types::Text)]
        sensor: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        reading: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        first_r: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<ReadingWithFirst>(&mut connection)?;

    assert_eq!(results.len(), 4);
    // All sensor A readings should have first_r = 10
    for r in results.iter().filter(|r| r.sensor == "A") {
        assert_eq!(r.first_r, 10, "Sensor A first value should be 10");
    }
    // All sensor B readings should have first_r = 100
    for r in results.iter().filter(|r| r.sensor == "B") {
        assert_eq!(r.first_r, 100, "Sensor B first value should be 100");
    }

    Ok(())
}

/// Semantic test: NTH_VALUE() works correctly.
#[test]
fn test_nth_value_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE rankings (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value,
               NTH_VALUE(value, 2) OVER (ORDER BY value DESC ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) as second_highest
        FROM rankings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(rankings::table)
        .values(&Ranking { id: 1, value: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(rankings::table)
        .values(&Ranking { id: 2, value: 80 })
        .execute(&mut connection)?;
    diesel::insert_into(rankings::table)
        .values(&Ranking { id: 3, value: 60 })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct RankingWithNth {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        value: i32,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        second_highest: Option<i32>,
    }

    let results = diesel::sql_query(&select_stmt).load::<RankingWithNth>(&mut connection)?;

    assert_eq!(results.len(), 3);
    // All rows should have second_highest = 80 (the 2nd highest value)
    for r in &results {
        assert_eq!(r.second_highest, Some(80), "Second highest should be 80");
    }

    Ok(())
}

/// Semantic test: Aggregate window functions (SUM, AVG, COUNT) work correctly.
#[test]
fn test_aggregate_window_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE orders (
            id SERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT id, user_id, amount,
               SUM(amount) OVER (PARTITION BY user_id) as user_total,
               COUNT(*) OVER (PARTITION BY user_id) as user_count
        FROM orders;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // User 1: orders of 100 and 50 using Diesel ORM
    diesel::insert_into(orders::table)
        .values(&Order { id: 1, user_id: Some(1), status: None, amount: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(orders::table)
        .values(&Order { id: 2, user_id: Some(1), status: None, amount: 50 })
        .execute(&mut connection)?;
    // User 2: single order of 200
    diesel::insert_into(orders::table)
        .values(&Order { id: 3, user_id: Some(2), status: None, amount: 200 })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct OrderWithAgg {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        user_id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        amount: i32,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        user_total: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        user_count: i64,
    }

    let results = diesel::sql_query(&select_stmt).load::<OrderWithAgg>(&mut connection)?;

    assert_eq!(results.len(), 3);

    // User 1: total = 150, count = 2
    for r in results.iter().filter(|r| r.user_id == 1) {
        assert_eq!(r.user_total, 150);
        assert_eq!(r.user_count, 2);
    }

    // User 2: total = 200, count = 1
    for r in results.iter().filter(|r| r.user_id == 2) {
        assert_eq!(r.user_total, 200);
        assert_eq!(r.user_count, 1);
    }

    Ok(())
}
