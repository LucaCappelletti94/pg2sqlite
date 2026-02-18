//! Tests for CHECK constraint handling with the
//! `remove_unsupported_check_constraints` option.
//!
//! Key behaviours tested:
//! - A table-level CHECK whose top-level expression is a function call causes a
//!   translation error unless `remove_unsupported_check_constraints()` is set.
//! - With the option set the constraint is silently dropped.
//! - A simple arithmetic CHECK (no functions) always passes through unchanged.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

/// A table-level CHECK whose top-level expression is an unsupported function
/// should fail translation when the option is not set.
#[test]
fn test_check_constraint_with_unsupported_function_causes_error() {
    // array_length is a PostgreSQL-specific function that doesn't exist in SQLite.
    // Using it as the top-level expression of a TABLE-LEVEL check constraint
    // (comma-separated, not inline on the column) triggers the function detection.
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (array_length(col)));";
    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err(), "Expected translation error for unsupported function in CHECK");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("array_length") || error_msg.to_lowercase().contains("undefined"),
        "Error should mention the function name, got: {error_msg}"
    );
}

/// With `remove_unsupported_check_constraints()` the same constraint is
/// silently dropped.
#[test]
fn test_check_constraint_removed_with_option() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, col TEXT, CHECK (array_length(col)));";
    let options = Pg2SqliteOptions::default().remove_unsupported_check_constraints();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        !sql_output.contains("CHECK"),
        "CHECK constraint should be removed when option is set, got: {sql_output}"
    );
}

/// A simple arithmetic CHECK (no PG-specific functions) always passes through.
#[test]
fn test_valid_check_constraint_passes_through() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, price INTEGER, CHECK (price > 0));";
    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        sql_output.contains("CHECK"),
        "Valid CHECK constraint should pass through, got: {sql_output}"
    );
    assert!(
        sql_output.contains("price > 0"),
        "CHECK condition should be preserved verbatim, got: {sql_output}"
    );
}

/// Column-level CHECK constraints are always silently dropped (regardless of
/// the option), because they are handled at the column option level, not the
/// table constraint level.
#[test]
fn test_column_level_check_is_always_dropped() {
    // Inline column CHECK (no leading comma) → ColumnOption::Check → always None
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, price INTEGER CHECK (price > 0));";
    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        !sql_output.contains("CHECK"),
        "Column-level CHECK should be silently dropped, got: {sql_output}"
    );
}
