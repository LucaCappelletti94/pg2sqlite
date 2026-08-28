//! Tests that `generate_series` in a FROM clause produces an explicit
//! typed translation refusal rather than silently passing through
//! to SQLite (where it would fail at runtime with "no such table").

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

#[test]
fn generate_series_in_from_produces_error() {
    let result = translate("SELECT * FROM generate_series(1, 10)");
    assert!(result.is_err(), "Expected error for generate_series, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("generate_series"),
        "Error should mention generate_series: {err}"
    );
}

#[test]
fn generate_series_with_alias_produces_error() {
    let result = translate("SELECT n FROM generate_series(1, 5) AS g(n)");
    assert!(result.is_err(), "Expected error for generate_series with alias, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("generate_series"),
        "Error should mention generate_series: {err}"
    );
}
