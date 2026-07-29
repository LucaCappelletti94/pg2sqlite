//! TDD tests for IS DISTINCT FROM / IS NOT DISTINCT FROM translation (Section
//! 2), plus IS UNKNOWN / IS NOT UNKNOWN semantic tests.

#![allow(missing_docs)]

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

diesel::table! {
    pairs (id) {
        id -> Integer,
        a -> Nullable<Text>,
        b -> Nullable<Text>,
    }
}

/// IS DISTINCT FROM is translated to NOT (x IS y) — compact and correct.
#[test]
fn test_is_distinct_from_translation_form() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT a IS DISTINCT FROM b FROM pairs;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let out = translated[0].to_string();

    // Must use NOT (x IS y), not CASE
    assert!(out.contains("NOT"), "Should contain NOT, got: {out}");
    assert!(out.contains(" IS "), "Should contain IS, got: {out}");
    assert!(!out.contains("CASE"), "Should NOT use CASE expression, got: {out}");

    Ok(())
}

/// IS DISTINCT FROM semantics: NULLs compare as distinct from any non-NULL.
#[test]
fn test_is_distinct_from_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE pairs (id SERIAL PRIMARY KEY, a TEXT, b TEXT);
        SELECT id FROM pairs WHERE a IS DISTINCT FROM b ORDER BY id;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;

    // Execute DDL
    diesel::sql_query(translated[0].to_string()).execute(&mut conn)?;

    diesel::sql_query(
        "INSERT INTO pairs VALUES (1,'x','x'),(2,'x','y'),(3,NULL,NULL),(4,NULL,'x'),(5,'x',NULL)",
    )
    .execute(&mut conn)?;

    // Execute the SELECT
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let rows = diesel::sql_query(translated[1].to_string()).load::<Row>(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|r| r.id).collect();

    // Row 1 (x=x): NOT distinct → excluded
    // Row 2 (x≠y): distinct → included
    // Row 3 (NULL, NULL): NOT distinct → excluded
    // Row 4 (NULL, 'x'): distinct → included
    // Row 5 ('x', NULL): distinct → included
    assert_eq!(ids, vec![2, 4, 5]);
    Ok(())
}

/// IS NOT DISTINCT FROM semantics: NULLs compare as equal.
#[test]
fn test_is_not_distinct_from_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE pairs (id SERIAL PRIMARY KEY, a TEXT, b TEXT);
        SELECT id FROM pairs WHERE a IS NOT DISTINCT FROM b ORDER BY id;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    diesel::sql_query(translated[0].to_string()).execute(&mut conn)?;
    diesel::sql_query(
        "INSERT INTO pairs VALUES (1,'x','x'),(2,'x','y'),(3,NULL,NULL),(4,NULL,'x')",
    )
    .execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let rows = diesel::sql_query(translated[1].to_string()).load::<Row>(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|r| r.id).collect();

    // Row 1 (x=x): not distinct → included
    // Row 2 (x≠y): distinct → excluded
    // Row 3 (NULL=NULL): not distinct → included
    // Row 4 (NULL≠'x'): distinct → excluded
    assert_eq!(ids, vec![1, 3]);
    Ok(())
}

mod expr_overlay_schema {
    diesel::table! {
        pair_values (id) {
            id -> Integer,
            a -> Nullable<Integer>,
            b -> Nullable<Integer>,
        }
    }

    diesel::table! {
        numeric_values (id) {
            id -> Integer,
            value -> Nullable<Integer>,
        }
    }
}

use expr_overlay_schema::{numeric_values, pair_values};

#[derive(Debug, Clone, diesel::Insertable)]
#[diesel(table_name = pair_values)]
struct PairValue {
    id: i32,
    a: Option<i32>,
    b: Option<i32>,
}

#[derive(Debug, Clone, diesel::Insertable)]
#[diesel(table_name = numeric_values)]
struct NumericValue {
    id: i32,
    value: Option<i32>,
}

fn translate(sql: &str) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    Ok(translated)
}

fn select_sql(translated: &[Statement]) -> String {
    translated
        .iter()
        .find(|stmt| matches!(stmt, Statement::Query(_)))
        .expect("should contain a SELECT query")
        .to_string()
}

fn execute_non_query_statements(
    translated: &[Statement],
    conn: &mut SqliteConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    for stmt in translated.iter().filter(|stmt| !matches!(stmt, Statement::Query(_))) {
        diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(())
}

#[derive(Debug, diesel::QueryableByName)]
struct IdRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

#[test]
fn test_is_distinct_from_semantic_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE pair_values (
            id INTEGER PRIMARY KEY,
            a INTEGER,
            b INTEGER
        );
        SELECT id FROM pair_values WHERE a IS DISTINCT FROM b ORDER BY id;
    ";

    let translated = translate(sql)?;
    let query = select_sql(&translated);
    assert!(
        !query.to_uppercase().contains("IS DISTINCT FROM"),
        "IS DISTINCT FROM should be rewritten, got: {query}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_non_query_statements(&translated, &mut conn)?;

    diesel::insert_into(pair_values::table)
        .values(&[
            PairValue { id: 1, a: Some(1), b: Some(1) },
            PairValue { id: 2, a: Some(1), b: Some(2) },
            PairValue { id: 3, a: None, b: None },
            PairValue { id: 4, a: None, b: Some(1) },
            PairValue { id: 5, a: Some(1), b: None },
        ])
        .execute(&mut conn)?;

    let rows: Vec<IdRow> = diesel::sql_query(query).load(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![2, 4, 5]);

    Ok(())
}

#[test]
fn test_is_not_distinct_from_semantic_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE pair_values (
            id INTEGER PRIMARY KEY,
            a INTEGER,
            b INTEGER
        );
        SELECT id FROM pair_values WHERE a IS NOT DISTINCT FROM b ORDER BY id;
    ";

    let translated = translate(sql)?;
    let query = select_sql(&translated);
    assert!(
        !query.to_uppercase().contains("IS NOT DISTINCT FROM"),
        "IS NOT DISTINCT FROM should be rewritten, got: {query}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_non_query_statements(&translated, &mut conn)?;

    diesel::insert_into(pair_values::table)
        .values(&[
            PairValue { id: 1, a: Some(1), b: Some(1) },
            PairValue { id: 2, a: Some(1), b: Some(2) },
            PairValue { id: 3, a: None, b: None },
            PairValue { id: 4, a: None, b: Some(1) },
            PairValue { id: 5, a: Some(1), b: None },
        ])
        .execute(&mut conn)?;

    let rows: Vec<IdRow> = diesel::sql_query(query).load(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![1, 3]);

    Ok(())
}

#[test]
fn test_is_unknown_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE numeric_values (
            id INTEGER PRIMARY KEY,
            value INTEGER
        );
        SELECT id FROM numeric_values WHERE (value > 0) IS UNKNOWN ORDER BY id;
    ";

    let translated = translate(sql)?;
    let query = select_sql(&translated);
    assert!(!query.to_uppercase().contains("UNKNOWN"), "UNKNOWN should be rewritten, got: {query}");
    assert!(query.to_uppercase().contains("IS NULL"), "Expected IS NULL rewrite, got: {query}");

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_non_query_statements(&translated, &mut conn)?;

    diesel::insert_into(numeric_values::table)
        .values(&[
            NumericValue { id: 1, value: Some(10) },
            NumericValue { id: 2, value: Some(-1) },
            NumericValue { id: 3, value: None },
        ])
        .execute(&mut conn)?;

    let rows: Vec<IdRow> = diesel::sql_query(query).load(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![3]);

    Ok(())
}

#[test]
fn test_is_not_unknown_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE numeric_values (
            id INTEGER PRIMARY KEY,
            value INTEGER
        );
        SELECT id FROM numeric_values WHERE (value > 0) IS NOT UNKNOWN ORDER BY id;
    ";

    let translated = translate(sql)?;
    let query = select_sql(&translated);
    assert!(!query.to_uppercase().contains("UNKNOWN"), "UNKNOWN should be rewritten, got: {query}");
    assert!(
        query.to_uppercase().contains("IS NOT NULL"),
        "Expected IS NOT NULL rewrite, got: {query}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_non_query_statements(&translated, &mut conn)?;

    diesel::insert_into(numeric_values::table)
        .values(&[
            NumericValue { id: 1, value: Some(10) },
            NumericValue { id: 2, value: Some(-1) },
            NumericValue { id: 3, value: None },
        ])
        .execute(&mut conn)?;

    let rows: Vec<IdRow> = diesel::sql_query(query).load(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![1, 2]);

    Ok(())
}
