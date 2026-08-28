//! Tests for GROUP L: Table constraint catch-all.
//!
//! L1: PrimaryKey/Unique expression indexes should have their column
//!     expressions translated through the IndexColumn translator.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

fn default_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

#[test]
fn pk_constraint_columns_translated() {
    // A simple PRIMARY KEY constraint should pass through correctly
    let sql = r#"
        CREATE TABLE t (
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
        "#;
    let options = default_options();
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(lower.contains("primary key"), "PRIMARY KEY should appear in output: {sql_str}");
    // Execute the emitted DDL to prove real SQLite accepts the translated schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn unique_constraint_columns_translated() {
    // A UNIQUE constraint with expression should translate the expression
    let sql = r#"
        CREATE TABLE t (
            id INTEGER PRIMARY KEY,
            email TEXT,
            UNIQUE (email)
        );
        "#;
    let options = default_options();
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(lower.contains("unique"), "UNIQUE should appear in output: {sql_str}");
    // Execute the emitted DDL to prove real SQLite accepts the translated schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn pk_with_deferrable_characteristics_errors() {
    // PrimaryKey with DEFERRABLE should translate characteristics
    // The characteristics translator returns a typed refusal.
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

/// Translates and returns the error message, or the emitted SQL when the
/// translation unexpectedly succeeds.
fn reject(sql: &str) -> String {
    match translate_sql(sql, &default_options()) {
        Err(error) => error,
        Ok(emitted) => panic!("expected a rejection, got: {emitted}"),
    }
}

/// PostgreSQL's exclusion constraint has no SQLite form, and used to be copied
/// into the CREATE TABLE body verbatim, where SQLite answers `near "USING":
/// syntax error`.
#[test]
fn exclude_constraint_is_rejected() {
    let error = reject(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, room INTEGER, EXCLUDE USING btree (room WITH =));",
    );
    assert!(
        error.to_uppercase().contains("EXCLUDE"),
        "expected the error to name the constraint, got {error}"
    );
}

/// Guards the fix from refusing the constraints that do translate.
#[test]
fn the_translatable_constraints_still_translate() {
    let sql = "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE t (
            id INTEGER,
            parent_id INTEGER,
            s TEXT,
            PRIMARY KEY (id),
            UNIQUE (s),
            FOREIGN KEY (parent_id) REFERENCES parent (id),
            CHECK (id > 0)
         );";
    let sql_str =
        translate_sql(sql, &default_options()).expect("every translatable constraint must survive");
    let lower = sql_str.to_lowercase();
    for expected in ["primary key", "unique", "foreign key", "check"] {
        assert!(lower.contains(expected), "{expected} must survive: {sql_str}");
    }
    // Execute the emitted DDL to prove real SQLite accepts the translated schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&default_options()).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// `NULLS NOT DISTINCT` makes two NULL rows collide, which SQLite cannot do:
/// its unique indexes always treat NULLs as distinct. Verified on both, where
/// PostgreSQL 16 answers `duplicate key value violates unique constraint` for
/// the second NULL and SQLite accepts it. Dropping the clause would therefore
/// change which rows the database accepts, so it is refused rather than
/// cleared. The clause used to reach the output and fail with `near "NULLS":
/// syntax error`.
#[test]
fn unique_nulls_not_distinct_is_rejected() {
    let error =
        reject("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, UNIQUE NULLS NOT DISTINCT (s));");
    assert!(
        error.to_uppercase().contains("NULLS NOT DISTINCT"),
        "expected the error to name the clause, got {error}"
    );
}

/// `NULLS DISTINCT` is PostgreSQL's default and is exactly what SQLite does, so
/// the clause is dropped rather than refused. Guards the rejection above from
/// swallowing the harmless spelling.
#[test]
fn unique_nulls_distinct_is_translated() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, UNIQUE NULLS DISTINCT (s));";
    let sql_str = translate_sql(sql, &default_options())
        .expect("NULLS DISTINCT matches SQLite and must translate");
    assert!(
        !sql_str.to_uppercase().contains("NULLS"),
        "the clause has no SQLite form and must not reach the output: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&default_options()).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}
