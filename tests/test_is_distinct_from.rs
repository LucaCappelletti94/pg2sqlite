//! TDD tests for IS DISTINCT FROM / IS NOT DISTINCT FROM translation (Section
//! 2).

#![allow(missing_docs)]

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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
