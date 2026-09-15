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
//!
//! A name no column answers to is nobody's business here, and both guards
//! leave that statement to the engine that will refuse it.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rusqlite::Connection;

mod helpers;
use helpers::translate_pg as translate;

/// Applies every emitted statement, which proves SQLite takes the output.
fn apply(connection: &Connection, statements: &[String]) {
    for statement in statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("SQLite rejected emitted SQL: {error}\n{statement}"));
    }
}

/// What SQLite answers for the last emitted statement, the ones before it
/// having been applied.
fn engine_refusal(connection: &Connection, statements: &[String]) -> String {
    let (last, setup) = statements.split_last().expect("at least one statement");
    apply(connection, setup);
    connection
        .execute_batch(&format!("{last};"))
        .expect_err("SQLite has to be the one refusing this")
        .to_string()
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

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    apply(&connection, &statements);
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
    apply(&Connection::open_in_memory().expect("in-memory SQLite"), &statements);
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

/// A policy-bearing table whose `note` column the database fills in, with
/// `returning` naming what the insert reads back.
fn policy_schema(returning: &str) -> String {
    format!(
        "CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    \"Note\" TEXT DEFAULT 'unset'
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT WITH CHECK (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (owner_id) VALUES (42) RETURNING {returning};"
    )
}

fn monitor_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

#[test]
fn returning_a_quoted_defaulted_column_is_refused_on_a_policy_table() {
    let error = Pg2Sqlite::default()
        .sql(&policy_schema("\"Note\""))
        .expect("parse")
        .translate(&monitor_options())
        .expect_err("a defaulted column cannot be answered from the view row")
        .to_string();

    assert!(error.contains("RETURNING reads Note"), "the refusal must name the column: {error}");
    assert!(
        error.contains("with_strict_rls_validation"),
        "the refusal must name the option that makes it work: {error}"
    );
}

#[test]
fn returning_an_undeclared_column_is_left_to_the_engine() {
    let statements: Vec<String> = Pg2Sqlite::default()
        .sql(&policy_schema("nosuch"))
        .expect("parse")
        .translate(&monitor_options())
        .expect("a name no column answers to is not a column the database fills in")
        .iter()
        .map(ToString::to_string)
        .collect();

    let insert = statements.last().expect("an insert statement");
    assert!(insert.contains("RETURNING nosuch"), "the name must reach SQLite intact: {insert}");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .create_scalar_function(
            "current_app_user",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |_| Ok(42i64),
        )
        .expect("register the session variable function");
    let error = engine_refusal(&connection, &statements);
    assert!(error.contains("nosuch"), "SQLite must name the column it lacks: {error}");
}

#[test]
fn an_assignment_to_an_undeclared_column_is_left_to_the_engine() {
    let statements = translate(
        "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, note TEXT);\n\
         UPDATE t SET missing = 5 WHERE note = 'x';",
        &Pg2SqliteOptions::default(),
    )
    .expect("the identity guard has nothing to say about a column the schema lacks");

    let update = statements.last().expect("an update statement");
    assert!(update.contains("missing = 5"), "the assignment must reach SQLite intact: {update}");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    let error = engine_refusal(&connection, &statements);
    assert!(error.contains("missing"), "SQLite must name the column it lacks: {error}");
}
