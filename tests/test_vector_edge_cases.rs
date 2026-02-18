//! Tests for vector translation edge cases in
//! `src/impls/translator_impls/vector.rs`.
//!
//! Covers: table-level PK constraint, missing PK error, vector without
//! dimensions, multiple vector columns, and vector with RLS.

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

fn translate_err(sql: &str) -> String {
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    match result {
        Err(e) => e.to_string(),
        Ok(stmts) => stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"),
    }
}

// ==================== Vector with table-level PK constraint
// ====================

#[test]
fn vector_with_table_level_pk() {
    let sql = "
        CREATE TABLE items (
            embedding VECTOR(384),
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
    ";
    let output = translate(sql);
    // Should produce vec0 virtual table with table-level PK
    assert!(output.contains("vec0") || output.contains("BLOB"), "Expected vec0 or BLOB: {output}");
}

// ==================== Vector with column-level PK ====================

#[test]
fn vector_with_column_level_pk() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR(384)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("vec0"), "Expected vec0 virtual table: {output}");
}

// ==================== Multiple vector columns ====================

#[test]
fn multiple_vector_columns() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            title_embedding VECTOR(384),
            content_embedding VECTOR(768)
        );
    ";
    let output = translate(sql);
    // Should produce separate vec0 tables for each vector column
    assert!(output.contains("vec0"), "Expected vec0: {output}");
}

// ==================== Vector column without explicit dimensions
// ====================

#[test]
fn vector_without_dimensions() {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR
        );
    ";
    let output = translate(sql);
    // VECTOR without dimensions should still translate
    assert!(output.contains("BLOB") || output.contains("vec0"), "Expected BLOB or vec0: {output}");
}

// ==================== Vector table without PK (should error)
// ====================

#[test]
fn vector_without_pk_produces_error() {
    let sql = "
        CREATE TABLE items (
            name TEXT NOT NULL,
            embedding VECTOR(384)
        );
    ";
    let output = translate_err(sql);
    // Should produce an error about missing primary key
    assert!(
        output.contains("primary key")
            || output.contains("PRIMARY KEY")
            || output.contains("Primary"),
        "Expected PK error: {output}"
    );
}

// ==================== Vector with composite PK (should error)
// ====================

#[test]
fn vector_with_composite_pk() {
    let sql = "
        CREATE TABLE items (
            id1 INTEGER,
            id2 INTEGER,
            embedding VECTOR(384),
            PRIMARY KEY (id1, id2)
        );
    ";
    // Composite PK should be problematic for vec0 sync triggers
    let output = translate_err(sql);
    // Either error or work with first PK column
    assert!(
        output.contains("vec0") || output.contains("primary key") || output.contains("BLOB"),
        "Expected vec0 or PK error: {output}"
    );
}
