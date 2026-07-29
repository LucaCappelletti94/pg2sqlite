//! Tests for statement-level filtering/passthrough in
//! `src/impls/translator_impls/statement.rs`.
//!
//! Covers:
//! - Passthrough statements: VACUUM, COMMIT, ROLLBACK, START TRANSACTION,
//!   SAVEPOINT, RELEASE
//! - DROP TABLE/VIEW/INDEX -> strips CASCADE/RESTRICT
//! - DROP TRIGGER -> strips table name and option
//! - Filtered/skipped statements: ALTER TABLE, CREATE FUNCTION, GRANT, etc.
//! - DROP of unsupported object type -> empty

mod helpers;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Helper: translate SQL and return the output or error string.
fn translate(sql: &str) -> Result<String, String> {
    helpers::translate_sql(sql, &Pg2SqliteOptions::default())
}

/// Helper: translate SQL and return the count of resulting statements.
fn translate_count(sql: &str) -> Result<usize, String> {
    helpers::translate_count(sql, &Pg2SqliteOptions::default())
}

#[test]
fn vacuum_passes_through() {
    let output = translate("VACUUM;").unwrap();
    assert!(output.contains("VACUUM"), "VACUUM should pass through, got: {output}");
}

#[test]
fn commit_passes_through() {
    let output = translate("COMMIT;").unwrap();
    assert!(output.contains("COMMIT"), "COMMIT should pass through, got: {output}");
}

#[test]
fn rollback_passes_through() {
    let output = translate("ROLLBACK;").unwrap();
    assert!(output.contains("ROLLBACK"), "ROLLBACK should pass through, got: {output}");
}

#[test]
fn start_transaction_passes_through() {
    let output = translate("START TRANSACTION;").unwrap();
    // SQLite uses BEGIN instead of START TRANSACTION, but the parser should handle
    // it
    assert!(
        output.contains("TRANSACTION") || output.contains("BEGIN"),
        "START TRANSACTION should pass through, got: {output}"
    );
}

#[test]
fn savepoint_passes_through() {
    let output = translate("SAVEPOINT sp1;").unwrap();
    assert!(output.contains("SAVEPOINT"), "SAVEPOINT should pass through, got: {output}");
}

#[test]
fn release_savepoint_passes_through() {
    let output = translate("RELEASE SAVEPOINT sp1;").unwrap();
    assert!(output.contains("RELEASE"), "RELEASE SAVEPOINT should pass through, got: {output}");
}

#[test]
fn drop_table_strips_cascade() {
    // Use IF EXISTS to avoid schema builder requiring the table to exist
    let output = translate("DROP TABLE IF EXISTS t CASCADE;").unwrap();
    assert!(output.contains("DROP TABLE"), "DROP TABLE should be present, got: {output}");
    assert!(!output.contains("CASCADE"), "CASCADE should be stripped, got: {output}");
}

#[test]
fn drop_table_if_exists() {
    let output = translate("DROP TABLE IF EXISTS t;").unwrap();
    assert!(output.contains("DROP TABLE"), "DROP TABLE should be present, got: {output}");
    assert!(output.contains("IF EXISTS"), "IF EXISTS should be preserved, got: {output}");
}

#[test]
fn drop_view_strips_restrict() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE VIEW v AS SELECT * FROM t;
               DROP VIEW v RESTRICT;";
    let output = translate(sql).unwrap();
    assert!(output.contains("DROP VIEW"), "DROP VIEW should be present, got: {output}");
    assert!(!output.contains("RESTRICT"), "RESTRICT should be stripped, got: {output}");
}

#[test]
fn drop_index() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE INDEX idx_name ON t (name);
               DROP INDEX idx_name;";
    let output = translate(sql).unwrap();
    assert!(output.contains("DROP INDEX"), "DROP INDEX should be present, got: {output}");
}

#[test]
fn drop_trigger_strips_table_name() {
    let sql = "DROP TRIGGER IF EXISTS my_trigger ON my_table;";
    let output = translate(sql).unwrap();
    assert!(output.contains("DROP TRIGGER"), "DROP TRIGGER should be present, got: {output}");
    // The ON my_table should be removed for SQLite
    assert!(!output.contains("ON my_table"), "ON table_name should be stripped, got: {output}");
}

#[test]
fn drop_sequence_produces_empty() {
    let count = translate_count("DROP SEQUENCE my_seq;").unwrap();
    assert_eq!(count, 0, "DROP SEQUENCE should produce no output");
}

#[test]
fn alter_table_filtered() {
    let count = translate_count("ALTER TABLE t ADD COLUMN name TEXT;").unwrap();
    assert_eq!(count, 0, "ALTER TABLE should be filtered");
}

#[test]
fn create_function_filtered() {
    let sql = "CREATE FUNCTION my_func() RETURNS void AS $$ BEGIN END; $$ LANGUAGE plpgsql;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE FUNCTION should be filtered");
}

#[test]
fn create_extension_filtered() {
    let sql = "CREATE EXTENSION IF NOT EXISTS pgcrypto;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE EXTENSION should be filtered");
}

#[test]
fn grant_filtered() {
    // The role and table must exist in the schema for the schema builder
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE ROLE some_role;
               GRANT SELECT ON t TO some_role;";
    let count = translate_count(sql).unwrap();
    // CREATE TABLE produces 1 statement; CREATE ROLE and GRANT are both filtered
    assert_eq!(count, 1, "Only CREATE TABLE should survive; GRANT and CREATE ROLE filtered");
}

#[test]
fn revoke_filtered() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE ROLE some_role;
               GRANT SELECT ON t TO some_role;
               REVOKE SELECT ON t FROM some_role;";
    let count = translate_count(sql).unwrap();
    // Only CREATE TABLE survives
    assert_eq!(count, 1, "Only CREATE TABLE should survive; REVOKE filtered");
}

#[test]
fn create_type_filtered() {
    let sql = "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE TYPE should be filtered");
}

#[test]
fn create_schema_filtered() {
    let sql = "CREATE SCHEMA myschema;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE SCHEMA should be filtered");
}

#[test]
fn set_statement_filtered() {
    let sql = "SET search_path TO myschema;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "SET should be filtered");
}

#[test]
fn create_policy_filtered() {
    let sql = "CREATE POLICY my_policy ON t FOR SELECT USING (true);";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE POLICY should be filtered");
}

#[test]
fn create_role_filtered() {
    let sql = "CREATE ROLE my_role;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE ROLE should be filtered");
}

#[test]
fn comment_filtered() {
    let sql = "COMMENT ON TABLE t IS 'my table';";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "COMMENT should be filtered");
}

#[test]
fn create_sequence_filtered() {
    let sql = "CREATE SEQUENCE my_seq;";
    let count = translate_count(sql).unwrap();
    assert_eq!(count, 0, "CREATE SEQUENCE should be filtered");
}

#[test]
fn mixed_statements_filters_correctly() {
    let sql = "
        CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
        CREATE EXTENSION IF NOT EXISTS pgcrypto;
        CREATE INDEX idx_name ON t (name);
        ALTER TABLE t ADD COLUMN email TEXT;
        DROP INDEX idx_name;
    ";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    // Should get: CREATE TABLE, CREATE INDEX, DROP INDEX (3 statements)
    // CREATE EXTENSION and ALTER TABLE should be filtered
    assert_eq!(
        stmts.len(),
        3,
        "Expected 3 statements, got: {} - {:?}",
        stmts.len(),
        stmts.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}
