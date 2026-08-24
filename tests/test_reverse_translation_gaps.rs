//! Tests for GROUP 4: Reverse translation gaps.
//!
//! Covers:
//! - 4a: Missing reverse function mappings (unicode, json_object, json_array)
//! - 4b: Rename within_group translation (OrderByExpr expressions translated)
//! - 4c: Transaction statement passthrough (COMMIT, ROLLBACK, BEGIN, SAVEPOINT,
//!   RELEASE SAVEPOINT)

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val TEXT, num INT);";

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert_ne!(stmts.len(), 0, "reverse output must not be empty");
    stmts[0].to_string()
}

fn reverse_ok(pg_ddl: &str, sqlite_sql: &str) -> Vec<String> {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    stmts.iter().map(ToString::to_string).collect()
}

#[test]
fn reverse_unicode_to_ascii() {
    let pg = reverse(SCHEMA, "SELECT unicode('A') FROM t;");
    assert!(pg.contains("ascii"), "Expected ascii in output: {pg}");
    assert!(!pg.contains("unicode"), "Should not contain unicode: {pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{pg}"));
}

#[test]
fn reverse_json_object_to_json_build_object() {
    let pg = reverse(SCHEMA, "SELECT json_object('key', 'value') FROM t;");
    assert!(pg.contains("json_build_object"), "Expected json_build_object in output: {pg}");
    assert!(!pg.contains("json_object("), "Should not contain json_object(: {pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{pg}"));
}

#[test]
fn reverse_json_array_to_json_build_array() {
    let pg = reverse(SCHEMA, "SELECT json_array(1, 2, 3) FROM t;");
    assert!(pg.contains("json_build_array"), "Expected json_build_array in output: {pg}");
    assert!(!pg.contains("json_array("), "Should not contain json_array(: {pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{pg}"));
}

#[test]
fn reverse_rename_within_group_translates_exprs() {
    let pg = reverse(SCHEMA, "SELECT json_group_array(val ORDER BY datetime('now')) FROM t;");
    assert!(pg.contains("json_agg"), "Expected json_agg: {pg}");
    assert!(pg.contains("NOW()"), "Expected ORDER BY expr to be reverse-translated to NOW(): {pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{pg}"));
}

#[test]
fn reverse_commit() {
    let stmts = reverse_ok(SCHEMA, "COMMIT;");
    assert!(!stmts.is_empty(), "COMMIT should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("COMMIT"), "Expected COMMIT in output: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, out)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{out}"));
}

#[test]
fn reverse_rollback() {
    let stmts = reverse_ok(SCHEMA, "ROLLBACK;");
    assert!(!stmts.is_empty(), "ROLLBACK should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("ROLLBACK"), "Expected ROLLBACK in output: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, out)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{out}"));
}

#[test]
fn reverse_begin() {
    let stmts = reverse_ok(SCHEMA, "BEGIN;");
    assert!(!stmts.is_empty(), "BEGIN should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("BEGIN"), "Expected BEGIN in output: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, out)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{out}"));
}

#[test]
fn reverse_savepoint() {
    let stmts = reverse_ok(SCHEMA, "SAVEPOINT sp1;");
    assert!(!stmts.is_empty(), "SAVEPOINT should reverse OK");
    let out = &stmts[0];
    assert!(
        out.contains("SAVEPOINT") && out.contains("sp1"),
        "Expected SAVEPOINT sp1 in output: {out}"
    );
    Parser::parse_sql(&PostgreSqlDialect {}, out)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{out}"));
}

#[test]
fn reverse_release_savepoint() {
    let stmts = reverse_ok(SCHEMA, "RELEASE SAVEPOINT sp1;");
    assert!(!stmts.is_empty(), "RELEASE SAVEPOINT should reverse OK");
    let out = &stmts[0];
    assert!(
        out.contains("RELEASE") && out.contains("sp1"),
        "Expected RELEASE SAVEPOINT sp1 in output: {out}"
    );
    Parser::parse_sql(&PostgreSqlDialect {}, out)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{out}"));
}

/// Reverse-translate `sqlite_sql` against the given PostgreSQL DDL and return
/// the result, Ok or Err, without unwrapping.
fn reverse_result(pg_ddl: &str, sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options)?;
    Ok(stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; "))
}

// H7: DELETE and UPDATE with ORDER BY or LIMIT must be refused in the reverse
// direction. PostgreSQL has no such grammar for DELETE or UPDATE. Currently the
// clauses pass through, producing SQL the server rejects.

/// DELETE with a WHERE clause, an ORDER BY, and a LIMIT is valid SQLite but
/// invalid PostgreSQL. The reverse direction must refuse it.
#[test]
fn delete_with_where_order_by_limit_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "DELETE FROM t WHERE num = 1 ORDER BY num LIMIT 5");
    assert!(
        result.is_err(),
        "DELETE with ORDER BY and LIMIT must be refused in the reverse direction, got: {:?}",
        result
    );
}

/// DELETE with a bare LIMIT is valid SQLite but invalid PostgreSQL. The reverse
/// direction must refuse it.
#[test]
fn delete_with_limit_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "DELETE FROM t LIMIT 5");
    assert!(
        result.is_err(),
        "DELETE with LIMIT must be refused in the reverse direction, got: {:?}",
        result
    );
}

/// UPDATE with a LIMIT is valid SQLite but invalid PostgreSQL. The reverse
/// direction must refuse it. (The UPDATE ORDER BY LIMIT combination is
/// blocked earlier by the SQLite parser, so only the bare LIMIT form is
/// testable here.)
#[test]
fn update_with_limit_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "UPDATE t SET num = 2 LIMIT 5");
    assert!(
        result.is_err(),
        "UPDATE with LIMIT must be refused in the reverse direction, got: {:?}",
        result
    );
}

// M4: `t.rowid` (compound identifier) must be refused like bare `rowid` is.
// PostgreSQL has no implicit rowid column. Currently the compound form passes
// through, while the bare form is already refused.

/// `t.rowid` passes through today even though `rowid` is refused. The last
/// segment of the compound identifier is `rowid`, which makes the reference
/// equally invalid in PostgreSQL. The reverse direction must refuse it.
#[test]
fn compound_rowid_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "SELECT t.rowid FROM t");
    assert!(
        result.is_err(),
        "compound t.rowid must be refused in the reverse direction, got: {:?}",
        result
    );
    if let Err(err) = &result {
        let msg = err.to_string();
        assert!(msg.to_ascii_lowercase().contains("rowid"), "error must mention rowid, got: {msg}");
    }
}

/// Green companion: bare `rowid` is already refused. Pinned here so the
/// two cases are visibly paired.
#[test]
fn bare_rowid_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "SELECT rowid FROM t");
    assert!(
        result.is_err(),
        "bare rowid must be refused in the reverse direction, got: {:?}",
        result
    );
    if let Err(err) = &result {
        let msg = err.to_string();
        assert!(msg.to_ascii_lowercase().contains("rowid"), "error must mention rowid, got: {msg}");
    }
}

// M5: COLLATE BINARY and COLLATE RTRIM are SQLite-only collations that
// PostgreSQL does not recognise. Currently they pass through; the reverse
// direction must refuse them as it already refuses COLLATE NOCASE.

/// `ORDER BY x COLLATE BINARY` passes through today. BINARY is a SQLite
/// collation not available in PostgreSQL. The reverse direction must refuse it.
#[test]
fn collate_binary_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "SELECT val FROM t ORDER BY val COLLATE BINARY");
    assert!(
        result.is_err(),
        "COLLATE BINARY must be refused in the reverse direction, got: {:?}",
        result
    );
}

/// `ORDER BY x COLLATE RTRIM` passes through today. RTRIM is a SQLite
/// collation not available in PostgreSQL. The reverse direction must refuse it.
#[test]
fn collate_rtrim_refused_in_reverse() {
    let result = reverse_result(SCHEMA, "SELECT val FROM t ORDER BY val COLLATE RTRIM");
    assert!(
        result.is_err(),
        "COLLATE RTRIM must be refused in the reverse direction, got: {:?}",
        result
    );
}
