//! Tests for POSITION and SUBSTRING expression translation.
//!
//! - POSITION(substr IN str) translates to INSTR(str, substr)
//! - SUBSTRING(str FROM pos FOR len) translates to SUBSTR(str, pos, len)

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
mod helpers;

mod schema {
    diesel::table! {
        /// Strings table for position/substring tests.
        strings (id) {
            /// Primary key.
            id -> Integer,
            /// Text value.
            text_val -> Text,
        }
    }
}

use schema::strings;

/// A string record for testing.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, QueryableByName)]
#[diesel(table_name = strings)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StringData {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    text_val: String,
}

#[test]
fn test_position_translation() {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT POSITION('world' IN text_val) as pos FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let query = helpers::prepared_user_select(sql, &options);

    // Should contain INSTR, not POSITION
    assert!(
        query.to_uppercase().contains("INSTR"),
        "POSITION should translate to INSTR, got: {query}"
    );
    assert!(
        !query.to_uppercase().contains("POSITION"),
        "Should not contain POSITION, got: {query}"
    );
}

#[test]
fn test_position_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;
    diesel::insert_into(strings::table)
        .values(&StringData { id: 2, text_val: "goodbye".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    #[allow(dead_code)]
    struct PosResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        pos: i32,
    }

    // INSTR returns 1-based position, 0 if not found
    // Raw SQL needed to test INSTR function translation
    let results: Vec<PosResult> =
        diesel::sql_query("SELECT id, INSTR(text_val, 'world') as pos FROM strings ORDER BY id")
            .load(&mut conn)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].pos, 7); // "world" starts at position 7 in "hello world"
    assert_eq!(results[1].pos, 0); // "world" not found in "goodbye"

    Ok(())
}

#[test]
fn test_substring_translation() {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT SUBSTRING(text_val FROM 1 FOR 5) as sub FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let query = helpers::prepared_user_select(sql, &options);

    // Should contain SUBSTR, not SUBSTRING
    assert!(
        query.to_uppercase().contains("SUBSTR"),
        "SUBSTRING should translate to SUBSTR, got: {query}"
    );
}

#[test]
fn test_substring_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct SubResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        sub: String,
    }

    // SUBSTR(str, start, length) - 1-based indexing
    // Raw SQL needed to test SUBSTR function translation
    let results: Vec<SubResult> =
        diesel::sql_query("SELECT SUBSTR(text_val, 1, 5) as sub FROM strings").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sub, "hello");

    Ok(())
}

#[test]
fn test_substring_no_length_translation() {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT SUBSTRING(text_val FROM 7) as sub FROM strings;
    ";

    let options = Pg2SqliteOptions::default();
    let query = helpers::prepared_user_select(sql, &options);

    assert!(
        query.to_uppercase().contains("SUBSTR"),
        "SUBSTRING should translate to SUBSTR, got: {query}"
    );
}

#[test]
fn test_substring_no_length_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE strings (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(strings::table)
        .values(&StringData { id: 1, text_val: "hello world".to_string() })
        .execute(&mut conn)?;

    #[derive(QueryableByName, Debug)]
    struct SubResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        sub: String,
    }

    // SUBSTR(str, start) without length returns from start to end
    // Raw SQL needed to test SUBSTR function translation
    let results: Vec<SubResult> =
        diesel::sql_query("SELECT SUBSTR(text_val, 7) as sub FROM strings").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sub, "world");

    Ok(())
}

mod overlay_schema {
    diesel::table! {
        overlay_samples (id) {
            id -> Integer,
            text_val -> Text,
        }
    }
}

use overlay_schema::overlay_samples;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = overlay_samples)]
struct OverlaySample {
    id: i32,
    text_val: String,
}

#[derive(Debug, QueryableByName)]
struct OverlayRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    out_text: String,
}

#[test]
fn test_overlay_with_for_semantic() {
    let sql = "
        CREATE TABLE overlay_samples (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT id, OVERLAY(text_val PLACING 'ZZ' FROM 3 FOR 2) AS out_text
        FROM overlay_samples
        ORDER BY id;
    ";

    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(sql, &options).expect("translation failed");
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    assert!(
        !query.to_uppercase().contains("OVERLAY("),
        "OVERLAY should be rewritten, got: {query}"
    );
    assert!(query.to_uppercase().contains("SUBSTR"), "Expected SUBSTR rewrite, got: {query}");

    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("establish");
    // Translated DDL: runtime string from the translator, cannot use the typed
    // DSL.
    for stmt in &stmts {
        if !helpers::is_user_statement(stmt, "SELECT") {
            diesel::sql_query(stmt.as_str()).execute(&mut conn).expect("DDL failed");
        }
    }

    diesel::insert_into(overlay_samples::table)
        .values(&[
            OverlaySample { id: 1, text_val: "abcdef".to_string() },
            OverlaySample { id: 2, text_val: "123456".to_string() },
        ])
        .execute(&mut conn)
        .expect("insert failed");

    // Translated SELECT: runtime string from the translator, cannot use the
    // typed DSL.
    let rows: Vec<OverlayRow> = diesel::sql_query(&query).load(&mut conn).expect("load failed");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].out_text, "abZZef");
    assert_eq!(rows[1].id, 2);
    assert_eq!(rows[1].out_text, "12ZZ56");
}

/// Test OVERLAY without FOR (replacement length defaults to length of
/// replacement string).
#[test]
fn test_overlay_without_for_semantic() {
    let sql = "
        CREATE TABLE overlay_samples (
            id INTEGER PRIMARY KEY,
            text_val TEXT NOT NULL
        );
        SELECT id, OVERLAY(text_val PLACING 'sqlite' FROM 7) AS out_text
        FROM overlay_samples
        ORDER BY id;
    ";

    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(sql, &options).expect("translation failed");
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    assert!(
        !query.to_uppercase().contains("OVERLAY("),
        "OVERLAY should be rewritten, got: {query}"
    );
    assert!(query.to_uppercase().contains("SUBSTR"), "Expected SUBSTR rewrite, got: {query}");

    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("establish");
    // Translated DDL: runtime string from the translator, cannot use the typed
    // DSL.
    for stmt in &stmts {
        if !helpers::is_user_statement(stmt, "SELECT") {
            diesel::sql_query(stmt.as_str()).execute(&mut conn).expect("DDL failed");
        }
    }

    diesel::insert_into(overlay_samples::table)
        .values(&[OverlaySample { id: 1, text_val: "hello world".to_string() }])
        .execute(&mut conn)
        .expect("insert failed");

    // Translated SELECT: runtime string from the translator, cannot use the
    // typed DSL.
    let rows: Vec<OverlayRow> = diesel::sql_query(&query).load(&mut conn).expect("load failed");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].out_text, "hello sqlite");
}
