//! R2-20: A bare `vector` column (no dimension) currently silently emits no
//! vec0 virtual table or sync triggers, leaving the index story broken without
//! any message to the caller. After the fix a `LossyDrop` warning is emitted
//! naming the column and explaining that vec0 needs an explicit dimension.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};

/// Translate the schema and return any warnings.
fn warnings_for(sql: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings
}

/// A `vector` column without a dimension count must warn.
#[test]
fn dimensionless_vector_column_emits_lossy_drop_warning() {
    let warnings = warnings_for("CREATE TABLE docs (id INT PRIMARY KEY, embedding vector);");
    let has_warning = warnings.iter().any(|w| {
        matches!(
            w,
            TranslationWarning::LossyDrop { construct, .. }
                if construct.to_lowercase().contains("embedding")
                    || construct.to_lowercase().contains("vector")
                    || construct.to_lowercase().contains("dimension")
        )
    });
    assert!(
        has_warning,
        "dimensionless vector column must emit a LossyDrop warning, got: {warnings:?}"
    );
}

/// A `vector(384)` column (with dimension) must not warn - it is fully
/// supported.
#[test]
fn dimensioned_vector_column_does_not_warn() {
    let warnings = warnings_for("CREATE TABLE docs (id INT PRIMARY KEY, embedding vector(384));");
    // Only a vec0 table is emitted. No LossyDrop warning about a missing dimension.
    let has_dimension_warning = warnings.iter().any(|w| {
        matches!(
            w,
            TranslationWarning::LossyDrop { construct, .. }
                if construct.to_lowercase().contains("dimension")
        )
    });
    assert!(
        !has_dimension_warning,
        "a vector(384) column must not warn about missing dimension, got: {warnings:?}"
    );
}

/// The translated CREATE TABLE still runs in SQLite even when the column has no
/// dimension (the column becomes BLOB, vec0 is skipped).
#[test]
fn dimensionless_vector_column_translation_still_executes() {
    let report = Pg2Sqlite::default()
        .sql("CREATE TABLE docs (id INT PRIMARY KEY, embedding vector);")
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");

    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &report.statements {
        // rusqlite: applying translated DDL (dynamically generated, diesel DSL cannot
        // express DDL).
        conn.execute_batch(&stmt.to_string())
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{stmt}"));
    }
    // If we get here the DDL is valid SQLite.
}
