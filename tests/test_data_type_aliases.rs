//! Tests for SQL data type alias mappings from PostgreSQL to SQLite.
//!
//! Covers integer, float, numeric, binary, text, bit, and temporal type
//! aliases, typed string literals, and a Diesel end-to-end integration test.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// Helpers
// ============================================================================

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

fn translate_ok(sql: &str) -> String {
    translate(sql).expect("should translate")
}

fn translate_err(sql: &str) -> String {
    translate(sql).expect_err("should fail")
}

// ============================================================================
// Diesel schema for functional test
// ============================================================================

mod schema {
    diesel::table! {
        type_compat_test (id) {
            id        -> BigInt,
            label     -> Text,
            notes     -> Nullable<Text>,
            amount    -> Nullable<Double>,
            ratio     -> Nullable<Double>,
            raw_bytes -> Nullable<Binary>,
            flag      -> Nullable<Integer>,
            created   -> Nullable<Text>,
            active    -> Integer,
        }
    }
}

use schema::type_compat_test;

#[derive(Insertable)]
#[diesel(table_name = type_compat_test)]
struct NewRow {
    id: i64,
    label: String,
    active: i32,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = type_compat_test)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)]
struct Row {
    id: i64,
    label: String,
    active: i32,
}

// ============================================================================
// Integer alias tests
// ============================================================================

#[test]
fn bigint_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col BIGINT);");
    assert!(out.contains("INTEGER"), "BIGINT should map to INTEGER, got: {out}");
    assert!(!out.contains("BIGINT"), "Output should not contain BIGINT, got: {out}");
}

#[test]
fn int8_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col INT8);");
    assert!(out.contains("INTEGER"), "INT8 should map to INTEGER, got: {out}");
    assert!(!out.contains("INT8"), "Output should not contain INT8, got: {out}");
}

#[test]
fn int4_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col INT4);");
    assert!(out.contains("INTEGER"), "INT4 should map to INTEGER, got: {out}");
    assert!(!out.contains("INT4"), "Output should not contain INT4, got: {out}");
}

#[test]
fn int2_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col INT2);");
    assert!(out.contains("INTEGER"), "INT2 should map to INTEGER, got: {out}");
    assert!(!out.contains("INT2"), "Output should not contain INT2, got: {out}");
}

#[test]
fn tinyint_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TINYINT);");
    assert!(out.contains("INTEGER"), "TINYINT should map to INTEGER, got: {out}");
    assert!(!out.contains("TINYINT"), "Output should not contain TINYINT, got: {out}");
}

// ============================================================================
// Float alias tests
// ============================================================================

#[test]
fn double_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col DOUBLE);");
    assert!(out.contains("REAL"), "DOUBLE should map to REAL, got: {out}");
}

#[test]
fn double_precision_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col DOUBLE PRECISION);");
    assert!(out.contains("REAL"), "DOUBLE PRECISION should map to REAL, got: {out}");
}

#[test]
fn float8_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col FLOAT8);");
    assert!(out.contains("REAL"), "FLOAT8 should map to REAL, got: {out}");
}

#[test]
fn float4_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col FLOAT4);");
    assert!(out.contains("REAL"), "FLOAT4 should map to REAL, got: {out}");
}

// ============================================================================
// Numeric/Decimal tests
// ============================================================================

#[test]
fn numeric_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col NUMERIC(10,2));");
    assert!(out.contains("REAL"), "NUMERIC should map to REAL, got: {out}");
    assert!(!out.contains("NUMERIC"), "Output should not contain NUMERIC, got: {out}");
}

#[test]
fn decimal_maps_to_real() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col DECIMAL(5,2));");
    assert!(out.contains("REAL"), "DECIMAL should map to REAL, got: {out}");
    assert!(!out.contains("DECIMAL"), "Output should not contain DECIMAL, got: {out}");
}

// ============================================================================
// Binary alias tests
// ============================================================================

#[test]
fn binary_maps_to_blob() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col BINARY(50));");
    assert!(out.contains("BLOB"), "BINARY should map to BLOB, got: {out}");
}

#[test]
fn varbinary_maps_to_blob() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col VARBINARY(50));");
    assert!(out.contains("BLOB"), "VARBINARY should map to BLOB, got: {out}");
}

// ============================================================================
// Text alias tests
// ============================================================================

#[test]
fn clob_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col CLOB);");
    assert!(out.contains("TEXT"), "CLOB should map to TEXT, got: {out}");
    assert!(!out.contains("CLOB"), "Output should not contain CLOB, got: {out}");
}

#[test]
fn nvarchar_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col NVARCHAR(50));");
    assert!(out.contains("TEXT"), "NVARCHAR should map to TEXT, got: {out}");
    assert!(!out.contains("NVARCHAR"), "Output should not contain NVARCHAR, got: {out}");
}

#[test]
fn enum_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col ENUM('a','b'));");
    assert!(out.contains("TEXT"), "ENUM should map to TEXT, got: {out}");
}

#[test]
fn tsvector_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TSVECTOR);");
    assert!(out.contains("TEXT"), "TSVECTOR should map to TEXT, got: {out}");
    assert!(!out.contains("TSVECTOR"), "Output should not contain TSVECTOR, got: {out}");
}

#[test]
fn tsquery_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TSQUERY);");
    assert!(out.contains("TEXT"), "TSQUERY should map to TEXT, got: {out}");
    assert!(!out.contains("TSQUERY"), "Output should not contain TSQUERY, got: {out}");
}

// ============================================================================
// Bit type tests
// ============================================================================

#[test]
fn bit_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col BIT);");
    assert!(out.contains("INTEGER"), "BIT should map to INTEGER, got: {out}");
}

#[test]
fn varbit_maps_to_integer() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col VARBIT(8));");
    assert!(out.contains("INTEGER"), "VARBIT should map to INTEGER, got: {out}");
    assert!(!out.contains("VARBIT"), "Output should not contain VARBIT, got: {out}");
}

// ============================================================================
// Temporal type tests
// ============================================================================

#[test]
fn interval_col_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col INTERVAL);");
    assert!(out.contains("TEXT"), "INTERVAL should map to TEXT, got: {out}");
    assert!(!out.contains("INTERVAL"), "Output should not contain INTERVAL, got: {out}");
}

#[test]
fn date_col_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col DATE);");
    assert!(out.contains("TEXT"), "DATE should map to TEXT, got: {out}");
    assert!(!out.contains("DATE"), "Output should not contain DATE, got: {out}");
}

#[test]
fn time_col_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TIME);");
    assert!(out.contains("TEXT"), "TIME should map to TEXT, got: {out}");
}

#[test]
fn datetime_col_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col DATETIME);");
    assert!(out.contains("TEXT"), "DATETIME should map to TEXT, got: {out}");
    assert!(!out.contains("DATETIME"), "Output should not contain DATETIME, got: {out}");
}

#[test]
fn timestamptz_tz_variant_maps_to_text() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TIMESTAMPTZ);");
    assert!(out.contains("TEXT"), "TIMESTAMPTZ should map to TEXT, got: {out}");
    assert!(!out.contains("TIMESTAMPTZ"), "Output should not contain TIMESTAMPTZ, got: {out}");
}

// ============================================================================
// Typed string literal tests (DATE/TIME/DATETIME '...')
// ============================================================================

#[test]
fn date_typed_string_translates() {
    // DATE '1999-01-01' becomes CAST('1999-01-01' AS TEXT) in SQLite
    let out = translate_ok("SELECT DATE '1999-01-01';");
    assert!(!out.is_empty(), "DATE typed string should translate, got: {out}");
}

#[test]
fn time_typed_string_translates() {
    let out = translate_ok("SELECT TIME '01:23:34';");
    assert!(!out.is_empty(), "TIME typed string should translate, got: {out}");
}

#[test]
fn datetime_typed_string_translates() {
    let out = translate_ok("SELECT DATETIME '1999-01-01 01:23:34';");
    assert!(!out.is_empty(), "DATETIME typed string should translate, got: {out}");
}

// ============================================================================
// Negative tests — correctly-rejected features
// ============================================================================

#[test]
fn array_type_still_errors() {
    let err = translate_err("CREATE TABLE t (id INT PRIMARY KEY, arr INT[]);");
    assert!(err.contains("Array type"), "Expected Array error, got: {err}");
}

// ============================================================================
// Diesel functional test
// ============================================================================

#[test]
fn diesel_translated_schema_accepts_insert_and_query() {
    let fixture = include_str!("fixtures/missing_types.sql");
    let stmts =
        Pg2Sqlite::default().sql(fixture).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();

    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for stmt in &stmts {
        diesel::sql_query(stmt.to_string()).execute(&mut conn).unwrap();
    }

    diesel::insert_into(type_compat_test::table)
        .values(NewRow { id: 1, label: "hello".to_string(), active: 1 })
        .execute(&mut conn)
        .unwrap();

    let rows = type_compat_test::table.select(Row::as_select()).load(&mut conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].label, "hello");
}
