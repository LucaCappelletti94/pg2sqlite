//! A column declared under a quoted mixed-case name is found by every schema
//! lookup the translator makes.
//!
//! Three statements PostgreSQL accepts, each one refused or mistranslated
//! before the lookups took a comparison:
//!
//! - `INSERT INTO "Tbl" (id, "ColA") VALUES (1, DEFAULT)` was refused as a
//!   column the translation schema does not declare.
//! - `UPDATE t SET "Id" = 5` on a `GENERATED ALWAYS AS IDENTITY` key passed
//!   through, where PostgreSQL refuses anything but `DEFAULT`.
//! - `RETURNING "Note"` over a policy-bearing table was emitted against the
//!   view, whose row would answer NULL for a defaulted column.
//!
//! The lookups fold, so one column answers to `"ColA"` and to `cola`, which
//! costs nothing because a table carrying both is refused where it is created.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rusqlite::Connection;

mod helpers;
use helpers::translate_pg as translate;

/// Applies every emitted statement, which proves SQLite takes the output.
fn apply(statements: &[String]) -> Connection {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("SQLite rejected emitted SQL: {error}\n{statement}"));
    }
    connection
}

#[test]
fn a_quoted_column_carries_its_default_into_the_values_row() {
    let statements = translate(
        "CREATE TABLE \"Tbl\" (id INT PRIMARY KEY, \"ColA\" INT DEFAULT 7);\n\
         INSERT INTO \"Tbl\" (id, \"ColA\") VALUES (1, DEFAULT);",
        &Pg2SqliteOptions::default(),
    )
    .expect("a quoted column name resolves against the schema");

    let insert = statements.last().expect("an insert statement");
    assert!(insert.contains("VALUES (1, 7)"), "the declared default must be substituted: {insert}");

    let connection = apply(&statements);
    let stored: i64 = connection
        .query_row("SELECT \"ColA\" FROM \"Tbl\" WHERE id = 1", [], |row| row.get(0))
        .expect("the inserted row");
    assert_eq!(stored, 7);
}

#[test]
fn the_folded_spelling_reaches_the_same_quoted_column() {
    let statements = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, \"ColA\" INT DEFAULT 7);\n\
         INSERT INTO t (id, cola) VALUES (1, DEFAULT);",
        &Pg2SqliteOptions::default(),
    )
    .expect("the emitted table holds one column under either spelling");

    let insert = statements.last().expect("an insert statement");
    assert!(insert.contains("VALUES (1, 7)"), "the declared default must be substituted: {insert}");
    apply(&statements);
}

#[test]
fn a_quoted_generated_always_key_refuses_a_written_value() {
    let error = translate(
        "CREATE TABLE t (\"Id\" INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, note TEXT);\n\
         UPDATE t SET \"Id\" = 5 WHERE note = 'x';",
        &Pg2SqliteOptions::default(),
    )
    .expect_err("PostgreSQL answers that the column can only be updated to DEFAULT")
    .to_string();

    assert!(
        error.contains("GENERATED ALWAYS AS IDENTITY"),
        "the refusal must name what makes the assignment impossible: {error}"
    );
    assert!(error.contains("Id"), "the refusal must name the column: {error}");
}

#[test]
fn returning_a_quoted_defaulted_column_is_refused_on_a_policy_table() {
    let schema = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    \"Note\" TEXT DEFAULT 'unset'
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT WITH CHECK (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (owner_id) VALUES (42) RETURNING \"Note\";";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    let error = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(&options)
        .expect_err("a defaulted column cannot be answered from the view row")
        .to_string();

    assert!(error.contains("RETURNING reads Note"), "the refusal must name the column: {error}");
    assert!(
        error.contains("with_strict_rls_validation"),
        "the refusal must name the option that makes it work: {error}"
    );
}
