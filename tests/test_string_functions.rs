//! Tests for PostgreSQL string function translations.

mod helpers;

use diesel::prelude::*;
use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Table for testing string functions.
    texts (id) {
        /// Primary key.
        id -> Integer,
        /// Text content.
        content -> Text,
    }
}

diesel::table! {
    /// Table for testing chr function.
    codes (id) {
        /// Primary key.
        id -> Integer,
        /// Character code value.
        code -> Integer,
    }
}

diesel::table! {
    /// Table for testing CONCAT_WS function.
    names (id) {
        /// Primary key.
        id -> Integer,
        /// First name.
        first -> Nullable<Text>,
        /// Middle name.
        middle -> Nullable<Text>,
        /// Last name.
        last -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = texts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Text {
    /// Primary key.
    id: i32,
    /// Text content.
    content: String,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = codes)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Code {
    /// Primary key.
    id: i32,
    /// Character code value.
    code: i32,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = names)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Name {
    /// Primary key.
    id: i32,
    /// First name.
    first: Option<String>,
    /// Middle name.
    middle: Option<String>,
    /// Last name.
    last: Option<String>,
}

/// Test that strpos(string, substring) is translated to INSTR(string,
/// substring).
#[test]
fn test_strpos_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE texts (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL
        );
        SELECT strpos(content, 'hello') as pos FROM texts;
    ";

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
    // Translated SQL is dynamically generated; rusqlite execute_batch proves SQLite
    // accepts it.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{stmt}"));
    }

    Ok(())
}

/// Semantic test: strpos translation works correctly in SQLite.
#[test]
fn test_strpos_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE texts (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL
        );
        SELECT content, strpos(content, 'world') as pos FROM texts;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(texts::table)
        .values(&Text { id: 1, content: "hello world".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(texts::table)
        .values(&Text { id: 2, content: "no match here".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(texts::table)
        .values(&Text { id: 3, content: "world at start".to_string() })
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

#[test]
fn test_chr_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE codes (id SERIAL PRIMARY KEY, code INTEGER NOT NULL);
        SELECT chr(code) as character FROM codes;
    ";

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
    // Translated SQL is dynamically generated; rusqlite execute_batch proves SQLite
    // accepts it.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{stmt}"));
    }

    Ok(())
}

/// Semantic test: chr translation works correctly in SQLite.
#[test]
fn test_chr_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE codes (id SERIAL PRIMARY KEY, code INTEGER NOT NULL);
        SELECT chr(code) as character FROM codes;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(codes::table)
        .values(&Code { id: 1, code: 65 }) // 'A'
        .execute(&mut connection)?;
    diesel::insert_into(codes::table)
        .values(&Code { id: 2, code: 66 }) // 'B'
        .execute(&mut connection)?;
    diesel::insert_into(codes::table)
        .values(&Code { id: 3, code: 97 }) // 'a'
        .execute(&mut connection)?;

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
    let sql = "
        CREATE TABLE names (id SERIAL PRIMARY KEY, first TEXT, middle TEXT, last TEXT);
        SELECT CONCAT_WS(' ', first, middle, last) as full_name FROM names;
    ";

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
    // Translated SQL is dynamically generated; rusqlite execute_batch proves SQLite
    // accepts it.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{stmt}"));
    }

    Ok(())
}

/// Semantic test: CONCAT_WS translation works correctly in SQLite.
#[test]
fn test_concat_ws_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE names (id SERIAL PRIMARY KEY, first TEXT, middle TEXT, last TEXT);
        SELECT CONCAT_WS('-', first, middle, last) as full_name FROM names;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data using Diesel ORM
    diesel::insert_into(names::table)
        .values(&Name {
            id: 1,
            first: Some("John".to_string()),
            middle: Some("Q".to_string()),
            last: Some("Public".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(names::table)
        .values(&Name {
            id: 2,
            first: Some("Jane".to_string()),
            middle: Some("A".to_string()),
            last: Some("Doe".to_string()),
        })
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

/// Semantic test: CONCAT_WS skips NULL arguments without adding extra
/// separators.
#[test]
fn test_concat_ws_semantic_skips_nulls() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE names (id SERIAL PRIMARY KEY, first TEXT, middle TEXT, last TEXT);
        SELECT CONCAT_WS('-', first, middle, last) as full_name FROM names ORDER BY id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(names::table)
        .values(&Name {
            id: 1,
            first: Some("John".to_string()),
            middle: None,
            last: Some("Doe".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(names::table)
        .values(&Name {
            id: 2,
            first: None,
            middle: Some("Q".to_string()),
            last: Some("Public".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(names::table)
        .values(&Name { id: 3, first: Some("Jane".to_string()), middle: None, last: None })
        .execute(&mut connection)?;
    diesel::insert_into(names::table)
        .values(&Name { id: 4, first: None, middle: None, last: None })
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

    assert_eq!(results.len(), 4);
    assert_eq!(results[0].full_name, "John-Doe");
    assert_eq!(results[1].full_name, "Q-Public");
    assert_eq!(results[2].full_name, "Jane");
    assert_eq!(results[3].full_name, "");

    Ok(())
}

#[test]
fn left_function_to_substr() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT left('hello', 3)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("substr("), "expected substr: {sql}");
    // Translated SQL is dynamically generated; rusqlite execute_batch proves SQLite
    // accepts it.
    rusqlite::Connection::open_in_memory()
        .expect("in-memory SQLite")
        .execute_batch(&sql)
        .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{sql}"));
}

#[test]
fn left_function_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT left('hello world', 5) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].result, "hello", "left('hello world', 5) should be 'hello'");

    Ok(())
}

#[test]
fn right_function_to_substr() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT right('hello', 3)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("substr("), "expected substr: {sql}");
    // Translated SQL is dynamically generated; rusqlite execute_batch proves SQLite
    // accepts it.
    rusqlite::Connection::open_in_memory()
        .expect("in-memory SQLite")
        .execute_batch(&sql)
        .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{sql}"));
}

#[test]
fn right_function_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT right('hello world', 5) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = diesel::SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].result, "world", "right('hello world', 5) should be 'world'");

    Ok(())
}
