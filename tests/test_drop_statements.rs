//! Tests for DROP statement translations (DROP TABLE, DROP VIEW, DROP INDEX,
//! DROP TRIGGER).

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};

mod helpers;
use helpers::Count;

// ============================================================================
// Schema Definitions
// ============================================================================

diesel::table! {
    /// Test table for users.
    users (id) {
        /// User ID.
        id -> Integer,
        /// User name.
        name -> Nullable<Text>,
    }
}

diesel::table! {
    /// Test table for users with email field.
    users_with_email (id) {
        /// User ID.
        id -> Integer,
        /// User email.
        email -> Text,
    }
}

diesel::table! {
    /// Test table for audit log.
    audit_log (id) {
        /// Log entry ID.
        id -> Integer,
        /// Log message.
        message -> Nullable<Text>,
    }
}

diesel::table! {
    /// Test table for other_table.
    other_table (id) {
        /// ID.
        id -> Integer,
    }
}

// ============================================================================
// Model Structs
// ============================================================================

/// A user record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// User ID.
    id: i32,
    /// User name.
    name: Option<String>,
}

/// A user record with email.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users_with_email)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct UserWithEmail {
    /// User ID.
    id: i32,
    /// User email.
    email: String,
}

/// An audit log entry.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = audit_log)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct AuditLogEntry {
    /// Log entry ID.
    id: i32,
    /// Log message.
    message: Option<String>,
}

/// An entry in other_table.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = other_table)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct OtherTableEntry {
    /// ID.
    id: i32,
}

/// Helper to create an empty schema for direct translation tests.
fn empty_schema() -> Result<sql_traits::structs::ParserDB, sql_traits::errors::Error> {
    sql_traits::structs::ParserDB::from_statements(vec![], "test".to_string())
}

/// Helper to translate a single statement directly without full schema
/// validation.
fn translate_statement(
    sql: &str,
) -> Result<Vec<sqlparser::ast::Statement>, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default();
    let statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)?;

    let schema = empty_schema()?;
    let mut result = Vec::new();
    for stmt in statements {
        let translated = stmt.translate(&schema, &options)?;
        result.extend(translated);
    }
    Ok(result)
}

/// Helper to create a connection and run translated SQL.
fn translate_and_execute(sql: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default();
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Helper to check if a table exists in SQLite.
fn table_exists(conn: &mut SqliteConnection, table_name: &str) -> bool {
    let result: Result<Count, _> = diesel::sql_query(format!(
        "SELECT COUNT(*) as count FROM sqlite_master WHERE type='table' AND name='{table_name}'"
    ))
    .get_result(conn);

    result.is_ok_and(|c| c.count > 0)
}

/// Helper to check if a view exists in SQLite.
fn view_exists(conn: &mut SqliteConnection, view_name: &str) -> bool {
    let result: Result<Count, _> = diesel::sql_query(format!(
        "SELECT COUNT(*) as count FROM sqlite_master WHERE type='view' AND name='{view_name}'"
    ))
    .get_result(conn);

    result.is_ok_and(|c| c.count > 0)
}

/// Helper to check if an index exists in SQLite.
fn index_exists(conn: &mut SqliteConnection, index_name: &str) -> bool {
    let result: Result<Count, _> = diesel::sql_query(format!(
        "SELECT COUNT(*) as count FROM sqlite_master WHERE type='index' AND name='{index_name}'"
    ))
    .get_result(conn);

    result.is_ok_and(|c| c.count > 0)
}

/// Helper to check if a trigger exists in SQLite.
fn trigger_exists(conn: &mut SqliteConnection, trigger_name: &str) -> bool {
    let result: Result<Count, _> = diesel::sql_query(format!(
        "SELECT COUNT(*) as count FROM sqlite_master WHERE type='trigger' AND name='{trigger_name}'"
    ))
    .get_result(conn);

    result.is_ok_and(|c| c.count > 0)
}

// ============================================================================
// DROP TABLE Tests
// ============================================================================

/// Test basic DROP TABLE translation.
#[test]
fn test_drop_table_translation() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TABLE users;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();
    assert!(drop_str.contains("DROP TABLE"), "Should contain DROP TABLE: {drop_str}");
    assert!(drop_str.contains("users"), "Should contain table name: {drop_str}");

    Ok(())
}

/// Test DROP TABLE IF EXISTS translation.
#[test]
fn test_drop_table_if_exists_translation() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TABLE IF EXISTS users;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();
    assert!(drop_str.contains("IF EXISTS"), "Should contain IF EXISTS: {drop_str}");

    Ok(())
}

/// Test DROP TABLE IF EXISTS execution when table does not exist.
#[test]
fn test_drop_table_if_exists_nonexistent() -> Result<(), Box<dyn std::error::Error>> {
    // Create a minimal setup, then execute DROP TABLE IF EXISTS
    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Create a table first using DDL (table creation is DDL, not DML)
    diesel::sql_query("CREATE TABLE other_table (id INTEGER PRIMARY KEY)")
        .execute(&mut connection)?;

    // Translate and execute DROP TABLE IF EXISTS for nonexistent table
    let translated = translate_statement("DROP TABLE IF EXISTS nonexistent_table;")?;
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Should not error and other_table should still exist
    assert!(table_exists(&mut connection, "other_table"));

    Ok(())
}

/// Test DROP TABLE CASCADE is stripped for SQLite.
#[test]
fn test_drop_table_cascade_stripped() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TABLE users CASCADE;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();
    assert!(!drop_str.contains("CASCADE"), "CASCADE should be stripped: {drop_str}");

    Ok(())
}

/// Test DROP TABLE RESTRICT is stripped for SQLite.
#[test]
fn test_drop_table_restrict_stripped() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TABLE users RESTRICT;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();
    assert!(!drop_str.contains("RESTRICT"), "RESTRICT should be stripped: {drop_str}");

    Ok(())
}

/// Test DROP TABLE execution in SQLite.
#[test]
fn test_drop_table_execution() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Create table directly in SQLite using DDL (table creation is DDL, not DML)
    diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&mut connection)?;
    assert!(table_exists(&mut connection, "users"), "Table should exist after CREATE");

    // Translate and execute DROP TABLE
    let translated = translate_statement("DROP TABLE users;")?;
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    assert!(!table_exists(&mut connection, "users"), "Table should not exist after DROP");

    Ok(())
}

// ============================================================================
// DROP VIEW Tests
// ============================================================================

/// Test basic DROP VIEW translation and execution.
#[test]
fn test_drop_view() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            active BOOLEAN NOT NULL DEFAULT true
        );
        CREATE VIEW active_users AS SELECT * FROM users WHERE active = true;
        DROP VIEW active_users;
    ";

    let mut connection = translate_and_execute(sql)?;

    assert!(table_exists(&mut connection, "users"), "Table should still exist");
    assert!(!view_exists(&mut connection, "active_users"), "View should not exist after DROP");

    Ok(())
}

/// Test DROP VIEW IF EXISTS.
#[test]
fn test_drop_view_if_exists() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (id SERIAL PRIMARY KEY);
        DROP VIEW IF EXISTS nonexistent_view;
    ";

    // Should not error
    let _connection = translate_and_execute(sql)?;

    Ok(())
}

/// Test DROP VIEW CASCADE is stripped.
#[test]
fn test_drop_view_cascade_stripped() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL);
        CREATE VIEW all_users AS SELECT * FROM users;
        DROP VIEW all_users CASCADE;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let drop_stmt = translated.iter().find(|s| s.to_string().contains("DROP VIEW")).unwrap();
    let drop_str = drop_stmt.to_string();

    assert!(!drop_str.contains("CASCADE"), "CASCADE should be stripped: {drop_str}");

    let mut connection = translate_and_execute(sql)?;
    assert!(!view_exists(&mut connection, "all_users"));

    Ok(())
}

// ============================================================================
// DROP INDEX Tests
// ============================================================================

/// Test basic DROP INDEX translation and execution.
#[test]
fn test_drop_index() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (id SERIAL PRIMARY KEY, email TEXT NOT NULL);
        CREATE INDEX idx_users_email ON users (email);
        DROP INDEX idx_users_email;
    ";

    let mut connection = translate_and_execute(sql)?;

    assert!(table_exists(&mut connection, "users"), "Table should still exist");
    assert!(!index_exists(&mut connection, "idx_users_email"), "Index should not exist after DROP");

    Ok(())
}

/// Test DROP INDEX IF EXISTS.
#[test]
fn test_drop_index_if_exists() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (id SERIAL PRIMARY KEY);
        DROP INDEX IF EXISTS nonexistent_index;
    ";

    // Should not error
    let _connection = translate_and_execute(sql)?;

    Ok(())
}

/// Test DROP INDEX translation.
#[test]
fn test_drop_index_translation() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP INDEX idx_name;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();
    assert!(drop_str.contains("DROP INDEX"), "Should contain DROP INDEX: {drop_str}");

    Ok(())
}

// ============================================================================
// DROP TRIGGER Tests
// ============================================================================

/// Test DROP TRIGGER translation - verifies ON table_name is stripped.
#[test]
fn test_drop_trigger_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE audit_log (id SERIAL PRIMARY KEY, message TEXT);

        CREATE OR REPLACE FUNCTION log_changes() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            INSERT INTO audit_log (message) VALUES ('change');
            RETURN NEW;
        END;
        $$;

        CREATE TRIGGER log_user_changes
        AFTER INSERT ON users
        FOR EACH ROW EXECUTE FUNCTION log_changes();

        DROP TRIGGER log_user_changes ON users;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Find the DROP TRIGGER statement
    let drop_stmt = translated.iter().find(|s| s.to_string().contains("DROP TRIGGER"));

    assert!(drop_stmt.is_some(), "DROP TRIGGER should be in translated output");

    let drop_str = drop_stmt.unwrap().to_string();

    // SQLite DROP TRIGGER syntax doesn't include ON table_name
    assert!(!drop_str.contains(" ON "), "ON table_name should be stripped for SQLite: {drop_str}");

    Ok(())
}

/// Test DROP TRIGGER IF EXISTS translation.
#[test]
fn test_drop_trigger_if_exists() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TRIGGER IF EXISTS nonexistent_trigger ON users;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();

    // Should contain IF EXISTS
    assert!(drop_str.contains("IF EXISTS"), "IF EXISTS should be preserved: {drop_str}");
    // Should not contain ON table_name
    assert!(!drop_str.contains(" ON "), "ON table should be stripped: {drop_str}");

    Ok(())
}

/// Test DROP TRIGGER CASCADE is stripped.
#[test]
fn test_drop_trigger_cascade_stripped() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TRIGGER my_trigger ON my_table CASCADE;")?;

    assert_eq!(translated.len(), 1);
    let drop_str = translated[0].to_string();

    assert!(!drop_str.contains("CASCADE"), "CASCADE should be stripped: {drop_str}");
    assert!(!drop_str.contains(" ON "), "ON table should be stripped: {drop_str}");

    Ok(())
}

/// Test DROP TRIGGER execution with SQLite.
#[test]
fn test_drop_trigger_execution() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Create tables directly in SQLite using DDL (table creation is DDL, not DML)
    diesel::sql_query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&mut connection)?;
    diesel::sql_query("CREATE TABLE audit_log (id INTEGER PRIMARY KEY, message TEXT)")
        .execute(&mut connection)?;

    // Create a trigger directly in SQLite using DDL (trigger creation is DDL, not
    // DML)
    diesel::sql_query(
        "CREATE TRIGGER log_user_insert AFTER INSERT ON users \
         BEGIN INSERT INTO audit_log (message) VALUES ('user added'); END",
    )
    .execute(&mut connection)?;

    assert!(trigger_exists(&mut connection, "log_user_insert"), "Trigger should exist");

    // Translate and execute DROP TRIGGER (PostgreSQL syntax)
    let translated = translate_statement("DROP TRIGGER log_user_insert ON users;")?;
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    assert!(
        !trigger_exists(&mut connection, "log_user_insert"),
        "Trigger should not exist after DROP"
    );

    Ok(())
}

// ============================================================================
// PostgreSQL-specific DROP statements (should be ignored)
// ============================================================================

/// Test that DROP ROLE produces no output (ignored).
#[test]
fn test_drop_role_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP ROLE some_role;")?;
    assert!(translated.is_empty(), "DROP ROLE should produce no output");
    Ok(())
}

/// Test that DROP SCHEMA produces no output (ignored).
#[test]
fn test_drop_schema_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP SCHEMA my_schema CASCADE;")?;
    assert!(translated.is_empty(), "DROP SCHEMA should produce no output");
    Ok(())
}

/// Test that DROP DATABASE produces no output (ignored).
#[test]
fn test_drop_database_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP DATABASE my_database;")?;
    assert!(translated.is_empty(), "DROP DATABASE should produce no output");
    Ok(())
}

/// Test that DROP SEQUENCE produces no output (ignored).
#[test]
fn test_drop_sequence_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP SEQUENCE my_sequence;")?;
    assert!(translated.is_empty(), "DROP SEQUENCE should produce no output");
    Ok(())
}

/// Test that DROP TYPE produces no output (ignored).
#[test]
fn test_drop_type_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate_statement("DROP TYPE my_custom_type;")?;
    assert!(translated.is_empty(), "DROP TYPE should produce no output");
    Ok(())
}

// ============================================================================
// Snapshot Tests
// ============================================================================

/// Snapshot test for DROP statement translations.
#[test]
fn test_drop_statements_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let statements = vec![
        "DROP TABLE users;",
        "DROP TABLE IF EXISTS users;",
        "DROP TABLE users CASCADE;",
        "DROP VIEW my_view;",
        "DROP VIEW IF EXISTS my_view;",
        "DROP VIEW my_view RESTRICT;",
        "DROP INDEX idx_name;",
        "DROP INDEX IF EXISTS idx_name;",
    ];

    let mut translated_all = Vec::new();
    for sql in statements {
        let translated = translate_statement(sql)?;
        for stmt in translated {
            translated_all.push(stmt.to_string());
        }
    }

    let translated_sql = translated_all.join("\n");

    insta::assert_snapshot!("drop_statements", translated_sql);

    Ok(())
}

/// Snapshot test for DROP TRIGGER translations.
#[test]
fn test_drop_trigger_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let statements = vec![
        "DROP TRIGGER my_trigger ON my_table;",
        "DROP TRIGGER IF EXISTS my_trigger ON my_table;",
        "DROP TRIGGER my_trigger ON my_table CASCADE;",
    ];

    let mut translated_all = Vec::new();
    for sql in statements {
        let translated = translate_statement(sql)?;
        for stmt in translated {
            translated_all.push(stmt.to_string());
        }
    }

    let translated_sql = translated_all.join("\n");

    insta::assert_snapshot!("drop_trigger", translated_sql);

    Ok(())
}
