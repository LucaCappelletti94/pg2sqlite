//! Tests for CREATE VIEW translation (basic, IF NOT EXISTS, TEMPORARY,
//! MATERIALIZED error, OR REPLACE rewrite) and the ALTER VIEW spelling of the
//! same redefinition.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

fn translate(sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

fn translate_joined(sql: &str) -> Result<String, String> {
    translate(sql).map(|v| v.join("\n"))
}

#[test]
fn basic_view_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("CREATE VIEW v"), "Expected CREATE VIEW: {output}");
    sqlite_accepts_all(sql);
}

#[test]
fn view_if_not_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE VIEW IF NOT EXISTS v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
    sqlite_accepts_all(sql);
}

#[test]
fn temporary_view() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               CREATE TEMPORARY VIEW v AS SELECT * FROM t;";
    let output = translate_joined(sql).unwrap();
    assert!(output.contains("VIEW"), "Expected VIEW: {output}");
    sqlite_accepts_all(sql);
}

#[test]
fn materialized_view_produces_error() {
    let result = translate("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(result.is_err(), "Expected error for MATERIALIZED VIEW");
    let err = result.unwrap_err();
    assert!(err.contains("MATERIALIZED"), "Expected MATERIALIZED in error: {err}");
}

#[test]
fn or_replace_view_emits_drop_then_create() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               CREATE OR REPLACE VIEW v AS SELECT * FROM t;";
    let stmts = translate(sql).unwrap();
    // Table DDL + DROP VIEW IF EXISTS + CREATE VIEW = 3 statements
    assert_eq!(stmts.len(), 3, "Expected 3 statements, got {}: {stmts:?}", stmts.len());
    let drop = &stmts[1];
    let create = &stmts[2];
    assert!(
        drop.to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Second statement should be DROP VIEW IF EXISTS, got: {drop}"
    );
    assert!(
        create.to_uppercase().contains("CREATE VIEW"),
        "Third statement should be CREATE VIEW, got: {create}"
    );
    assert!(
        !create.to_uppercase().contains("OR REPLACE"),
        "CREATE VIEW should not contain OR REPLACE: {create}"
    );
}

#[test]
fn materialized_view_still_errors() {
    let result = translate_joined("CREATE MATERIALIZED VIEW mv AS SELECT 1;");
    assert!(result.is_err(), "Expected error for materialized view");
    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Expected error for materialized view, got empty");
}

#[test]
fn create_or_replace_view_emits_drop_and_create() {
    // CREATE OR REPLACE VIEW is now translated to DROP VIEW IF EXISTS + CREATE VIEW
    let pg = "CREATE OR REPLACE VIEW v AS SELECT 1;";
    let sql = translate_joined(pg).unwrap();
    assert!(
        sql.to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Expected DROP VIEW IF EXISTS in output: {sql}"
    );
    assert!(sql.to_uppercase().contains("CREATE VIEW"), "Expected CREATE VIEW in output: {sql}");
    sqlite_accepts_all(pg);
}

/// `ALTER VIEW v AS ...` redefines a view, which is what `CREATE OR REPLACE
/// VIEW` does, so the two must emit the same thing rather than one of them
/// being refused.
#[test]
fn alter_view_emits_what_create_or_replace_emits() {
    let definition = "CREATE TABLE t (id INT PRIMARY KEY, n INT);";
    let altered =
        translate(&format!("{definition} ALTER VIEW v AS SELECT n FROM t WHERE n > 1;")).unwrap();
    let replaced = translate(&format!(
        "{definition} CREATE OR REPLACE VIEW v AS SELECT n FROM t WHERE n > 1;"
    ))
    .unwrap();

    assert_eq!(altered, replaced, "the two spellings should emit the same statements");
}

/// The column list form redefines the view's output names, so it has to travel
/// too.
#[test]
fn alter_view_carries_its_column_list() {
    let definition = "CREATE TABLE t (id INT PRIMARY KEY, n INT);";
    let altered =
        translate(&format!("{definition} ALTER VIEW v (label) AS SELECT n FROM t;")).unwrap();
    let replaced =
        translate(&format!("{definition} CREATE OR REPLACE VIEW v (label) AS SELECT n FROM t;"))
            .unwrap();

    assert_eq!(altered, replaced, "the column list should survive the same way");
}

/// The redefinition takes effect: reading the view after it returns the new
/// query's rows, not the old one's.
#[test]
fn an_altered_view_returns_the_new_definition() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         INSERT INTO t VALUES (1, 10), (2, 20);
         CREATE VIEW v AS SELECT n FROM t WHERE n = 10;
         ALTER VIEW v AS SELECT n FROM t WHERE n = 20;
         SELECT n FROM v;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("20".to_string())], "the view should carry the new definition");
}

/// Only a redefinition translates. A view option has no SQLite counterpart and
/// is still refused, through the same check `CREATE VIEW` already applies.
#[test]
fn alter_view_with_options_is_still_refused() {
    let result = translate_joined(
        "CREATE TABLE t (id INT PRIMARY KEY);
         ALTER VIEW v WITH (security_barrier = true) AS SELECT * FROM t;",
    );
    assert!(result.is_err(), "a view option should be refused, got: {result:?}");
}

/// Executes every statement emitted for `pg` against an in-memory SQLite.
/// The translated SQL is dynamically generated by the translator; rusqlite
/// execute_batch is used per the R79 forward-direction rule.
fn sqlite_accepts_all(pg: &str) {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in translate(pg).expect("translate") {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
    }
}
