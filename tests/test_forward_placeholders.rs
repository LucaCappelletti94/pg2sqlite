//! Forward translation must map PostgreSQL numbered parameters to SQLite
//! numbered placeholders. PostgreSQL emits only `$N`; SQLite's canonical
//! numbered form is `?N`. Preserving the number keeps the bind index intact so
//! the same bind vector drives both the PostgreSQL original and the SQLite
//! translation, and so a later reverse translation round trips exactly.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};
use sqlparser::{
    dialect::{PostgreSqlDialect, SQLiteDialect},
    parser::Parser,
};

const SCHEMA: &str = "CREATE TABLE t (a INT, b INT, c INT, d INT);";

fn forward(pg_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = Parser::parse_sql(&PostgreSqlDialect {}, pg_sql).unwrap();
    stmts
        .iter()
        .flat_map(|s| s.translate(&schema, &options).unwrap())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn reparse_sqlite(sql: &str) {
    Parser::parse_sql(&SQLiteDialect {}, sql)
        .unwrap_or_else(|e| panic!("forward output must reparse under SQLiteDialect: {e}\n{sql}"));
}

#[test]
fn numbered_parameters_map_to_sqlite_placeholders() {
    let out = forward("SELECT * FROM t WHERE a > $1 AND b = $2");
    assert_eq!(out, "SELECT * FROM t WHERE a > ?1 AND b = ?2");
    assert!(!out.contains('$'), "{out}");
    reparse_sqlite(&out);
}

#[test]
fn number_is_preserved_not_reassigned() {
    // Forward mapping is a direct number-preserving rewrite, unlike the reverse
    // assignment rule for bare `?`.
    let out = forward("SELECT * FROM t WHERE a > $2 AND b = $1");
    assert_eq!(out, "SELECT * FROM t WHERE a > ?2 AND b = ?1");
    reparse_sqlite(&out);
}

#[test]
fn parameter_in_limit_and_offset() {
    let out = forward("SELECT * FROM t LIMIT $1 OFFSET $2");
    assert_eq!(out, "SELECT * FROM t LIMIT ?1 OFFSET ?2");
    reparse_sqlite(&out);
}

#[test]
fn parameter_in_in_list() {
    let out = forward("SELECT * FROM t WHERE a IN ($1, $2)");
    assert_eq!(out, "SELECT * FROM t WHERE a IN (?1, ?2)");
    reparse_sqlite(&out);
}

#[test]
fn parameter_in_between_bounds() {
    let out = forward("SELECT * FROM t WHERE a BETWEEN $1 AND $2");
    assert_eq!(out, "SELECT * FROM t WHERE a BETWEEN ?1 AND ?2");
    reparse_sqlite(&out);
}

#[test]
fn parameter_as_function_argument() {
    let out = forward("SELECT length($1) FROM t");
    assert_eq!(out, "SELECT length(?1) FROM t");
    reparse_sqlite(&out);
}

#[test]
fn parameter_in_select_list_expression() {
    let out = forward("SELECT a + $1 FROM t");
    assert_eq!(out, "SELECT a + ?1 FROM t");
    reparse_sqlite(&out);
}

#[test]
fn duplicate_parameter_reference_is_preserved() {
    let out = forward("SELECT * FROM t WHERE a = $1 AND b = $1");
    assert_eq!(out, "SELECT * FROM t WHERE a = ?1 AND b = ?1");
    reparse_sqlite(&out);
}

#[test]
fn update_and_delete_and_insert_parameters() {
    assert_eq!(forward("UPDATE t SET a = $1 WHERE b = $2"), "UPDATE t SET a = ?1 WHERE b = ?2");
    assert_eq!(forward("DELETE FROM t WHERE a = $1"), "DELETE FROM t WHERE a = ?1");
    assert_eq!(
        forward("INSERT INTO t (a, b) VALUES ($1, $2)"),
        "INSERT INTO t (a, b) VALUES (?1, ?2)"
    );
}

#[test]
fn statement_without_parameters_is_unchanged() {
    let out = forward("SELECT a FROM t WHERE a > 1 ORDER BY a");
    assert_eq!(out, "SELECT a FROM t WHERE a > 1 ORDER BY a");
    reparse_sqlite(&out);
}

#[test]
fn every_forward_output_reparses_as_sqlite_without_dollar_parameters() {
    let corpus = [
        "SELECT * FROM t WHERE a > $1 AND b = $2",
        "SELECT * FROM t WHERE a > $2 AND b = $1",
        "SELECT * FROM t LIMIT $1 OFFSET $2",
        "SELECT * FROM t WHERE a IN ($1, $2)",
        "SELECT * FROM t WHERE a BETWEEN $1 AND $2",
        "SELECT length($1) FROM t",
        "SELECT a + $1 FROM t",
        "UPDATE t SET a = $1 WHERE b = $2",
        "DELETE FROM t WHERE a = $1",
        "INSERT INTO t (a, b) VALUES ($1, $2)",
    ];
    for sql in corpus {
        let out = forward(sql);
        assert!(!out.contains('$'), "dollar parameter leaked for {sql}: {out}");
        reparse_sqlite(&out);
    }
}
