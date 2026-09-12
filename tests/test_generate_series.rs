//! Tests that `generate_series` in a FROM clause produces an explicit
//! typed translation refusal rather than silently passing through
//! to SQLite (where it would fail at runtime with "no such table").

mod helpers;
use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

#[test]
fn generate_series_in_from_produces_error() {
    let result =
        translate_sql("SELECT * FROM generate_series(1, 10)", &Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for generate_series, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("generate_series"),
        "Error should mention generate_series: {err}"
    );
}

#[test]
fn generate_series_with_alias_produces_error() {
    let result =
        translate_sql("SELECT n FROM generate_series(1, 5) AS g(n)", &Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for generate_series with alias, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("generate_series"),
        "Error should mention generate_series: {err}"
    );
}
