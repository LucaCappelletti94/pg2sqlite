//! Tests for forward function mapping gaps.
//!
//! Covers PostgreSQL functions that were previously falling through to
//! `PassThrough` and should instead be explicitly renamed or rejected.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, val TEXT);";

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

fn translate_err(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err()
        .to_string()
}

// ---------------------------------------------------------------------------
// ascii() -> unicode()
// ---------------------------------------------------------------------------

#[test]
fn test_ascii_renamed_to_unicode() {
    let sql = &format!("{TABLE} SELECT ascii('A') FROM t;");
    let output = translate(sql);
    assert!(output.contains("unicode("), "ascii() should be renamed to unicode(), got: {output}");
    assert!(!output.contains("ascii("), "ascii() should no longer appear in output, got: {output}");
}

// ---------------------------------------------------------------------------
// current_database() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_current_database_unsupported() {
    let sql = &format!("{TABLE} SELECT current_database() FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("current_database"), "Error should mention current_database, got: {err}");
}

// ---------------------------------------------------------------------------
// current_schema() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_current_schema_unsupported() {
    let sql = &format!("{TABLE} SELECT current_schema() FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("current_schema"), "Error should mention current_schema, got: {err}");
}

// ---------------------------------------------------------------------------
// pg_typeof() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_pg_typeof_unsupported() {
    let sql = &format!("{TABLE} SELECT pg_typeof(1) FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("pg_typeof"), "Error should mention pg_typeof, got: {err}");
}

// ---------------------------------------------------------------------------
// unnest() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_unnest_unsupported() {
    let sql = &format!("{TABLE} SELECT unnest(ARRAY[1,2]) FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("unnest"), "Error should mention unnest, got: {err}");
    assert!(err.contains("json_each"), "Error should suggest json_each() alternative, got: {err}");
}

// ---------------------------------------------------------------------------
// encode() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_encode_unsupported() {
    let sql = &format!("{TABLE} SELECT encode(val, 'hex') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("encode"), "Error should mention encode, got: {err}");
    assert!(err.contains("hex()"), "Error should suggest hex()/unhex() alternative, got: {err}");
}

// ---------------------------------------------------------------------------
// decode() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_decode_unsupported() {
    let sql = &format!("{TABLE} SELECT decode(val, 'hex') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("decode"), "Error should mention decode, got: {err}");
    assert!(err.contains("unhex()"), "Error should suggest hex()/unhex() alternative, got: {err}");
}

// ---------------------------------------------------------------------------
// to_number() -> Unsupported
// ---------------------------------------------------------------------------

#[test]
fn test_to_number_unsupported() {
    let sql = &format!("{TABLE} SELECT to_number('12', '99') FROM t;");
    let err = translate_err(sql);
    assert!(err.contains("to_number"), "Error should mention to_number, got: {err}");
    assert!(err.contains("CAST"), "Error should suggest CAST alternative, got: {err}");
}
