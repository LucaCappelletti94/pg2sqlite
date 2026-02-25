//! Tests for missing type mappings and column option handling.
//!
//! Covers:
//! - Integer aliases: BIGINT, INT8, INT4, INT2, TINYINT → INTEGER
//! - Float aliases: DOUBLE, DOUBLE PRECISION, FLOAT8, FLOAT4 → REAL
//! - Numeric/Decimal → REAL (intentionally lossy)
//! - Binary aliases: BINARY, VARBINARY → BLOB
//! - Text aliases: CLOB, NVARCHAR, ENUM, TSVECTOR, TSQUERY → TEXT
//! - Bit types: BIT, VARBIT → INTEGER
//! - Temporal types: DATE, TIME, DATETIME, TIMESTAMPTZ → TEXT (already in TEXT
//!   group)
//! - Column options: COLLATE, CHARACTER SET, COMMENT silently dropped
//! - Typed string literals: DATE '...', TIME '...', DATETIME '...' translate
//! - Negative tests: still-unsupported features produce errors

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
// Column option tests
// ============================================================================

#[test]
fn collate_option_silently_dropped() {
    let out = translate_ok(r#"CREATE TABLE t (id INT PRIMARY KEY, col TEXT COLLATE "de_DE");"#);
    assert!(!out.contains("COLLATE"), "COLLATE should be dropped, got: {out}");
}

#[test]
fn character_set_option_silently_dropped() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col TEXT CHARACTER SET utf8mb4);");
    assert!(!out.contains("CHARACTER SET"), "CHARACTER SET should be dropped, got: {out}");
}

#[test]
fn comment_option_silently_dropped() {
    let out = translate_ok("CREATE TABLE t (id INT PRIMARY KEY, col INT COMMENT 'desc');");
    assert!(!out.contains("COMMENT"), "COMMENT should be dropped, got: {out}");
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

#[test]
fn uuid_without_repr_still_errors() {
    let err = translate_err("CREATE TABLE t (id UUID PRIMARY KEY);");
    assert!(
        err.contains("UUID translation requires"),
        "Expected UUID representation error, got: {err}"
    );
}

#[test]
fn materialized_view_still_errors() {
    let err = translate_err("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(!err.is_empty(), "Expected error for materialized view, got empty");
}

#[test]
fn at_time_zone_named_zone_still_errors() {
    // Named timezones like 'Europe/Brussels' are not supported; only
    // UTC/local/fixed offsets are.
    let err = translate_err("SELECT col AT TIME ZONE 'Europe/Brussels' FROM t;");
    assert!(!err.is_empty(), "Expected error for named AT TIME ZONE, got empty");
}

#[test]
fn create_or_replace_view_emits_drop_and_create() {
    // CREATE OR REPLACE VIEW is now translated to DROP VIEW IF EXISTS + CREATE VIEW
    let sql = translate_ok("CREATE OR REPLACE VIEW v AS SELECT 1;");
    assert!(
        sql.to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Expected DROP VIEW IF EXISTS in output: {sql}"
    );
    assert!(sql.to_uppercase().contains("CREATE VIEW"), "Expected CREATE VIEW in output: {sql}");
}

#[test]
fn percentile_cont_still_errors() {
    let err = translate_err("SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY val) FROM t;");
    assert!(!err.is_empty(), "Expected error for PERCENTILE_CONT, got empty");
}

#[test]
fn stddev_still_errors() {
    let err = translate_err("SELECT STDDEV(val) FROM t;");
    assert!(!err.is_empty(), "Expected error for STDDEV, got empty");
}

#[test]
fn regexp_replace_still_errors() {
    let err = translate_err("SELECT REGEXP_REPLACE(col, 'a', 'b') FROM t;");
    assert!(!err.is_empty(), "Expected error for REGEXP_REPLACE, got empty");
}

#[test]
fn split_part_still_errors() {
    let err = translate_err("SELECT SPLIT_PART(col, ',', 1) FROM t;");
    assert!(!err.is_empty(), "Expected error for SPLIT_PART, got empty");
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
