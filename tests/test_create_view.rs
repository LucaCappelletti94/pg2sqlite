//! Tests for CREATE VIEW translation (basic, IF NOT EXISTS, TEMPORARY,
//! MATERIALIZED error, OR REPLACE rewrite).

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

fn translate_joined(sql: &str) -> Result<String, String> {
    translate(sql).map(|v| v.join("\n"))
}

#[test]
fn basic_view_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("CREATE VIEW v"), "Expected CREATE VIEW: {output}");
}

#[test]
fn view_if_not_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW IF NOT EXISTS v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
}

#[test]
fn temporary_view() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE TEMPORARY VIEW v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("VIEW"), "Expected VIEW: {output}");
}

#[test]
fn materialized_view_produces_error() {
    let result = translate("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(result.is_err(), "Expected error for MATERIALIZED VIEW");
    let err = result.unwrap_err();
    assert!(err.contains("MATERIALIZED"), "Expected MATERIALIZED in error: {err}");
}

#[test]
fn or_replace_view_emits_drop_then_create() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE OR REPLACE VIEW v AS SELECT * FROM t;";
    let stmts = translate(sql).unwrap();
    // Table DDL + DROP VIEW IF EXISTS + CREATE VIEW = 3 statements
    assert_eq!(stmts.len(), 3, "Expected 3 statements, got {}: {stmts:?}", stmts.len());
    let drop = &stmts[1];
    let create = &stmts[2];
    assert!(
        drop.to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Second statement should be DROP VIEW IF EXISTS, got: {drop}"
    );
    assert!(
        create.to_uppercase().contains("CREATE VIEW"),
        "Third statement should be CREATE VIEW, got: {create}"
    );
    assert!(
        !create.to_uppercase().contains("OR REPLACE"),
        "CREATE VIEW should not contain OR REPLACE: {create}"
    );
}

#[test]
fn materialized_view_still_errors() {
    let result = translate_joined("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(result.is_err(), "Expected error for materialized view");
    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Expected error for materialized view, got empty");
}

#[test]
fn create_or_replace_view_emits_drop_and_create() {
    // CREATE OR REPLACE VIEW is now translated to DROP VIEW IF EXISTS + CREATE VIEW
    let sql = translate_joined("CREATE OR REPLACE VIEW v AS SELECT 1;").unwrap();
    assert!(
        sql.to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Expected DROP VIEW IF EXISTS in output: {sql}"
    );
    assert!(sql.to_uppercase().contains("CREATE VIEW"), "Expected CREATE VIEW in output: {sql}");
}
