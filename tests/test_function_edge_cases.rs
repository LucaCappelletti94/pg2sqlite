//! Tests for function translation edge cases in
//! `src/impls/translator_impls/function.rs`.
//!
//! Covers:
//! - CONCAT with zero args -> error
//! - CONCAT_WS with <2 args -> error
//! - CONCAT with single arg -> passthrough
//! - CONCAT_WS with separator + single value -> just the value
//! - FILTER clause on aggregate -> CASE WHEN transformation
//! - string_agg -> group_concat
//! - strpos -> INSTR
//! - chr -> char

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Helper: translate SQL and return the output or error string.
fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

// ==================== CONCAT ====================

#[test]
fn concat_zero_args_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT concat() FROM t;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("CONCAT requires at least one argument"),
        "Expected CONCAT args error, got: {err}"
    );
}

#[test]
fn concat_single_arg_passthrough() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT concat(name) FROM t;";
    let output = translate(sql).unwrap();
    // With a single arg, CONCAT(name) should become just `name` (no || operator)
    assert!(!output.contains("||"), "Single arg CONCAT should not use ||, got: {output}");
    assert!(
        output.contains("name"),
        "Single arg CONCAT should pass through the expression, got: {output}"
    );
}

#[test]
fn concat_multiple_args_uses_string_concat() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat(first_name, ' ', last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "Multi-arg CONCAT should use ||, got: {output}");
}

// ==================== CONCAT_WS ====================

#[test]
fn concat_ws_less_than_two_args_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT concat_ws(',') FROM t;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("CONCAT_WS requires at least two arguments"),
        "Expected CONCAT_WS args error, got: {err}"
    );
}

#[test]
fn concat_ws_separator_plus_single_value() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT concat_ws(',', name) FROM t;";
    let output = translate(sql).unwrap();
    // With separator + single value, result is just the value (no || operator)
    assert!(!output.contains("||"), "CONCAT_WS with single value should not use ||, got: {output}");
}

#[test]
fn concat_ws_separator_plus_multiple_values() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat_ws(', ', first_name, last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "CONCAT_WS with multiple values should use ||, got: {output}");
}

// ==================== FILTER clause ====================

#[test]
fn filter_clause_on_count_star() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT COUNT(*) FILTER (WHERE x > 5) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("CASE WHEN"), "FILTER should become CASE WHEN, got: {output}");
    assert!(!output.contains("FILTER"), "FILTER clause should be removed, got: {output}");
}

#[test]
fn filter_clause_on_named_aggregate() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT SUM(x) FILTER (WHERE x > 0) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("CASE WHEN"), "FILTER should become CASE WHEN, got: {output}");
    assert!(!output.contains("FILTER"), "FILTER clause should be removed, got: {output}");
}

// ==================== Function renames ====================

#[test]
fn string_agg_becomes_group_concat() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT string_agg(name, ', ') FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("group_concat"),
        "string_agg should become group_concat, got: {output}"
    );
    assert!(
        !output.to_lowercase().contains("string_agg"),
        "string_agg should be replaced, got: {output}"
    );
}

#[test]
fn strpos_becomes_instr() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT strpos(name, 'test') FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("INSTR"), "strpos should become INSTR, got: {output}");
}

#[test]
fn chr_becomes_char() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT chr(65) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("char"), "chr should become char, got: {output}");
    // Make sure "chr" is not present
    assert!(!output.contains("chr("), "chr should be renamed to char, got: {output}");
}

// ==================== Other function translations ====================

#[test]
fn least_becomes_min() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT least(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MIN"), "LEAST should become MIN, got: {output}");
}

#[test]
fn greatest_becomes_max() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT greatest(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MAX"), "GREATEST should become MAX, got: {output}");
}

#[test]
fn now_becomes_datetime_now() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT now();";
    let output = translate(sql).unwrap();
    assert!(output.contains("datetime"), "NOW() should become datetime('now'), got: {output}");
}
