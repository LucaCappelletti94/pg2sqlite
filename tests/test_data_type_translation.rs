//! Tests for data type translation branches in
//! `src/impls/translator_impls/data_type.rs`.
//!
//! Covers:
//! - Array type error
//! - UUID without representation, as BLOB, as TEXT
//! - Custom types: GEOGRAPHY, countrycode, CAS, vector, etc.
//! - Standard mappings: SERIAL, SmallInt, Boolean, Float, Bytea, Varchar, JSON,
//!   Timestamp
//! - Unknown custom type error

use std::sync::Once;

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};
use sqlite_vec::sqlite3_vec_init;

/// Register sqlite-vec once so connections opened by any test in this binary
/// have vec0 available. rusqlite FFI is the only path to this API.
///
/// SAFETY: `sqlite3_vec_init` has the C signature `(db, pzErrMsg, pApi) ->
/// int`; the transmute restores that type for `sqlite3_auto_extension`.
fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    });
}

/// Helper: translate SQL and return the output string.
fn translate(sql: &str, options: &Pg2SqliteOptions) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(options)
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn array_type_without_representation_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, arr INT[]);";
    let result = translate(sql, &Pg2SqliteOptions::default());
    let err = result.expect_err("array column should be rejected");
    assert!(err.contains("with_array_representation"), "Expected array error, got: {err}");
}

#[test]
fn uuid_without_representation_produces_error() {
    let sql = "CREATE TABLE t (id UUID PRIMARY KEY);";
    let result = translate(sql, &Pg2SqliteOptions::default());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("UUID translation requires"),
        "Expected UUID representation error, got: {err}"
    );
}

#[test]
fn unknown_custom_type_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x CUSTOMTYPE);";
    let result = translate(sql, &Pg2SqliteOptions::default());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Unknown PostgreSQL custom type"),
        "Expected unknown type error, got: {err}"
    );
}

#[test]
fn uuid_as_blob() {
    let sql = "CREATE TABLE t (id UUID PRIMARY KEY);";
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let output = translate(sql, &options).unwrap();
    assert!(output.contains("BLOB"), "UUID should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn uuid_as_text() {
    let sql = "CREATE TABLE t (id UUID PRIMARY KEY);";
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    let output = translate(sql, &options).unwrap();
    assert!(output.contains("TEXT"), "UUID should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn geography_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, geom GEOGRAPHY);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "GEOGRAPHY should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn countrycode_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, cc countrycode);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "countrycode should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn countrycode_uppercase_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, cc CountryCode);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "CountryCode should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn cas_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, c cas);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    // BINARY is not a valid SQLite STRICT type; must be BLOB.
    assert!(output.contains("BLOB"), "cas should map to BLOB, got: {output}");
    assert!(
        !output.contains("BINARY"),
        "cas must not produce BINARY (invalid in STRICT tables), got: {output}"
    );
    parse_sqlite(&output);
}

#[test]
fn molecular_formula_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, mf MolecularFormula);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "MolecularFormula should map to BLOB, got: {output}");
    assert!(!output.contains("BINARY"), "MolecularFormula must not produce BINARY, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn media_type_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, mt MediaType);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "MediaType should map to BLOB, got: {output}");
    assert!(!output.contains("BINARY"), "MediaType must not produce BINARY, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn vector_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(384));";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "vector should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn halfvec_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding halfvec(384));";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "halfvec should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn schema_qualified_vector_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding public.vector(384));";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "public.vector should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn serial_to_integer() {
    let sql = "CREATE TABLE t (id SERIAL PRIMARY KEY);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("INTEGER"), "SERIAL should map to INTEGER, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn smallint_to_integer() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val SMALLINT);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    // Both id and val should be INTEGER
    assert!(output.contains("INTEGER"), "SmallInt should map to INTEGER, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn boolean_to_integer() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, flag BOOLEAN);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("INTEGER"), "BOOLEAN should map to INTEGER, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn float_to_real() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val FLOAT);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("REAL"), "FLOAT should map to REAL, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn bytea_to_blob() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, data BYTEA);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "BYTEA should map to BLOB, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn varchar_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(255));";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "VARCHAR should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn json_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, data JSON);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "JSON should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn jsonb_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, data JSONB);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "JSONB should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn timestamp_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, created_at TIMESTAMP);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "TIMESTAMP should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn timestamp_with_timezone_to_text() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, created_at TIMESTAMP WITH TIME ZONE);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "TIMESTAMP WITH TIME ZONE should map to TEXT, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn text_passes_through() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("TEXT"), "TEXT should pass through, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn integer_passes_through() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("INTEGER"), "INTEGER should pass through, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn real_passes_through() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val REAL);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("REAL"), "REAL should pass through, got: {output}");
    parse_sqlite(&output);
}

#[test]
fn blob_passes_through() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, data BLOB);";
    let output = translate(sql, &Pg2SqliteOptions::default()).unwrap();
    assert!(output.contains("BLOB"), "BLOB should pass through, got: {output}");
    parse_sqlite(&output);
}

/// Execute each statement emitted by the `translate` helper against a fresh
/// in-memory SQLite connection.
///
/// The `translate` helper joins multiple statements with `\n` (no `;`).
/// `execute_batch` needs semicolons between statements, so we split on newlines
/// and execute each one individually. sqlite-vec is registered so tests that
/// translate vector types can execute the resulting vec0 DDL.
///
/// rusqlite is used directly because diesel does not expose
/// `sqlite3_auto_extension`.
fn parse_sqlite(sql: &str) {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in sql.lines().filter(|l| !l.trim().is_empty()) {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("translated DDL must run in SQLite: {e}\n{stmt}"));
    }
}
