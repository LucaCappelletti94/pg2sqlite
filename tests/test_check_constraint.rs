//! Tests for CHECK constraint translation — column-level and table-level.
//!
//! Covers:
//! - Column-level CHECK constraints survive translation and are enforced at
//!   runtime
//! - Table-level CHECK with unsupported functions causes translation errors
//! - `remove_unsupported_check_constraints` option silently drops constraints
//! - Nested function calls inside CHECK are handled correctly

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

/// The constraint must survive translation AND still be enforced at runtime, so
/// this executes the DDL rather than only inspecting it.
#[test]
fn test_check_constraint_is_translated() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE items (id SERIAL PRIMARY KEY, quantity INT CHECK (quantity > 0));";

    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let translated_sql = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");

    // The translated DDL must contain the CHECK clause
    assert!(
        translated_sql.contains("CHECK"),
        "CHECK constraint must appear in translated output, got: {translated_sql}"
    );

    // The CHECK must actually be enforced by SQLite at runtime
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Valid insert should succeed
    diesel::sql_query("INSERT INTO items (id, quantity) VALUES (1, 5)").execute(&mut conn)?;

    // Invalid insert must fail (quantity = -1 violates CHECK quantity > 0)
    let result =
        diesel::sql_query("INSERT INTO items (id, quantity) VALUES (2, -1)").execute(&mut conn);
    assert!(result.is_err(), "INSERT with quantity = -1 should violate CHECK constraint");

    Ok(())
}

/// A table-level CHECK whose top-level expression is an unsupported function
/// should fail translation when the option is not set.
#[test]
fn test_check_constraint_with_unsupported_function_causes_error() {
    // array_length is a PostgreSQL-specific function that doesn't exist in
    // SQLite. Using it as the top-level expression of a TABLE-LEVEL check
    // constraint (comma-separated, not inline on the column) triggers the
    // function detection.
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (array_length(col)));";
    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err(), "Expected translation error for unsupported function in CHECK");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("array_length") || error_msg.to_lowercase().contains("undefined"),
        "Error should mention the function name, got: {error_msg}"
    );
}

/// With `remove_unsupported_check_constraints()` the same constraint is
/// silently dropped.
#[test]
fn test_check_constraint_removed_with_option() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (array_length(col)));";
    let options = Pg2SqliteOptions::default().with_remove_unsupported_check_constraints();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        !sql_output.contains("CHECK"),
        "CHECK constraint should be removed when option is set, got: {sql_output}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// A simple arithmetic CHECK (no PG-specific functions) always passes through.
#[test]
fn test_valid_check_constraint_passes_through() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, price INTEGER, CHECK (price > 0));";
    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        sql_output.contains("CHECK"),
        "Valid CHECK constraint should pass through, got: {sql_output}"
    );
    assert!(
        sql_output.contains("price > 0"),
        "CHECK condition should be preserved verbatim, got: {sql_output}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// A table-level CHECK with a nested function call should fail translation
/// when removal is disabled.
#[test]
fn test_nested_function_check_constraint_causes_error() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (col IS NULL OR array_length(col) > 0));";
    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err(), "Expected translation error for nested function in CHECK");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("array_length") || error_msg.to_lowercase().contains("undefined"),
        "Error should mention the nested function name, got: {error_msg}"
    );
}

/// With `remove_unsupported_check_constraints()` nested function calls in
/// CHECK constraints are removed as well.
#[test]
fn test_nested_function_check_constraint_removed_with_option() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (col IS NULL OR array_length(col) > 0));";
    let options = Pg2SqliteOptions::default().with_remove_unsupported_check_constraints();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        !sql_output.contains("CHECK"),
        "Nested function CHECK constraint should be removed when option is set, got: {sql_output}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// Column-level CHECK constraints are now translated to SQLite CHECK
/// constraints.
#[test]
fn test_column_level_check_is_translated() {
    // Inline column CHECK (no leading comma) → ColumnOption::Check → translated
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, price INTEGER CHECK (price > 0));";
    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        sql_output.contains("CHECK"),
        "Column-level CHECK should be translated, got: {sql_output}"
    );
    assert!(
        sql_output.contains("price > 0"),
        "CHECK condition should be preserved, got: {sql_output}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// Column-level CHECK constraints are silently dropped when
/// `remove_unsupported_check_constraints` is set.
#[test]
fn test_column_level_check_is_dropped_with_option() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, price INTEGER CHECK (price > 0));";
    let options = Pg2SqliteOptions::default().with_remove_unsupported_check_constraints();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        !sql_output.contains("CHECK"),
        "Column-level CHECK should be dropped when option is set, got: {sql_output}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// Table-level CHECK with `char_length` is translated to `length`.
#[test]
fn table_check_translates_char_length() {
    let sql = "CREATE TABLE t (name TEXT, CHECK (char_length(name) > 0))";
    let translated =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    let lower = sql_output.to_lowercase();
    assert!(lower.contains("length("), "expected char_length -> length translation: {sql_output}");
    assert!(!lower.contains("char_length"), "char_length should be translated: {sql_output}");
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = translated.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script).unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// Semantic test: table-level CHECK with `char_length` rejects empty strings.
#[test]
fn table_check_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "CREATE TABLE validated (name TEXT NOT NULL, CHECK (char_length(name) > 0))";
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    // Valid insert: name has length > 0
    let result =
        diesel::sql_query("INSERT INTO validated (name) VALUES ('hello')").execute(&mut conn);
    assert!(result.is_ok(), "valid insert should succeed");

    // Invalid insert: empty string, length == 0 -> CHECK constraint should fail
    let result = diesel::sql_query("INSERT INTO validated (name) VALUES ('')").execute(&mut conn);
    assert!(result.is_err(), "empty string should violate CHECK constraint");

    Ok(())
}

#[test]
fn test_check_constraint_string_length() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE products (id SERIAL PRIMARY KEY, code TEXT CHECK (length(code) >= 3));";

    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Valid
    diesel::sql_query("INSERT INTO products (id, code) VALUES (1, 'ABC')").execute(&mut conn)?;

    // Too short — must fail
    let result =
        diesel::sql_query("INSERT INTO products (id, code) VALUES (2, 'AB')").execute(&mut conn);
    assert!(result.is_err(), "INSERT with code 'AB' (length 2) should violate CHECK");

    Ok(())
}
