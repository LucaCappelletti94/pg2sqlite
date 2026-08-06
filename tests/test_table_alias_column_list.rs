//! `FROM t AS x (a, b)` renames a table's columns in PostgreSQL, and SQLite
//! accepts no column list on a table alias, so the list used to reach SQLite
//! verbatim and fail as `near "(": syntax error` (R105). The rewrite projects
//! the declared columns under their new names inside a derived table,
//! `(SELECT id AS a, s AS b FROM t) AS x`, the same shape the UNNEST lowering
//! already uses for the same reason.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use rusqlite::Connection;

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
     INSERT INTO t VALUES (1, 'one');";

/// Runs `query` against the fixture and returns the first column of each row.
fn rows(query: &str) -> Vec<Option<String>> {
    run_translated_with(&format!("{FIXTURE} {query};"), &Pg2SqliteOptions::default())
}

/// Translates `query` against the fixture, applies everything but the probe,
/// and returns the column names the prepared probe exposes.
fn probe_column_names(query: &str) -> Vec<String> {
    let mut statements = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let probe = statements.pop().expect("the script should emit at least one statement");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let prepared = connection
        .prepare(&probe)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"));
    prepared.column_names().into_iter().map(str::to_string).collect()
}

fn refuse(query: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this alias list has no faithful translation")
        .to_string()
}

/// The defect: the list reached SQLite verbatim. The rewrite must expose the
/// renamed columns and the emitted query must run.
#[test]
fn a_full_alias_list_renames_both_columns() {
    assert_eq!(probe_column_names("SELECT * FROM t AS x (a, b)"), vec!["a", "b"]);
    assert_eq!(rows("SELECT x.b FROM t AS x (a, b) WHERE x.a = 1"), vec![Some("one".to_string())]);
}

/// A list shorter than the table renames the leading columns and keeps the
/// rest under their declared names, PostgreSQL's positional rule.
#[test]
fn a_shorter_alias_list_renames_the_prefix() {
    assert_eq!(probe_column_names("SELECT * FROM t AS x (a)"), vec!["a", "s"]);
    assert_eq!(rows("SELECT x.s FROM t AS x (a) WHERE x.a = 1"), vec![Some("one".to_string())]);
}

/// PostgreSQL refuses a list longer than the table's column count, so the
/// translation refuses it too rather than inventing columns.
#[test]
fn a_longer_alias_list_is_refused() {
    let error = refuse("SELECT * FROM t AS x (a, b, c)");
    assert!(
        error.contains('3') && error.contains('2'),
        "the refusal must carry both counts, got: {error}"
    );
}

/// The rewrite needs the declared column list, so a relation the schema does
/// not declare is refused naming it.
#[test]
fn an_alias_list_over_an_undeclared_relation_is_refused() {
    let error = refuse("SELECT * FROM absent AS x (a)");
    assert!(error.contains("absent"), "the refusal must name the relation, got: {error}");
}

/// A data type in the list belongs to a function returning `record`, and
/// PostgreSQL refuses it on a table.
#[test]
fn a_data_type_in_the_alias_list_is_refused() {
    let error = refuse("SELECT * FROM t AS x (a INT)");
    assert!(error.contains("data type"), "the refusal must name the construct, got: {error}");
}
