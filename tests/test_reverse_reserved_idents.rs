//! Reverse translation must quote bare identifier names that PostgreSQL
//! evaluates as pseudo-expressions (`user`, `current_date`, etc.) when the
//! schema confirms they are column references.
//!
//! Measured on PostgreSQL 17.3:
//!   CREATE TABLE tu (id INT, "user" TEXT);
//!   INSERT INTO tu VALUES (1, 'colval');
//!   SELECT user FROM tu;           -- returns 'postgres' (current role)
//!   SELECT "user" FROM tu;         -- returns 'colval'
//!
//! SQLite answers 'colval' for both spellings; the bare form is the defect.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    let pg = stmts.first().expect("one statement").to_string();
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("output must parse as PostgreSQL: {e}\n{pg}"));
    pg
}

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    translator.reverse_sql(sqlite_sql, &schema, &options).unwrap_err().to_string()
}

// Schema with a column named `user` — the prime pseudo-expression collision.
const USER_SCHEMA: &str = r#"CREATE TABLE tu (id INTEGER PRIMARY KEY, "user" TEXT NOT NULL);"#;

#[test]
fn user_column_in_select_is_quoted() {
    // Replica: SELECT user FROM tu (SQLite reads column, PG reads role)
    // Fix: emitted SQL must quote "user"
    let pg = reverse(USER_SCHEMA, r#"SELECT user FROM tu"#);
    assert!(pg.contains(r#""user""#), "expected double-quoted user: {pg}");
}

#[test]
fn already_quoted_user_column_passes_through() {
    // Already-quoted input needs no extra quoting.
    let pg = reverse(USER_SCHEMA, r#"SELECT "user" FROM tu"#);
    assert!(pg.contains(r#""user""#), "expected double-quoted user: {pg}");
}

// current_date / current_timestamp parse as special AST nodes in SQLite dialect
// (not Expr::Identifier), so both engines already agree — no quoting needed.
// Test localtime and current_schema instead: SQLite has no built-in for them.

#[test]
fn current_user_column_in_select_is_quoted() {
    let schema = r#"CREATE TABLE tcu (id INTEGER PRIMARY KEY, current_user TEXT);"#;
    let pg = reverse(schema, "SELECT current_user FROM tcu");
    assert!(pg.contains(r#""current_user""#), "expected quoted current_user: {pg}");
}

#[test]
fn current_schema_column_in_select_is_quoted() {
    let schema = r#"CREATE TABLE tcs (id INTEGER PRIMARY KEY, current_schema TEXT);"#;
    let pg = reverse(schema, "SELECT current_schema FROM tcs");
    assert!(pg.contains(r#""current_schema""#), "expected quoted current_schema: {pg}");
}

#[test]
fn session_user_column_in_select_is_quoted() {
    let schema = r#"CREATE TABLE tu2 (id INTEGER PRIMARY KEY, session_user TEXT);"#;
    let pg = reverse(schema, "SELECT session_user FROM tu2");
    assert!(pg.contains(r#""session_user""#), "expected quoted session_user: {pg}");
}

#[test]
fn user_column_without_schema_is_refused() {
    // No schema → can't confirm it's a column → refuse.
    let empty_schema = r#"CREATE TABLE other (id INTEGER PRIMARY KEY);"#;
    let err = reverse_err(empty_schema, "SELECT user FROM tu");
    assert!(err.contains("pseudo-expression"), "error should mention pseudo-expression: {err}");
}

#[test]
fn user_column_in_where_clause_is_quoted() {
    let pg = reverse(USER_SCHEMA, r#"SELECT id FROM tu WHERE user = 'alice'"#);
    assert!(pg.contains(r#""user""#), "expected quoted user in WHERE: {pg}");
}
