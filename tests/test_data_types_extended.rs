//! Tests for extended data type support: JSON, JSONB, complex defaults, UPSERT.
//!
//! These tests verify that the new type mappings work correctly at runtime.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// Schema definitions for test tables
diesel::table! {
    /// Test table for JSON/JSONB data types.
    json_data (id) {
        /// Primary key.
        id -> Integer,
        /// Optional JSON metadata.
        metadata -> Nullable<Text>,
        /// Required JSON settings (defaults to '{}').
        settings -> Text,
    }
}

diesel::table! {
    /// Test table for complex default values.
    complex_defaults (id) {
        /// Primary key.
        id -> Integer,
        /// Negative default value.
        negative_value -> Integer,
        /// Float default value.
        calculated -> Float,
        /// Boolean default value.
        bool_default -> Integer,
        /// Text default value.
        text_default -> Nullable<Text>,
    }
}

diesel::table! {
    /// Test table for UPSERT operations.
    upsert_test (key) {
        /// Primary key.
        key -> Text,
        /// Value field.
        value -> Text,
        /// Counter field.
        counter -> Integer,
    }
}

/// JSON data record.
#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = json_data)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct JsonData {
    /// Primary key.
    id: i32,
    /// Optional JSON metadata.
    metadata: Option<String>,
    /// Required JSON settings.
    settings: String,
}

/// New JSON data for insertion.
#[derive(Insertable)]
#[diesel(table_name = json_data)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NewJsonData {
    /// Optional JSON metadata.
    metadata: Option<String>,
    /// Required JSON settings.
    settings: String,
}

/// Complex defaults record.
#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = complex_defaults)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct ComplexDefaults {
    /// Primary key.
    id: i32,
    /// Negative default value.
    negative_value: i32,
    /// Float default value.
    calculated: f32,
    /// Boolean default value.
    bool_default: i32,
    /// Text default value.
    text_default: Option<String>,
}

/// UPSERT test record.
#[derive(Queryable, Selectable, Insertable, Debug)]
#[diesel(table_name = upsert_test)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct UpsertRow {
    /// Primary key.
    key: String,
    /// Value field.
    value: String,
    /// Counter field.
    counter: i32,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/data_types_extended.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for extended data types translation.
#[test]
fn test_data_types_extended_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/data_types_extended.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("data_types_extended_translation", translated_sql);

    Ok(())
}

/// Test JSON type works (stored as TEXT).
#[test]
fn test_json_type_stored_as_text() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert JSON data as text
    diesel::insert_into(json_data::table)
        .values(NewJsonData {
            metadata: Some("{\"key\": \"value\", \"number\": 42}".to_string()),
            settings: "{\"theme\": \"dark\"}".to_string(),
        })
        .execute(&mut connection)?;

    let row = json_data::table.select(JsonData::as_select()).first(&mut connection)?;
    assert_eq!(row.metadata, Some("{\"key\": \"value\", \"number\": 42}".to_string()));
    assert_eq!(row.settings, "{\"theme\": \"dark\"}".to_string());

    Ok(())
}

/// Test JSONB default value works.
#[test]
fn test_jsonb_default_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert with only metadata (settings should default to '{}')
    // Note: We use diesel::sql_query here because Diesel's Insertable requires all
    // non-nullable fields without defaults to be provided, but the schema has a
    // default.
    diesel::sql_query("INSERT INTO json_data (metadata) VALUES ('{\"test\": true}')")
        .execute(&mut connection)?;

    let row = json_data::table.select(JsonData::as_select()).first(&mut connection)?;
    assert_eq!(row.settings, "{}");

    Ok(())
}

/// Test that Array types cause an error (not yet supported).
#[test]
fn test_array_type_causes_error() {
    let sql = "CREATE TABLE array_data (id SERIAL PRIMARY KEY, tags TEXT[]);";

    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Array"), "Error should mention Array type: {}", err);
}

/// Test negative default value works.
#[test]
fn test_negative_default_value() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert with defaults
    diesel::sql_query("INSERT INTO complex_defaults DEFAULT VALUES").execute(&mut connection)?;

    let row =
        complex_defaults::table.select(ComplexDefaults::as_select()).first(&mut connection)?;
    assert_eq!(row.negative_value, -1);

    Ok(())
}

/// Test float default value works.
#[test]
fn test_float_default_value() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::sql_query("INSERT INTO complex_defaults DEFAULT VALUES").execute(&mut connection)?;

    let row =
        complex_defaults::table.select(ComplexDefaults::as_select()).first(&mut connection)?;
    assert!((row.calculated - 3.14).abs() < 0.001);

    Ok(())
}

/// Test boolean default value works.
#[test]
fn test_boolean_default_value() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::sql_query("INSERT INTO complex_defaults DEFAULT VALUES").execute(&mut connection)?;

    let row =
        complex_defaults::table.select(ComplexDefaults::as_select()).first(&mut connection)?;
    assert_eq!(row.bool_default, 1); // SQLite stores true as 1

    Ok(())
}

/// Test text default value works.
#[test]
fn test_text_default_value() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::sql_query("INSERT INTO complex_defaults DEFAULT VALUES").execute(&mut connection)?;

    let row =
        complex_defaults::table.select(ComplexDefaults::as_select()).first(&mut connection)?;
    assert_eq!(row.text_default, Some("hello".to_string()));

    Ok(())
}

/// Test ON CONFLICT DO UPDATE (UPSERT) works semantically.
#[test]
fn test_upsert_insert_new_row() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, counter INTEGER NOT NULL DEFAULT 0);
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // First insert
    diesel::sql_query("INSERT INTO kv (key, value, counter) VALUES ('a', 'first', 1)")
        .execute(&mut connection)?;

    #[derive(QueryableByName, Debug)]
    struct KvRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        key: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        value: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        counter: i32,
    }

    let row = diesel::sql_query("SELECT key, value, counter FROM kv WHERE key = 'a'")
        .get_result::<KvRow>(&mut connection)?;

    assert_eq!(row.key, "a");
    assert_eq!(row.value, "first");
    assert_eq!(row.counter, 1);

    Ok(())
}

/// Test ON CONFLICT DO UPDATE updates existing row.
#[test]
fn test_upsert_update_existing_row() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, counter INTEGER NOT NULL DEFAULT 0);
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // First insert
    diesel::sql_query("INSERT INTO kv (key, value, counter) VALUES ('a', 'first', 1)")
        .execute(&mut connection)?;

    // Upsert - should update existing row
    diesel::sql_query(
        "INSERT INTO kv (key, value, counter) VALUES ('a', 'second', 2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, counter = counter + 1",
    )
    .execute(&mut connection)?;

    #[derive(QueryableByName, Debug)]
    struct KvRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        key: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        value: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        counter: i32,
    }

    let row = diesel::sql_query("SELECT key, value, counter FROM kv WHERE key = 'a'")
        .get_result::<KvRow>(&mut connection)?;

    assert_eq!(row.key, "a");
    assert_eq!(row.value, "second"); // Updated from excluded
    assert_eq!(row.counter, 2); // counter + 1 = 1 + 1 = 2

    Ok(())
}

/// Test that all default types work together.
#[test]
fn test_all_defaults_together() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::sql_query("INSERT INTO complex_defaults DEFAULT VALUES").execute(&mut connection)?;

    let row =
        complex_defaults::table.select(ComplexDefaults::as_select()).first(&mut connection)?;

    assert_eq!(row.negative_value, -1);
    assert!((row.calculated - 3.14).abs() < 0.001);
    assert_eq!(row.bool_default, 1);
    assert_eq!(row.text_default, Some("hello".to_string()));

    Ok(())
}

/// Test that GIN index without to_tsvector causes an error.
#[test]
fn test_gin_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, content TEXT);
        CREATE INDEX idx_content ON documents USING GIN (content);
    ";

    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("to_tsvector"), "Error should mention to_tsvector: {}", err);
}

/// Test that GiST index on non-tsvector column causes an error.
#[test]
fn test_gist_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE locations (id SERIAL PRIMARY KEY, point TEXT);
        CREATE INDEX idx_point ON locations USING GiST (point);
    ";

    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("GiST"), "Error should mention GiST index: {}", err);
}

/// Test that GiST index with to_tsvector translates to FTS5 (same as GIN).
#[test]
fn test_gist_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE articles (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON articles USING GiST (to_tsvector('english', title || ' ' || body));
    ";

    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let translated_sql: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    // Should have: table + FTS5 virtual table + 3 triggers + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE articles"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("articles_fts"),
        "Expected articles_fts table name, got: {}",
        translated_sql[1]
    );
}

/// Test that GIN index with to_tsvector translates to FTS5.
#[test]
fn test_gin_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON documents USING GIN (to_tsvector('english', title || ' ' || body));
    ";

    let options = Pg2SqliteOptions::default();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let translated_sql: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    // Should have: table + FTS5 virtual table + 3 triggers (insert, delete, update)
    // + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE documents"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("documents_fts"),
        "Expected documents_fts table name, got: {}",
        translated_sql[1]
    );
    assert!(
        translated_sql[1].contains("title"),
        "Expected title column, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("body"), "Expected body column, got: {}", translated_sql[1]);

    // Statements 3-5 should be triggers
    assert!(
        translated_sql[2].contains("CREATE TRIGGER") && translated_sql[2].contains("AFTER INSERT"),
        "Expected INSERT trigger, got: {}",
        translated_sql[2]
    );
    assert!(
        translated_sql[3].contains("CREATE TRIGGER") && translated_sql[3].contains("AFTER DELETE"),
        "Expected DELETE trigger, got: {}",
        translated_sql[3]
    );
    assert!(
        translated_sql[4].contains("CREATE TRIGGER") && translated_sql[4].contains("AFTER UPDATE"),
        "Expected UPDATE trigger, got: {}",
        translated_sql[4]
    );
}
