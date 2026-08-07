//! Tests for expression translation error branches in
//! `src/impls/translator_impls/expr.rs`.
//!
//! Covers:
//! - ANY/SOME: = ANY(subquery/array literal) -> IN, other ops -> error
//! - ALL: <> ALL(subquery/array literal) -> NOT IN, other ops -> error
//! - SIMILAR TO -> error
//! - IS NORMALIZED -> error
//! - @@ (non-FTS) -> error
//! - Vector operators: <#>, <+>, <%> -> errors; <->, <=> -> vec_distance
//!   functions

use std::sync::Once;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlite_vec::sqlite3_vec_init;

/// Register sqlite-vec once per process so connections opened by
/// `sqlite_accepts` have vec0 and vec_distance_* available.
///
/// SAFETY: `sqlite3_vec_init` has the C signature `(db, pzErrMsg, pApi) ->
/// int`; the transmute restores it for `sqlite3_auto_extension`. rusqlite FFI
/// is the only path to this API.
fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    });
}
/// Helper: translate a full SQL statement and return the output or error
/// string.
fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn any_eq_subquery_translates_to_in() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val = ANY(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(output.contains("IN"), "= ANY(subquery) should translate to IN, got: {output}");
    sqlite_accepts(sql);
}

#[test]
fn any_eq_array_literal_translates_to_in_list() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val = ANY(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("IN (1, 2, 3)"),
        "= ANY(ARRAY[..]) should translate to IN list, got: {output}"
    );
    sqlite_accepts(sql);
}

/// Flipped R124 pin. The lowering emitted `(SELECT id FROM t)
/// __pg2sqlite_quantifier (__pg2sqlite_item)`, the derived-table column
/// alias list R105 established SQLite has no grammar for. The item column is
/// now aliased inside the projection, so the emitted query prepares and
/// answers PostgreSQL's `> ANY` semantics.
#[test]
fn any_gt_subquery_translates_to_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ANY(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("EXISTS"),
        "> ANY(subquery) should translate via EXISTS, got: {output}"
    );
    assert_eq!(
        quantifier_survivors(sql),
        vec![1, 10],
        "> ANY keeps rows whose val beats the smallest id"
    );
}

#[test]
fn any_gt_array_literal_translates_to_or_chain() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ANY(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(output.contains(" OR "), "> ANY(array) should translate to OR chain, got: {output}");
    assert!(!output.contains(" ANY"), "ANY keyword should be removed, got: {output}");
    sqlite_accepts(sql);
}

#[test]
fn all_neq_subquery_translates_to_not_in() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val <> ALL(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("NOT IN"),
        "<> ALL(subquery) should translate to NOT IN, got: {output}"
    );
    sqlite_accepts(sql);
}

#[test]
fn all_neq_array_literal_translates_to_not_in_list() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val <> ALL(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("NOT IN (1, 2, 3)"),
        "<> ALL(ARRAY[..]) should translate to NOT IN list, got: {output}"
    );
    sqlite_accepts(sql);
}

/// Flipped R124 pin, the `ALL` half of the shape above.
#[test]
fn all_gt_subquery_translates_to_not_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ALL(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("NOT EXISTS"),
        "> ALL(subquery) should translate via NOT EXISTS, got: {output}"
    );
    assert_eq!(
        quantifier_survivors(sql),
        vec![1],
        "> ALL keeps only rows whose val beats the largest id"
    );
}

/// Seeds `t` with `(1, 20), (7, 0), (10, 5)`, runs the translated statements,
/// and returns the ids the translated SELECT keeps.
fn quantifier_survivors(sql: &str) -> Vec<i64> {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut survivors = Vec::new();
    for stmt in &stmts {
        let text = stmt.to_string();
        if text.starts_with("SELECT") {
            let mut prepared = conn
                .prepare(&text)
                .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{text}"));
            survivors = prepared
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        } else {
            conn.execute_batch(&format!("{text};"))
                .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{text}"));
            if text.starts_with("CREATE TABLE") {
                conn.execute_batch("INSERT INTO t (id, val) VALUES (1, 20), (7, 0), (10, 5);")
                    .expect("seed rows");
            }
        }
    }
    survivors.sort_unstable();
    survivors
}

#[test]
fn all_gt_array_literal_translates_to_and_chain() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ALL(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(output.contains(" AND "), "> ALL(array) should translate to AND chain, got: {output}");
    assert!(!output.contains(" ALL"), "ALL keyword should be removed, got: {output}");
    sqlite_accepts(sql);
}

#[test]
fn similar_to_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name SIMILAR TO '%test%';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("SIMILAR TO"), "Expected SIMILAR TO error, got: {err}");
}

#[test]
fn is_normalized_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name IS NORMALIZED;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("IS NORMALIZED"), "Expected IS NORMALIZED error, got: {err}");
}

#[test]
fn at_at_without_tsvector_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name @@ 'test';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("@@"), "Expected @@ error, got: {err}");
}

#[test]
fn vector_l2_distance_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <-> '[1,2,3]';";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("vec_distance_L2"),
        "<-> should translate to vec_distance_L2, got: {output}"
    );
    sqlite_accepts(sql);
}

#[test]
fn vector_cosine_distance_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <=> '[1,2,3]';";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("vec_distance_cosine"),
        "<=> should translate to vec_distance_cosine, got: {output}"
    );
    sqlite_accepts(sql);
}

#[test]
fn vector_negative_inner_product_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <#> '[1,2,3]';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("<#>"), "Expected <#> error, got: {err}");
}

#[test]
fn vector_manhattan_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <+> '[1,2,3]';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("<+>"), "Expected <+> error, got: {err}");
}

#[test]
fn vector_jaccard_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <%> '[1,2,3]';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("<%>"), "Expected <%> error, got: {err}");
}

#[test]
fn at_time_zone_named_zone_still_errors() {
    // Named timezones like 'Europe/Brussels' are not supported; only
    // UTC/local/fixed offsets are.
    let result = translate("SELECT col AT TIME ZONE 'Europe/Brussels' FROM t;");
    assert!(result.is_err(), "Expected error for named AT TIME ZONE");
    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Expected error for named AT TIME ZONE, got empty");
}

/// Translate the original PG SQL and execute all emitted statements against an
/// in-memory SQLite connection that has sqlite-vec loaded.
///
/// rusqlite is used directly because diesel does not expose
/// `sqlite3_auto_extension`.
fn sqlite_accepts(pg: &str) {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted statement failed: {e}\n{stmt}"));
    }
}
