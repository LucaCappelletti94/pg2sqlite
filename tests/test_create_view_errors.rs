//! Tests for CREATE VIEW error branches in
//! `src/impls/translator_impls/create_view.rs`.
//!
//! Covers: MATERIALIZED VIEW error, OR REPLACE error, basic view passthrough.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

// ==================== Basic view ====================

#[test]
fn basic_view_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW v AS SELECT * FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("CREATE VIEW v"), "Expected CREATE VIEW: {output}");
}

#[test]
fn view_if_not_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW IF NOT EXISTS v AS SELECT * FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
}

#[test]
fn temporary_view() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE TEMPORARY VIEW v AS SELECT * FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("VIEW"), "Expected VIEW: {output}");
}

// ==================== MATERIALIZED VIEW error ====================

#[test]
fn materialized_view_produces_error() {
    let result = translate("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(result.is_err(), "Expected error for MATERIALIZED VIEW");
    let err = result.unwrap_err();
    assert!(err.contains("MATERIALIZED"), "Expected MATERIALIZED in error: {err}");
}

// ==================== OR REPLACE error ====================

#[test]
fn or_replace_view_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE OR REPLACE VIEW v AS SELECT * FROM t;";
    let result = translate(sql);
    assert!(result.is_err(), "Expected error for OR REPLACE VIEW");
    let err = result.unwrap_err();
    assert!(err.contains("OR REPLACE"), "Expected OR REPLACE in error: {err}");
}
