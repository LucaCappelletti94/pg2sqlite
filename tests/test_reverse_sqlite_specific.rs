//! Reverse translation must refuse SQLite-specific database qualifiers and
//! system catalog names that have no PostgreSQL equivalent.
//!
//! Measured on PostgreSQL 17.3:
//!   SELECT * FROM main.t;       → ERROR: relation "main.t" does not exist
//!   SELECT * FROM temp.t;       → ERROR: relation "temp.t" does not exist
//!   SELECT * FROM otherdb.t;    → ERROR: relation "otherdb.t" does not exist
//!   SELECT name FROM sqlite_master WHERE type='table';
//!                               → ERROR: relation "sqlite_master" does not
//! exist
//!
//! The INSERT path already refused main./temp. (via the schema resolver) but
//! with a misleading message; SELECT/UPDATE/DELETE passed through silently.
//! This test covers both the consistent refusal and the improved messages.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    translator.reverse_sql(sqlite_sql, &schema, &options).unwrap_err().to_string()
}

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, v TEXT);";

// ─── main. prefix
// ─────────────────────────────────────────────────────────────

#[test]
fn reverse_select_with_main_prefix_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT * FROM main.t");
    assert!(err.to_lowercase().contains("main"), "error must mention 'main': {err}");
}

#[test]
fn reverse_update_with_main_prefix_is_refused() {
    let err = reverse_err(SCHEMA, "UPDATE main.t SET v = 'x' WHERE id = 1");
    assert!(err.to_lowercase().contains("main"), "error must mention 'main': {err}");
}

#[test]
fn reverse_delete_with_main_prefix_is_refused() {
    let err = reverse_err(SCHEMA, "DELETE FROM main.t WHERE id = 1");
    assert!(err.to_lowercase().contains("main"), "error must mention 'main': {err}");
}

#[test]
fn reverse_insert_with_main_prefix_gives_informative_message() {
    // INSERT already refused; message should now name 'main' as a database
    // qualifier.
    let err = reverse_err(SCHEMA, "INSERT INTO main.t (id, v) VALUES (1, 'x')");
    assert!(err.to_lowercase().contains("main"), "error must mention 'main': {err}");
}

// ─── temp. prefix
// ─────────────────────────────────────────────────────────────

#[test]
fn reverse_select_with_temp_prefix_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT * FROM temp.t");
    assert!(err.to_lowercase().contains("temp"), "error must mention 'temp': {err}");
}

// ─── sqlite_master
// ────────────────────────────────────────────────────────────

#[test]
fn reverse_select_from_sqlite_master_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT name FROM sqlite_master WHERE type = 'table'");
    assert!(
        err.to_lowercase().contains("sqlite_master"),
        "error must mention sqlite_master: {err}"
    );
}

#[test]
fn reverse_select_from_sqlite_schema_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT name FROM sqlite_schema WHERE type = 'table'");
    assert!(
        err.to_lowercase().contains("sqlite_schema"),
        "error must mention sqlite_schema: {err}"
    );
}
