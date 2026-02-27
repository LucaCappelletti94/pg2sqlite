//! Tests for table-level FK constraint translation gaps (GROUP D).
//!
//! Verifies that table-level FOREIGN KEY constraints translate
//! characteristics consistently with column-level FK (column_option.rs).

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn fk_deferrable_initially_deferred_errors_consistently() {
    // Table-level FK with DEFERRABLE INITIALLY DEFERRED should error,
    // matching column-level FK behavior (column_option.rs:252-254)
    let sql = r#"
        CREATE TABLE parent (id INTEGER PRIMARY KEY);
        CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER,
            FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
        );
    "#;

    let result = translate(sql);
    assert!(
        result.is_err(),
        "Table-level FK with DEFERRABLE should error (matching column-level), got:\n{}",
        result.unwrap()
    );
}

#[test]
fn fk_basic_without_characteristics_works() {
    // Simple FK without characteristics should translate cleanly
    let sql = r#"
        CREATE TABLE parent (id INTEGER PRIMARY KEY);
        CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER,
            FOREIGN KEY (parent_id) REFERENCES parent(id)
        );
    "#;

    let result = translate(sql);
    assert!(
        result.is_ok(),
        "Simple FK without characteristics should translate fine, got err: {:?}",
        result.err()
    );
    let output = result.unwrap();
    assert!(output.contains("FOREIGN KEY"), "FK should be preserved in output");
    assert!(output.contains("REFERENCES"), "REFERENCES should be preserved in output");
}
