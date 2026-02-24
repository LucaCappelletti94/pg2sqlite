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

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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

// ==================== ANY/SOME ====================

#[test]
fn any_eq_subquery_translates_to_in() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val = ANY(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(output.contains("IN"), "= ANY(subquery) should translate to IN, got: {output}");
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
}

#[test]
fn any_gt_subquery_translates_to_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ANY(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("EXISTS"),
        "> ANY(subquery) should translate via EXISTS, got: {output}"
    );
}

#[test]
fn any_gt_array_literal_translates_to_or_chain() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ANY(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(output.contains(" OR "), "> ANY(array) should translate to OR chain, got: {output}");
    assert!(!output.contains(" ANY"), "ANY keyword should be removed, got: {output}");
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
}

#[test]
fn all_gt_subquery_translates_to_not_exists() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ALL(SELECT id FROM t);";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("NOT EXISTS"),
        "> ALL(subquery) should translate via NOT EXISTS, got: {output}"
    );
}

#[test]
fn all_gt_array_literal_translates_to_and_chain() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
               SELECT * FROM t WHERE val > ALL(ARRAY[1, 2, 3]);";
    let output = translate(sql).unwrap();
    assert!(output.contains(" AND "), "> ALL(array) should translate to AND chain, got: {output}");
    assert!(!output.contains(" ALL"), "ALL keyword should be removed, got: {output}");
}

// ==================== SIMILAR TO ====================

#[test]
fn similar_to_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name SIMILAR TO '%test%';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("SIMILAR TO"), "Expected SIMILAR TO error, got: {err}");
}

// ==================== IS NORMALIZED ====================

#[test]
fn is_normalized_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name IS NORMALIZED;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("IS NORMALIZED"), "Expected IS NORMALIZED error, got: {err}");
}

// ==================== @@ operator (non-FTS) ====================

#[test]
fn at_at_without_tsvector_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT * FROM t WHERE name @@ 'test';";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("@@"), "Expected @@ error, got: {err}");
}

// ==================== Vector distance operators ====================

#[test]
fn vector_l2_distance_translates() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT * FROM t ORDER BY embedding <-> '[1,2,3]';";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("vec_distance_L2"),
        "<-> should translate to vec_distance_L2, got: {output}"
    );
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
