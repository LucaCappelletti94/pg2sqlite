//! Tests for UNION / UNION ALL / INTERSECT / EXCEPT query translation.
//!
//! Set-operation queries are standard SQL and should pass through the
//! translator unchanged.  These tests verify that:
//! - Each set-operation keyword survives translation
//! - The translated SQL actually executes in SQLite (integration)

use diesel::{RunQueryDsl, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// UNION (with implicit deduplication) must survive translation.
#[test]
fn test_union_query_translation() {
    let sql = "
        CREATE TABLE t1 (a INTEGER, b TEXT);
        CREATE TABLE t2 (a INTEGER, b TEXT);
        SELECT a, b FROM t1 UNION SELECT a, b FROM t2;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(output.contains("UNION"), "Output should contain UNION keyword, got:\n{output}");
}

/// UNION ALL (no deduplication) must survive translation.
#[test]
fn test_union_all_translation() {
    let sql = "
        CREATE TABLE t1 (a INTEGER);
        CREATE TABLE t2 (a INTEGER);
        SELECT a FROM t1 UNION ALL SELECT a FROM t2;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        output.contains("UNION ALL"),
        "Output should contain UNION ALL keyword, got:\n{output}"
    );
}

/// INTERSECT must survive translation.
#[test]
fn test_intersect_translation() {
    let sql = "
        CREATE TABLE t1 (a INTEGER);
        CREATE TABLE t2 (a INTEGER);
        SELECT a FROM t1 INTERSECT SELECT a FROM t2;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        output.contains("INTERSECT"),
        "Output should contain INTERSECT keyword, got:\n{output}"
    );
}

/// EXCEPT must survive translation.
#[test]
fn test_except_translation() {
    let sql = "
        CREATE TABLE t1 (a INTEGER);
        CREATE TABLE t2 (a INTEGER);
        SELECT a FROM t1 EXCEPT SELECT a FROM t2;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(output.contains("EXCEPT"), "Output should contain EXCEPT keyword, got:\n{output}");
}

/// UNION ALL translated SQL executes correctly in an in-memory SQLite DB
/// and preserves duplicate rows.
#[test]
fn test_union_all_executes_in_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE src1 (val INTEGER);
        CREATE TABLE src2 (val INTEGER);
        SELECT val FROM src1 UNION ALL SELECT val FROM src2;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::sqlite::SqliteConnection::establish(":memory:")?;

    // Execute DDL statements
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    // Populate the tables
    diesel::sql_query("INSERT INTO src1 VALUES (1), (2), (3)").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO src2 VALUES (2), (3), (4)").execute(&mut conn)?;

    // Find the translated SELECT statement
    let union_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        #[allow(dead_code)]
        val: i32,
    }

    let results = diesel::sql_query(&union_stmt).load::<Row>(&mut conn)?;
    // UNION ALL keeps all rows including duplicates: 1, 2, 3, 2, 3, 4 → 6 rows
    assert_eq!(
        results.len(),
        6,
        "UNION ALL should keep duplicate rows (expected 6), got {}",
        results.len()
    );
    Ok(())
}

/// UNION (deduplicating) translated SQL returns the correct distinct rows.
#[test]
fn test_union_deduplicates_in_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE left_t (a INTEGER);
        CREATE TABLE right_t (a INTEGER);
        SELECT a FROM left_t UNION SELECT a FROM right_t;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::sqlite::SqliteConnection::establish(":memory:")?;

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::sql_query("INSERT INTO left_t VALUES (1), (2), (3)").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO right_t VALUES (2), (3), (4)").execute(&mut conn)?;

    let union_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        a: i32,
    }

    let mut results = diesel::sql_query(&union_stmt).load::<Row>(&mut conn)?;
    results.sort_by_key(|r| r.a);
    // UNION deduplicates: {1, 2, 3, 4} → 4 distinct rows
    assert_eq!(
        results.len(),
        4,
        "UNION should deduplicate (expected 4 rows), got {}",
        results.len()
    );
    assert_eq!(results.iter().map(|r| r.a).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    Ok(())
}
