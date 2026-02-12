//! Tests for PostgreSQL string function translations.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Test that strpos(string, substring) is translated to INSTR(string,
/// substring).
#[test]
fn test_strpos_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE texts (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL
        );
        SELECT strpos(content, 'hello') as pos FROM texts;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // strpos should be converted to INSTR
    assert!(
        select_stmt.to_uppercase().contains("INSTR"),
        "strpos should be converted to INSTR, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_lowercase().contains("strpos"),
        "strpos should not appear in output, got: {select_stmt}"
    );

    Ok(())
}

/// Semantic test: strpos translation works correctly in SQLite.
#[test]
fn test_strpos_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE texts (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL
        );
        SELECT content, strpos(content, 'world') as pos FROM texts;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::sql_query("INSERT INTO texts (content) VALUES ('hello world')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO texts (content) VALUES ('no match here')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO texts (content) VALUES ('world at start')")
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct TextPos {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        pos: i32,
    }

    let results = diesel::sql_query(&select_stmt).load::<TextPos>(&mut connection)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].pos, 7); // "hello world" - 'world' starts at position 7
    assert_eq!(results[1].pos, 0); // "no match here" - not found, returns 0
    assert_eq!(results[2].pos, 1); // "world at start" - 'world' starts at position 1

    Ok(())
}

/// Test that chr(n) is translated to char(n).
#[test]
fn test_chr_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE codes (id SERIAL PRIMARY KEY, code INTEGER NOT NULL);
        SELECT chr(code) as character FROM codes;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // chr should be converted to char
    assert!(
        select_stmt.to_lowercase().contains("char("),
        "chr should be converted to char, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_lowercase().contains("chr("),
        "chr should not appear in output, got: {select_stmt}"
    );

    Ok(())
}

/// Semantic test: chr translation works correctly in SQLite.
#[test]
fn test_chr_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE codes (id SERIAL PRIMARY KEY, code INTEGER NOT NULL);
        SELECT chr(code) as character FROM codes;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query("INSERT INTO codes (code) VALUES (65)").execute(&mut connection)?; // 'A'
    diesel::sql_query("INSERT INTO codes (code) VALUES (66)").execute(&mut connection)?; // 'B'
    diesel::sql_query("INSERT INTO codes (code) VALUES (97)").execute(&mut connection)?; // 'a'

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct CharResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        character: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<CharResult>(&mut connection)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].character, "A");
    assert_eq!(results[1].character, "B");
    assert_eq!(results[2].character, "a");

    Ok(())
}

/// Test that CONCAT_WS(sep, a, b, c) is translated to a || sep || b || sep ||
/// c.
#[test]
fn test_concat_ws_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE names (id SERIAL PRIMARY KEY, first TEXT, middle TEXT, last TEXT);
        SELECT CONCAT_WS(' ', first, middle, last) as full_name FROM names;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // CONCAT_WS should be converted to || operators
    assert!(
        select_stmt.contains("||"),
        "CONCAT_WS should be converted to || operators, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("CONCAT_WS"),
        "CONCAT_WS should not appear in output, got: {select_stmt}"
    );

    Ok(())
}

/// Semantic test: CONCAT_WS translation works correctly in SQLite.
#[test]
fn test_concat_ws_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE names (id SERIAL PRIMARY KEY, first TEXT, middle TEXT, last TEXT);
        SELECT CONCAT_WS('-', first, middle, last) as full_name FROM names;
    "#;

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query("INSERT INTO names (first, middle, last) VALUES ('John', 'Q', 'Public')")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO names (first, middle, last) VALUES ('Jane', 'A', 'Doe')")
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct FullName {
        #[diesel(sql_type = diesel::sql_types::Text)]
        full_name: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<FullName>(&mut connection)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].full_name, "John-Q-Public");
    assert_eq!(results[1].full_name, "Jane-A-Doe");

    Ok(())
}
