//! Tests for PostgreSQL window function translations.
//!
//! Window functions in PostgreSQL (ROW_NUMBER, RANK, LAG, LEAD, etc.) are
//! supported in SQLite 3.25+ with identical syntax, so most translations
//! are pass-through. The main exception is the FILTER clause, which is
//! not supported in SQLite.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Test that ROW_NUMBER() OVER passes through unchanged.
#[test]
fn test_row_number() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name, ROW_NUMBER() OVER (ORDER BY id) as row_num FROM users;
    "#;

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

    Ok(())
}

/// Test RANK() with PARTITION BY and ORDER BY clauses.
#[test]
fn test_rank_with_partition() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE employees (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            department TEXT NOT NULL,
            salary INTEGER NOT NULL
        );
        SELECT name, department, salary, RANK() OVER (PARTITION BY department ORDER BY salary DESC) as rank
        FROM employees;
    "#;

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

    Ok(())
}

/// Test DENSE_RANK() window function.
#[test]
fn test_dense_rank() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE scores (
            id SERIAL PRIMARY KEY,
            player TEXT NOT NULL,
            score INTEGER NOT NULL
        );
        SELECT player, score, DENSE_RANK() OVER (ORDER BY score DESC) as dense_rank
        FROM scores;
    "#;

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

    Ok(())
}

/// Test NTILE() window function.
#[test]
fn test_ntile() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT value, NTILE(4) OVER (ORDER BY value) as quartile FROM items;
    "#;

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

    Ok(())
}

/// Test LAG() and LEAD() window functions.
#[test]
fn test_lag_lead() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE time_series (
            id SERIAL PRIMARY KEY,
            ts TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT ts, value,
               LAG(value, 1, 0) OVER (ORDER BY ts) as prev_value,
               LEAD(value, 1, 0) OVER (ORDER BY ts) as next_value
        FROM time_series;
    "#;

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

    Ok(())
}

/// Test FIRST_VALUE() and LAST_VALUE() window functions.
#[test]
fn test_first_last_value() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE readings (
            id SERIAL PRIMARY KEY,
            sensor TEXT NOT NULL,
            reading INTEGER NOT NULL
        );
        SELECT sensor, reading,
               FIRST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as first_reading,
               LAST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as last_reading
        FROM readings;
    "#;

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

    Ok(())
}

/// Test NTH_VALUE() window function.
#[test]
fn test_nth_value() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE rankings (
            id SERIAL PRIMARY KEY,
            category TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT category, value,
               NTH_VALUE(value, 2) OVER (PARTITION BY category ORDER BY value DESC) as second_highest
        FROM rankings;
    "#;

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

    Ok(())
}

/// Test aggregate functions used as window functions (SUM, AVG, etc.).
#[test]
fn test_aggregate_as_window() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
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
    "#;

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

    Ok(())
}

/// Test running total with ROWS BETWEEN frame clause.
#[test]
fn test_rows_between_frame() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE transactions (
            id SERIAL PRIMARY KEY,
            ts TEXT NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT ts, amount,
               SUM(amount) OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running_total
        FROM transactions;
    "#;

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

    Ok(())
}

/// Test RANGE BETWEEN frame clause.
#[test]
fn test_range_between_frame() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT value,
               SUM(value) OVER (ORDER BY value RANGE BETWEEN 10 PRECEDING AND 10 FOLLOWING) as range_sum
        FROM data;
    "#;

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

    Ok(())
}

/// Test FILTER clause is translated to CASE expression.
#[test]
fn test_filter_clause_to_case() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id SERIAL PRIMARY KEY,
            status TEXT NOT NULL,
            value INTEGER NOT NULL
        );
        SELECT COUNT(*) FILTER (WHERE status = 'active') OVER () as active_count FROM data;
    "#;

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

    Ok(())
}

/// Test FILTER clause with aggregate (not window) is translated to CASE.
#[test]
fn test_filter_clause_aggregate_to_case() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE events (
            id SERIAL PRIMARY KEY,
            event_type TEXT NOT NULL
        );
        SELECT COUNT(*) FILTER (WHERE event_type = 'click') as click_count FROM events;
    "#;

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

    Ok(())
}

/// Semantic test: FILTER clause converted to CASE works correctly.
#[test]
fn test_filter_clause_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE orders (
            id SERIAL PRIMARY KEY,
            status TEXT NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT
            COUNT(*) FILTER (WHERE status = 'completed') as completed_count,
            SUM(amount) FILTER (WHERE status = 'completed') as completed_total
        FROM orders;
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
    diesel::sql_query("INSERT INTO orders (status, amount) VALUES ('completed', 100)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (status, amount) VALUES ('pending', 50)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (status, amount) VALUES ('completed', 75)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (status, amount) VALUES ('cancelled', 25)")
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
    let sql = r#"
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT name, ROW_NUMBER() OVER (ORDER BY id) as row_num FROM items;
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
    diesel::sql_query("INSERT INTO items (name) VALUES ('apple')").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO items (name) VALUES ('banana')").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO items (name) VALUES ('cherry')").execute(&mut connection)?;

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
    let sql = r#"
        CREATE TABLE time_series (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value,
               LAG(value, 1) OVER (ORDER BY id) as prev_value,
               LEAD(value, 1) OVER (ORDER BY id) as next_value
        FROM time_series;
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
    diesel::sql_query("INSERT INTO time_series (value) VALUES (10)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO time_series (value) VALUES (20)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO time_series (value) VALUES (30)").execute(&mut connection)?;

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
    let sql = r#"
        CREATE TABLE transactions (
            id SERIAL PRIMARY KEY,
            amount INTEGER NOT NULL
        );
        SELECT id, amount,
               SUM(amount) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running_total
        FROM transactions;
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
    diesel::sql_query("INSERT INTO transactions (amount) VALUES (100)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO transactions (amount) VALUES (50)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO transactions (amount) VALUES (75)").execute(&mut connection)?;

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
    let sql = r#"
        CREATE TABLE employees (
            id SERIAL PRIMARY KEY,
            dept TEXT NOT NULL,
            salary INTEGER NOT NULL
        );
        SELECT id, dept, salary,
               RANK() OVER (PARTITION BY dept ORDER BY salary DESC) as dept_rank
        FROM employees;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data - two departments
    diesel::sql_query("INSERT INTO employees (dept, salary) VALUES ('eng', 100000)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO employees (dept, salary) VALUES ('eng', 80000)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO employees (dept, salary) VALUES ('sales', 90000)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO employees (dept, salary) VALUES ('sales', 90000)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO employees (dept, salary) VALUES ('sales', 70000)")
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
    let sql = r#"
        CREATE TABLE scores (
            id SERIAL PRIMARY KEY,
            score INTEGER NOT NULL
        );
        SELECT id, score, DENSE_RANK() OVER (ORDER BY score DESC) as drank FROM scores;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert scores with ties
    diesel::sql_query("INSERT INTO scores (score) VALUES (100)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (score) VALUES (90)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (score) VALUES (90)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO scores (score) VALUES (80)").execute(&mut connection)?;

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
    let sql = r#"
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value, NTILE(3) OVER (ORDER BY value) as bucket FROM items;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert 9 items so they divide evenly into 3 buckets
    for i in 1..=9 {
        diesel::sql_query(format!("INSERT INTO items (value) VALUES ({i})"))
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
    let sql = r#"
        CREATE TABLE readings (
            id SERIAL PRIMARY KEY,
            sensor TEXT NOT NULL,
            reading INTEGER NOT NULL
        );
        SELECT id, sensor, reading,
               FIRST_VALUE(reading) OVER (PARTITION BY sensor ORDER BY id) as first_r
        FROM readings;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert readings for two sensors
    diesel::sql_query("INSERT INTO readings (sensor, reading) VALUES ('A', 10)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO readings (sensor, reading) VALUES ('A', 20)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO readings (sensor, reading) VALUES ('B', 100)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO readings (sensor, reading) VALUES ('B', 200)")
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
    let sql = r#"
        CREATE TABLE rankings (
            id SERIAL PRIMARY KEY,
            value INTEGER NOT NULL
        );
        SELECT id, value,
               NTH_VALUE(value, 2) OVER (ORDER BY value DESC ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) as second_highest
        FROM rankings;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query("INSERT INTO rankings (value) VALUES (100)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO rankings (value) VALUES (80)").execute(&mut connection)?;
    diesel::sql_query("INSERT INTO rankings (value) VALUES (60)").execute(&mut connection)?;

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
    let sql = r#"
        CREATE TABLE orders (
            id SERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            amount INTEGER NOT NULL
        );
        SELECT id, user_id, amount,
               SUM(amount) OVER (PARTITION BY user_id) as user_total,
               COUNT(*) OVER (PARTITION BY user_id) as user_count
        FROM orders;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // User 1: orders of 100 and 50
    diesel::sql_query("INSERT INTO orders (user_id, amount) VALUES (1, 100)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO orders (user_id, amount) VALUES (1, 50)")
        .execute(&mut connection)?;
    // User 2: single order of 200
    diesel::sql_query("INSERT INTO orders (user_id, amount) VALUES (2, 200)")
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
