//! Tests for GROUP L: Table constraint catch-all.
//!
//! L1: PrimaryKey/Unique expression indexes should have their column
//!     expressions translated through the IndexColumn translator.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn default_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

// ==================== L1: PK column expressions translated
// ====================

#[test]
fn pk_constraint_columns_translated() {
    // A simple PRIMARY KEY constraint should pass through correctly
    let options = default_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(lower.contains("primary key"), "PRIMARY KEY should appear in output: {sql}");
}

// ==================== L1: UNIQUE constraint columns translated
// ====================

#[test]
fn unique_constraint_columns_translated() {
    // A UNIQUE constraint with expression should translate the expression
    let options = default_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (
            id INTEGER PRIMARY KEY,
            email TEXT,
            UNIQUE (email)
        );
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(lower.contains("unique"), "UNIQUE should appear in output: {sql}");
}

// ==================== L1: PK + UNIQUE with characteristics
// ====================

#[test]
fn pk_with_deferrable_characteristics_errors() {
    // PrimaryKey with DEFERRABLE should translate characteristics
    // (characteristics translator returns UnsupportedSQLiteFeature error)
    let options = default_options();
    let result = translate_sql(
        r#"
        CREATE TABLE t (
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED
        );
        "#,
        &options,
    );

    // Should error because constraint characteristics are unsupported
    assert!(result.is_err(), "PK with DEFERRABLE should error: {result:?}");
}
