//! Tests for five DDL constraint-clause defects, each measured against
//! PostgreSQL 17 and SQLite 3.51.
//!
//! Test order mirrors the assignment:
//!   1. ALTER TABLE … DROP COLUMN CASCADE / RESTRICT
//!   2. UNIQUE / PRIMARY KEY … INCLUDE (…)
//!   3. CHECK … NOT ENFORCED / ENFORCED
//!   4. GENERATED ALWAYS AS IDENTITY (START WITH …)
//!   5. PRIMARY KEY / UNIQUE DEFERRABLE at column level

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use rusqlite::Connection;

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn translate(sql: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(sql)?.translate_to_sql(&opts())
}

fn translate_with_warnings(
    sql: &str,
) -> Result<(Vec<String>, Vec<TranslationWarning>), pg2sqlite::errors::Error> {
    let report = Pg2Sqlite::default().sql(sql)?.translate_with_report(&opts())?;
    let stmts = report.statements.iter().map(|s| s.to_string()).collect();
    Ok((stmts, report.warnings))
}

/// Executes every statement through rusqlite and returns the first error.
fn sqlite_exec(stmts: &[String]) -> Result<(), rusqlite::Error> {
    let conn = Connection::open_in_memory()?;
    for s in stmts {
        conn.execute_batch(s)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 1. ALTER TABLE … DROP COLUMN CASCADE / RESTRICT
// ---------------------------------------------------------------------------

/// PostgreSQL accepts DROP COLUMN x CASCADE. SQLite has no CASCADE semantics,
/// so emitting it would produce `near "CASCADE": syntax error`. The translator
/// must refuse and name the construct.
#[test]
fn drop_column_cascade_is_refused() {
    let sql = "CREATE TABLE t (a INT, b INT);\nALTER TABLE t DROP COLUMN a CASCADE;";
    let err = translate(sql).expect_err("DROP COLUMN CASCADE must be refused");
    let msg = err.to_string();
    assert!(msg.contains("CASCADE") || msg.contains("cascade"), "refusal must name CASCADE: {msg}");
}

/// DROP COLUMN x RESTRICT is the default PostgreSQL behaviour spelled out.
/// The translator must accept it and emit a bare DROP COLUMN.
#[test]
fn drop_column_restrict_translates_and_runs() {
    let sql = "CREATE TABLE t (a INT PRIMARY KEY, b INT);\nALTER TABLE t DROP COLUMN b RESTRICT;";
    let stmts = translate(sql).expect("DROP COLUMN RESTRICT must translate");
    sqlite_exec(&stmts).expect("emitted SQL must run in SQLite");

    // Verify the column is actually gone.
    let conn = Connection::open_in_memory().unwrap();
    for s in &stmts {
        conn.execute_batch(s).unwrap();
    }
    let col_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_table_info('t') WHERE name = 'b'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(col_count, 0, "column b must have been dropped");
}

// ---------------------------------------------------------------------------
// 2. UNIQUE / PRIMARY KEY … INCLUDE (…)
// ---------------------------------------------------------------------------

/// INCLUDE adds payload columns to the backing index; it changes no constraint
/// semantics. The translator must drop the clause with a warning and emit a
/// valid CREATE TABLE that SQLite accepts.
#[test]
fn unique_include_is_dropped_with_warning_and_runs() {
    let sql = "CREATE TABLE t (a INT PRIMARY KEY, b INT, UNIQUE (a) INCLUDE (b));";
    let (stmts, warnings) = translate_with_warnings(sql).expect("UNIQUE … INCLUDE must translate");

    // The emitted DDL must not contain INCLUDE.
    for s in &stmts {
        assert!(!s.to_uppercase().contains("INCLUDE"), "emitted SQL must not contain INCLUDE: {s}");
    }

    // A warning must be emitted for the dropped clause.
    assert!(!warnings.is_empty(), "dropping INCLUDE must be reported as a warning: {warnings:?}");

    // The emitted DDL must be valid SQLite.
    sqlite_exec(&stmts).expect("emitted SQL must run in SQLite");

    // The UNIQUE constraint must still enforce uniqueness.
    let conn = Connection::open_in_memory().unwrap();
    for s in &stmts {
        conn.execute_batch(s).unwrap();
    }
    conn.execute_batch("INSERT INTO t VALUES (1, 10);").expect("first row");
    let err = conn.execute_batch("INSERT INTO t VALUES (1, 20);");
    assert!(err.is_err(), "UNIQUE constraint must still fire after INCLUDE is dropped");
}

/// PRIMARY KEY … INCLUDE must also drop the clause with a warning.
#[test]
fn primary_key_include_is_dropped_with_warning_and_runs() {
    let sql = "CREATE TABLE t (a INT, b INT, PRIMARY KEY (a) INCLUDE (b));";
    let (stmts, warnings) =
        translate_with_warnings(sql).expect("PRIMARY KEY … INCLUDE must translate");

    for s in &stmts {
        assert!(!s.to_uppercase().contains("INCLUDE"), "emitted SQL must not contain INCLUDE: {s}");
    }

    assert!(!warnings.is_empty(), "dropping INCLUDE must be reported as a warning: {warnings:?}");

    sqlite_exec(&stmts).expect("emitted SQL must run in SQLite");

    // Primary key must still enforce uniqueness.
    let conn = Connection::open_in_memory().unwrap();
    for s in &stmts {
        conn.execute_batch(s).unwrap();
    }
    conn.execute_batch("INSERT INTO t VALUES (1, 10);").expect("first row");
    let err = conn.execute_batch("INSERT INTO t VALUES (1, 20);");
    assert!(err.is_err(), "PRIMARY KEY constraint must still fire after INCLUDE is dropped");
}

// ---------------------------------------------------------------------------
// 3. CHECK … NOT ENFORCED / ENFORCED
// ---------------------------------------------------------------------------

/// `CHECK (x > 0) NOT ENFORCED` is a MySQL spelling; PostgreSQL 17 rejects it.
/// The translator must refuse it at both the column and table constraint level.
#[test]
fn check_not_enforced_column_level_is_refused() {
    // sqlparser may or may not parse this under PostgreSqlDialect; if it does,
    // the translator must refuse it.
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT CHECK (x > 0) NOT ENFORCED);";
    match translate(sql) {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("ENFORCED") || msg.contains("enforced"),
                "refusal must name NOT ENFORCED: {msg}"
            );
        }
        Ok(stmts) => {
            // If the parser accepted it and we emitted SQL, SQLite must also
            // accept it — but this path indicates an unhandled passthrough.
            // Force the test to observe the SQL so we can see what happened.
            panic!(
                "CHECK NOT ENFORCED passed through without refusal; emitted: {}",
                stmts.join("; ")
            );
        }
    }
}

/// Table-level CHECK … NOT ENFORCED must also be refused.
#[test]
fn check_not_enforced_table_level_is_refused() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT, CHECK (x > 0) NOT ENFORCED);";
    match translate(sql) {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("ENFORCED") || msg.contains("enforced"),
                "refusal must name NOT ENFORCED: {msg}"
            );
        }
        Ok(stmts) => {
            panic!(
                "CHECK NOT ENFORCED passed through without refusal; emitted: {}",
                stmts.join("; ")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4. GENERATED ALWAYS AS IDENTITY (START WITH 100)
// ---------------------------------------------------------------------------

/// A bare GENERATED ALWAYS AS IDENTITY with no sequence options must keep
/// working exactly as before (the rowid alias path).
#[test]
fn bare_identity_still_works_as_rowid_alias() {
    let sql = "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);";
    let stmts = translate(sql).expect("bare GENERATED ALWAYS AS IDENTITY must still translate");
    sqlite_exec(&stmts).expect("emitted SQL must run in SQLite");

    // The rowid auto-assigns.
    let conn = Connection::open_in_memory().unwrap();
    for s in &stmts {
        conn.execute_batch(s).unwrap();
    }
    conn.execute_batch("INSERT INTO t DEFAULT VALUES;").unwrap();
    let id: i64 = conn.query_row("SELECT id FROM t", [], |r| r.get(0)).unwrap();
    assert_ne!(id, 0, "rowid alias must auto-assign");
}

/// GENERATED ALWAYS AS IDENTITY (START WITH 100) must be refused: the rowid
/// alias starts at 1, not 100, so silently dropping the option changes results.
#[test]
fn identity_with_start_with_is_refused() {
    let sql = "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY (START WITH 100) PRIMARY KEY);";
    let err = translate(sql).expect_err("IDENTITY (START WITH …) must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("START") || msg.contains("sequence"),
        "refusal must name the sequence option or START WITH: {msg}"
    );
}

/// INCREMENT BY is equally unhonorable.
#[test]
fn identity_with_increment_by_is_refused() {
    let sql = "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY (INCREMENT BY 5) PRIMARY KEY);";
    let err = translate(sql).expect_err("IDENTITY (INCREMENT BY …) must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("INCREMENT") || msg.contains("sequence"),
        "refusal must name the sequence option: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 5. DEFERRABLE at column level for PRIMARY KEY and UNIQUE
// ---------------------------------------------------------------------------

/// Column-level PRIMARY KEY DEFERRABLE must be refused: SQLite defers only
/// foreign keys, so a deferrable primary key silently becomes immediate.
#[test]
fn column_level_primary_key_deferrable_is_refused() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY DEFERRABLE INITIALLY DEFERRED, x INT);";
    let err = translate(sql).expect_err("column-level PRIMARY KEY DEFERRABLE must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("DEFERRABLE") || msg.contains("deferr"),
        "refusal must name DEFERRABLE: {msg}"
    );
}

/// Column-level UNIQUE DEFERRABLE must also be refused for the same reason.
#[test]
fn column_level_unique_deferrable_is_refused() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT UNIQUE DEFERRABLE);";
    let err = translate(sql).expect_err("column-level UNIQUE DEFERRABLE must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("DEFERRABLE") || msg.contains("deferr"),
        "refusal must name DEFERRABLE: {msg}"
    );
}

/// Table-level PRIMARY KEY DEFERRABLE is already refused; this pin guards
/// regression.
#[test]
fn table_level_primary_key_deferrable_is_refused() {
    let sql = "CREATE TABLE t (id INT, PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED);";
    let err = translate(sql).expect_err("table-level PRIMARY KEY DEFERRABLE must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("DEFERRABLE") || msg.contains("deferr"),
        "refusal must name DEFERRABLE: {msg}"
    );
}

/// Table-level UNIQUE DEFERRABLE is already refused; this pin guards
/// regression.
#[test]
fn table_level_unique_deferrable_is_refused() {
    let sql = "CREATE TABLE t (id INT, UNIQUE (id) DEFERRABLE);";
    let err = translate(sql).expect_err("table-level UNIQUE DEFERRABLE must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("DEFERRABLE") || msg.contains("deferr"),
        "refusal must name DEFERRABLE: {msg}"
    );
}
