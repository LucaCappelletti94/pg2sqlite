//! Tests for GROUP 4: Reverse translation gaps.
//!
//! Covers:
//! - 4a: Missing reverse function mappings (unicode, json_object, json_array)
//! - 4b: Rename within_group translation (OrderByExpr expressions translated)
//! - 4c: Transaction statement passthrough (COMMIT, ROLLBACK, BEGIN, SAVEPOINT,
//!   RELEASE SAVEPOINT)

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val TEXT, num INT);";

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty());
    stmts[0].to_string()
}

fn reverse_ok(pg_ddl: &str, sqlite_sql: &str) -> Vec<String> {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    stmts.iter().map(ToString::to_string).collect()
}

// ==================== 4a: Missing reverse function mappings
// ====================

#[test]
fn reverse_unicode_to_ascii() {
    let pg = reverse(SCHEMA, "SELECT unicode('A') FROM t;");
    assert!(pg.contains("ascii"), "Expected ascii in output: {pg}");
    assert!(!pg.contains("unicode"), "Should not contain unicode: {pg}");
}

#[test]
fn reverse_json_object_to_json_build_object() {
    let pg = reverse(SCHEMA, "SELECT json_object('key', 'value') FROM t;");
    assert!(pg.contains("json_build_object"), "Expected json_build_object in output: {pg}");
    assert!(!pg.contains("json_object("), "Should not contain json_object(: {pg}");
}

#[test]
fn reverse_json_array_to_json_build_array() {
    let pg = reverse(SCHEMA, "SELECT json_array(1, 2, 3) FROM t;");
    assert!(pg.contains("json_build_array"), "Expected json_build_array in output: {pg}");
    assert!(!pg.contains("json_array("), "Should not contain json_array(: {pg}");
}

// ==================== 4b: Rename within_group translation ====================

#[test]
fn reverse_rename_within_group_translates_exprs() {
    // json_group_array reverses to json_agg (Rename path).
    // The ORDER BY clause inside the function should also be reverse-translated.
    let pg = reverse(SCHEMA, "SELECT json_group_array(val ORDER BY datetime('now')) FROM t;");
    assert!(pg.contains("json_agg"), "Expected json_agg: {pg}");
    // The datetime('now') should be reverse-translated to NOW()
    assert!(pg.contains("NOW()"), "Expected ORDER BY expr to be reverse-translated to NOW(): {pg}");
}

// ==================== 4c: Transaction statement passthrough
// ====================

#[test]
fn reverse_commit() {
    let stmts = reverse_ok(SCHEMA, "COMMIT;");
    assert!(!stmts.is_empty(), "COMMIT should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("COMMIT"), "Expected COMMIT in output: {out}");
}

#[test]
fn reverse_rollback() {
    let stmts = reverse_ok(SCHEMA, "ROLLBACK;");
    assert!(!stmts.is_empty(), "ROLLBACK should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("ROLLBACK"), "Expected ROLLBACK in output: {out}");
}

#[test]
fn reverse_begin() {
    let stmts = reverse_ok(SCHEMA, "BEGIN;");
    assert!(!stmts.is_empty(), "BEGIN should reverse OK");
    let out = &stmts[0];
    assert!(out.contains("BEGIN"), "Expected BEGIN in output: {out}");
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
}
