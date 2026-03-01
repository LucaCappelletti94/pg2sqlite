//! Tests verifying NULL-handling semantics for CONCAT and CONCAT_WS.
//!
//! PostgreSQL's CONCAT ignores NULL arguments (treats them as empty string).
//! SQLite's `||` operator propagates NULLs.  The translator wraps each
//! argument with `COALESCE(arg, '')` to preserve PostgreSQL semantics.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn concat_wraps_args_with_coalesce() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a TEXT, b TEXT);
               SELECT CONCAT(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("COALESCE"),
        "CONCAT should wrap args with COALESCE to handle NULLs, got: {output}"
    );
}

#[test]
fn concat_three_args_all_coalesced() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a TEXT, b TEXT, c TEXT);
               SELECT CONCAT(a, b, c) FROM t;";
    let output = translate(sql).unwrap();
    // Each arg should be wrapped with COALESCE
    let coalesce_count = output.matches("COALESCE").count();
    assert!(
        coalesce_count >= 3,
        "CONCAT with 3 args should have at least 3 COALESCE wraps, got {coalesce_count} in: {output}"
    );
}

#[test]
fn concat_ws_wraps_value_args_with_coalesce_not_separator() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a TEXT, b TEXT);
               SELECT CONCAT_WS(',', a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("CASE WHEN"),
        "CONCAT_WS should use CASE expressions to skip NULL values, got: {output}"
    );
    assert!(
        !output.contains("COALESCE(',',"),
        "CONCAT_WS should not wrap the separator with COALESCE, got: {output}"
    );
}

#[test]
fn concat_single_arg_produces_coalesced_result() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT CONCAT(name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("COALESCE"),
        "CONCAT with one arg should still COALESCE-wrap it, got: {output}"
    );
}
