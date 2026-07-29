//! Tests for PostgreSQL to SQLite function translations.

#![allow(dead_code)]

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// Schema definitions for test tables
diesel::table! {
    /// Events table for testing NOW() function translation.
    events (id) {
        /// Event ID.
        id -> Integer,
        /// Event name.
        name -> Text,
        /// Timestamp when event was created.
        created_at -> Text,
    }
}

diesel::table! {
    /// Tags table for testing string_agg function translation.
    tags (id) {
        /// Tag ID.
        id -> Integer,
        /// Associated item ID.
        item_id -> Integer,
        /// Tag text.
        tag -> Text,
    }
}

diesel::table! {
    /// Items table for testing COALESCE function.
    items (id) {
        /// Item ID.
        id -> Integer,
        /// Item name (nullable).
        name -> Nullable<Text>,
        /// Item description (nullable).
        description -> Nullable<Text>,
    }
}

diesel::table! {
    /// Users table for testing ILIKE translation.
    users (id) {
        /// User ID.
        id -> Integer,
        /// User name.
        name -> Text,
    }
}

/// An event record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Event {
    /// Event ID.
    id: i32,
    /// Event name.
    name: String,
    /// Timestamp when event was created.
    created_at: String,
}

/// A tag record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = tags)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Tag {
    /// Tag ID.
    id: i32,
    /// Associated item ID.
    item_id: i32,
    /// Tag text.
    #[diesel(column_name = "tag")]
    text: String,
}

/// An item record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = items)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Item {
    /// Item ID.
    id: i32,
    /// Item name (nullable).
    name: Option<String>,
    /// Item description (nullable).
    description: Option<String>,
}

/// A user record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// User ID.
    id: i32,
    /// User name.
    name: String,
}

#[test]
fn test_now_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TIMESTAMP NOT NULL
        );
        SELECT * FROM events WHERE created_at > NOW();
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain datetime('now'), not NOW()
    assert!(
        select_stmt.contains("datetime('now')"),
        "NOW() should translate to datetime('now'), got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("NOW()"),
        "Should not contain NOW(), got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_now_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        INSERT INTO events (name, created_at) VALUES ('test', NOW());
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute all statements
    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Verify the row was inserted with a timestamp
    #[derive(QueryableByName, Debug)]
    struct EventResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        created_at: String,
    }

    let results = diesel::sql_query("SELECT id, created_at FROM events")
        .load::<EventResult>(&mut connection)?;

    assert_eq!(results.len(), 1);
    // created_at should be a valid timestamp (not empty, not "NOW()")
    assert!(!results[0].created_at.is_empty());
    assert!(!results[0].created_at.contains("NOW"));
    // Should look like a datetime (contains digits and dashes/colons)
    assert!(
        results[0].created_at.contains('-') || results[0].created_at.contains(':'),
        "created_at should be a datetime, got: {}",
        results[0].created_at
    );

    Ok(())
}

#[test]
fn test_string_agg_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE tags (
            id SERIAL PRIMARY KEY,
            item_id INTEGER NOT NULL,
            tag TEXT NOT NULL
        );
        SELECT item_id, string_agg(tag, ', ') FROM tags GROUP BY item_id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain group_concat, not string_agg
    assert!(
        select_stmt.contains("group_concat"),
        "string_agg should translate to group_concat, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_lowercase().contains("string_agg"),
        "Should not contain string_agg, got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_coalesce_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id SERIAL PRIMARY KEY,
            name TEXT,
            description TEXT
        );
        SELECT COALESCE(name, description, 'default') as result FROM items;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::insert_into(items::table)
        .values(&Item {
            id: 1,
            name: Some("Item1".to_string()),
            description: Some("Desc1".to_string()),
        })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 2, name: None, description: Some("Desc2".to_string()) })
        .execute(&mut connection)?;
    diesel::insert_into(items::table)
        .values(&Item { id: 3, name: None, description: None })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct Result {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<Result>(&mut connection)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].result, "Item1"); // name is not null
    assert_eq!(results[1].result, "Desc2"); // name is null, description is not
    assert_eq!(results[2].result, "default"); // both null, use default

    Ok(())
}

/// Test that ILIKE is translated to LIKE (SQLite LIKE is case-insensitive for
/// ASCII).
#[test]
fn test_ilike_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT * FROM users WHERE name ILIKE '%test%';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should contain LIKE, not ILIKE
    assert!(
        select_stmt.to_uppercase().contains("LIKE"),
        "ILIKE should translate to LIKE, got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("ILIKE"),
        "Should not contain ILIKE, got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_ilike_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        );
        SELECT * FROM users WHERE name ILIKE '%test%';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data with various cases
    diesel::insert_into(users::table)
        .values(&User { id: 1, name: "TEST".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 2, name: "test".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 3, name: "TeSt".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(users::table)
        .values(&User { id: 4, name: "other".to_string() })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct UserResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<UserResult>(&mut connection)?;

    // Should match all three "test" variants (case-insensitive), not "other"
    assert_eq!(results.len(), 3, "Should match 3 users with 'test' in name, got: {results:?}");

    Ok(())
}

#[test]
fn test_string_agg_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE tags (
            id SERIAL PRIMARY KEY,
            item_id INTEGER NOT NULL,
            tag TEXT NOT NULL
        );
        SELECT item_id, string_agg(tag, ', ') as tags FROM tags GROUP BY item_id;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::insert_into(tags::table)
        .values(&Tag { id: 1, item_id: 1, text: "rust".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(tags::table)
        .values(&Tag { id: 2, item_id: 1, text: "programming".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(tags::table)
        .values(&Tag { id: 3, item_id: 2, text: "python".to_string() })
        .execute(&mut connection)?;

    // Get and execute the SELECT statement
    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct TagResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        item_id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        tags: String,
    }

    let results = diesel::sql_query(&select_stmt).load::<TagResult>(&mut connection)?;

    assert_eq!(results.len(), 2);

    // Find item_id=1 result
    let item1 = results.iter().find(|r| r.item_id == 1).expect("Should have item 1");
    assert!(
        item1.tags.contains("rust") && item1.tags.contains("programming"),
        "Item 1 should have both tags, got: {}",
        item1.tags
    );

    Ok(())
}
