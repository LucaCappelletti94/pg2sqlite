//! Tests for generic fallback translation of DEFAULT expressions.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::translate_default as translate;

#[test]
fn test_default_case_when() {
    let sql =
        "CREATE TABLE t (id INT PRIMARY KEY, status INT DEFAULT CASE WHEN true THEN 1 ELSE 0 END);";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
    apply_translated_pg(sql);
}

#[test]
fn test_default_nested_arithmetic() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT (1 + 2));";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
    apply_translated_pg(sql);
}

#[test]
fn test_default_coalesce() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT COALESCE(0, 1));";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
    apply_translated_pg(sql);
}

/// Translates `pg` with default options and executes every emitted statement
/// against an in-memory SQLite connection.
/// Translated DDL cannot be expressed via diesel's typed DSL, so sql_query is
/// used here to prove the emitted SQL is accepted by SQLite.
fn apply_translated_pg(pg: &str) {
    use diesel::prelude::*;
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("in-memory connection");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated statement must execute: {e}\n{s}"));
    }
}
