//! Tests for generic fallback translation of DEFAULT expressions.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_default_case_when() {
    let sql =
        "CREATE TABLE t (id INT PRIMARY KEY, status INT DEFAULT CASE WHEN true THEN 1 ELSE 0 END);";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
}

#[test]
fn test_default_nested_arithmetic() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT (1 + 2));";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
}

#[test]
fn test_default_coalesce() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT COALESCE(0, 1));";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT in output: {output}");
}
