//! Tests for forward function mapping gaps.
//!
//! Covers PostgreSQL functions that were previously falling through to
//! `PassThrough` and should instead be explicitly renamed or rejected.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::{translate_default as translate, translate_default_err as translate_err};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, val TEXT);";

#[test]
fn test_ascii_renamed_to_unicode() {
    let sql = &format!("{TABLE} SELECT ascii('A') FROM t;");
    let output = translate(sql);
    assert!(output.contains("unicode("), "ascii() should be renamed to unicode(), got: {output}");
    assert!(!output.contains("ascii("), "ascii() should no longer appear in output, got: {output}");
    // Execute translated DDL + query to verify the output is valid SQLite.
    {
        let stmts = pg2sqlite::prelude::Pg2Sqlite::default()
            .sql(sql)
            .unwrap()
            .translate(&pg2sqlite::prelude::Pg2SqliteOptions::default())
            .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in &stmts {
            conn.execute_batch(&format!("{stmt};")).expect("translated SQL must execute in SQLite");
        }
    }
}

#[test]
fn test_current_database_unsupported() {
    let sql = &format!("{TABLE} SELECT current_database() FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("current_database"), "Error should mention current_database, got: {err}");
}

#[test]
fn test_current_schema_unsupported() {
    let sql = &format!("{TABLE} SELECT current_schema() FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("current_schema"), "Error should mention current_schema, got: {err}");
}

#[test]
fn test_pg_typeof_unsupported() {
    let sql = &format!("{TABLE} SELECT pg_typeof(1) FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("pg_typeof"), "Error should mention pg_typeof, got: {err}");
}

#[test]
fn test_unnest_unsupported() {
    let sql = &format!("{TABLE} SELECT unnest(ARRAY[1,2]) FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("unnest"), "Error should mention unnest, got: {err}");
    assert!(err.contains("json_each"), "Error should suggest json_each() alternative, got: {err}");
}

#[test]
fn test_encode_unsupported() {
    let sql = &format!("{TABLE} SELECT encode(val, 'hex') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("encode"), "Error should mention encode, got: {err}");
    assert!(err.contains("hex()"), "Error should suggest hex()/unhex() alternative, got: {err}");
}

#[test]
fn test_decode_unsupported() {
    let sql = &format!("{TABLE} SELECT decode(val, 'hex') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("decode"), "Error should mention decode, got: {err}");
    assert!(err.contains("unhex()"), "Error should suggest hex()/unhex() alternative, got: {err}");
}

#[test]
fn test_to_number_unsupported() {
    let sql = &format!("{TABLE} SELECT to_number('12', '99') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("to_number"), "Error should mention to_number, got: {err}");
    assert!(err.contains("CAST"), "Error should suggest CAST alternative, got: {err}");
}
