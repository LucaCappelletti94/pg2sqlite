//! Tests for string and math expression translation (TRIM, CEIL, FLOOR).
//!
//! These expressions pass through directly as SQLite supports them.

#![allow(clippy::float_cmp)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// Schema for tests
// ============================================================================

mod schema {
    diesel::table! {
        /// Data table for string/math tests.
        data (id) {
            /// Primary key.
            id -> Integer,
            /// Text value.
            text_val -> Text,
            /// Numeric value.
            num_val -> Float,
        }
    }
}

use schema::data;

/// A data record for testing.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, QueryableByName)]
#[diesel(table_name = data)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Data {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    text_val: String,
    #[diesel(sql_type = diesel::sql_types::Float)]
    num_val: f32,
}

// ============================================================================
// TRIM Tests
// ============================================================================

/// Test that TRIM expressions are translated correctly.
#[test]
fn test_trim_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
        SELECT TRIM(text_val) as trimmed FROM data;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.to_uppercase().contains("TRIM"), "Should contain TRIM, got: {select_stmt}");

    Ok(())
}

/// Test TRIM semantic execution.
#[test]
fn test_trim_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(data::table)
        .values(&Data { id: 1, text_val: "  hello  ".to_string(), num_val: 1.0 })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct TrimResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        trimmed: String,
    }

    let results: Vec<TrimResult> =
        diesel::sql_query("SELECT TRIM(text_val) as trimmed FROM data").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].trimmed, "hello");

    Ok(())
}

/// Test TRIM with LEADING/TRAILING/BOTH.
#[test]
fn test_trim_variants_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
        SELECT TRIM(LEADING ' ' FROM text_val) as ltrimmed FROM data;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.to_uppercase().contains("TRIM"), "Should contain TRIM, got: {select_stmt}");

    Ok(())
}

// ============================================================================
// CEIL Tests
// ============================================================================

/// Test that CEIL expressions are translated correctly.
#[test]
fn test_ceil_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
        SELECT CEIL(num_val) as ceiled FROM data;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // SQLite uses CEIL or CEILING
    assert!(select_stmt.to_uppercase().contains("CEIL"), "Should contain CEIL, got: {select_stmt}");

    Ok(())
}

/// Test CEIL semantic execution.
#[test]
fn test_ceil_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(data::table)
        .values(&Data { id: 1, text_val: "test".to_string(), num_val: 3.2 })
        .execute(&mut conn)?;
    diesel::insert_into(data::table)
        .values(&Data { id: 2, text_val: "test".to_string(), num_val: 3.8 })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct CeilResult {
        #[diesel(sql_type = diesel::sql_types::Float)]
        ceiled: f32,
    }

    let results: Vec<CeilResult> =
        diesel::sql_query("SELECT CEIL(num_val) as ceiled FROM data ORDER BY id")
            .load(&mut conn)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].ceiled, 4.0);
    assert_eq!(results[1].ceiled, 4.0);

    Ok(())
}

// ============================================================================
// FLOOR Tests
// ============================================================================

/// Test that FLOOR expressions are translated correctly.
#[test]
fn test_floor_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
        SELECT FLOOR(num_val) as floored FROM data;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("FLOOR"),
        "Should contain FLOOR, got: {select_stmt}"
    );

    Ok(())
}

/// Test FLOOR semantic execution.
#[test]
fn test_floor_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE data (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL,
            num_val REAL NOT NULL
        );
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(data::table)
        .values(&Data { id: 1, text_val: "test".to_string(), num_val: 3.2 })
        .execute(&mut conn)?;
    diesel::insert_into(data::table)
        .values(&Data { id: 2, text_val: "test".to_string(), num_val: 3.8 })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct FloorResult {
        #[diesel(sql_type = diesel::sql_types::Float)]
        floored: f32,
    }

    let results: Vec<FloorResult> =
        diesel::sql_query("SELECT FLOOR(num_val) as floored FROM data ORDER BY id")
            .load(&mut conn)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].floored, 3.0);
    assert_eq!(results[1].floored, 3.0);

    Ok(())
}
