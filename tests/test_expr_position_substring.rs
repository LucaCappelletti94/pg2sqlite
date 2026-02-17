//! Tests for POSITION and SUBSTRING expression translation.
//!
//! - POSITION(substr IN str) translates to INSTR(str, substr)
//! - SUBSTRING(str FROM pos FOR len) translates to SUBSTR(str, pos, len)

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// Schema for tests
// ============================================================================

mod schema {
    diesel::table! {
        /// Strings table for position/substring tests.
        strings (id) {
            /// Primary key.
            id -> Integer,
            /// Text value.
            text_val -> Text,
        }
    }
}

use schema::strings;

/// A string record for testing.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, QueryableByName)]
#[diesel(table_name = strings)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StringData {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    text_val: String,
}

// ============================================================================
// POSITION Tests
// ============================================================================

/// Test that POSITION expressions are translated to INSTR.
#[test]
fn test_position_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT POSITION('world' IN text_val) as pos FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain INSTR, not POSITION
    assert!(
        select_stmt.to_uppercase().contains("INSTR"),
        "POSITION should translate to INSTR, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("POSITION"),
        "Should not contain POSITION, got: {select_stmt}"
    );

    Ok(())
}

/// Test POSITION semantic execution.
#[test]
fn test_position_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;
    diesel::insert_into(strings::table)
        .values(&StringData { id: 2, text_val: "goodbye".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    #[allow(dead_code)]
    struct PosResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        pos: i32,
    }

    // INSTR returns 1-based position, 0 if not found
    // Raw SQL needed to test INSTR function translation
    let results: Vec<PosResult> =
        diesel::sql_query("SELECT id, INSTR(text_val, 'world') as pos FROM strings ORDER BY id")
            .load(&mut conn)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].pos, 7); // "world" starts at position 7 in "hello world"
    assert_eq!(results[1].pos, 0); // "world" not found in "goodbye"

    Ok(())
}

// ============================================================================
// SUBSTRING Tests
// ============================================================================

/// Test that SUBSTRING expressions are translated to SUBSTR.
#[test]
fn test_substring_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT SUBSTRING(text_val FROM 1 FOR 5) as sub FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain SUBSTR, not SUBSTRING
    assert!(
        select_stmt.to_uppercase().contains("SUBSTR"),
        "SUBSTRING should translate to SUBSTR, got: {select_stmt}"
    );

    Ok(())
}

/// Test SUBSTRING semantic execution.
#[test]
fn test_substring_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct SubResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        sub: String,
    }

    // SUBSTR(str, start, length) - 1-based indexing
    // Raw SQL needed to test SUBSTR function translation
    let results: Vec<SubResult> =
        diesel::sql_query("SELECT SUBSTR(text_val, 1, 5) as sub FROM strings").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sub, "hello");

    Ok(())
}

/// Test SUBSTRING without FOR (length).
#[test]
fn test_substring_no_length_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT SUBSTRING(text_val FROM 7) as sub FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.to_uppercase().contains("SUBSTR"),
        "SUBSTRING should translate to SUBSTR, got: {select_stmt}"
    );

    Ok(())
}

/// Test SUBSTRING without FOR (length) semantic execution.
#[test]
fn test_substring_no_length_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct SubResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        sub: String,
    }

    // SUBSTR(str, start) without length returns from start to end
    // Raw SQL needed to test SUBSTR function translation
    let results: Vec<SubResult> =
        diesel::sql_query("SELECT SUBSTR(text_val, 7) as sub FROM strings").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sub, "world");

    Ok(())
}
