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
fn fk_deferrable_initially_deferred_translates_consistently() {
    // Table-level FK deferrability translates, matching the column-level FK
    // path. Both go through `ConstraintCharacteristics::translate`. Deferral is
    // exercised against a running database in
    // `tests/test_deferrable_constraints.rs`, so this only pins the two paths
    // agreeing.
    let sql = r#"
        CREATE TABLE parent (id INTEGER PRIMARY KEY);
        CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER,
            FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
        );
    "#;

    let table_level = translate(sql).expect("table-level FK deferrability should translate");
    let column_level = translate(
        r#"
        CREATE TABLE parent (id INTEGER PRIMARY KEY);
        CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
        );
    "#,
    )
    .expect("column-level FK deferrability should translate");

    assert!(
        table_level.contains("DEFERRABLE INITIALLY DEFERRED"),
        "table-level FK should keep the deferral: {table_level}"
    );
    assert!(
        column_level.contains("DEFERRABLE INITIALLY DEFERRED"),
        "column-level FK should keep the deferral: {column_level}"
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
