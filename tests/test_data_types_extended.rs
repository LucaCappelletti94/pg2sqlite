//! Tests for extended data type support: JSON, JSONB, Arrays, complex defaults,
//! UPSERT.
//!
//! These tests verify that the new type mappings work correctly at runtime.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        json_data (id) {
            id -> Integer,
            metadata -> Nullable<Text>,
            settings -> Text,
        }
    }

    diesel::table! {
        array_data (id) {
            id -> Integer,
            tags -> Nullable<Text>,
            scores -> Nullable<Text>,
        }
    }

    diesel::table! {
        complex_defaults (id) {
            id -> Integer,
            negative_value -> Integer,
            calculated -> Float,
            bool_default -> Integer,
            text_default -> Nullable<Text>,
        }
    }

    diesel::table! {
        upsert_test (key) {
            key -> Text,
            value -> Text,
            counter -> Integer,
        }
    }
}

use schema::{array_data, complex_defaults, json_data, upsert_test};

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = json_data)]
struct JsonData {
    id: i32,
    metadata: Option<String>,
    settings: String,
}

#[derive(Insertable)]
#[diesel(table_name = json_data)]
struct NewJsonData {
    metadata: Option<String>,
    settings: String,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = array_data)]
struct ArrayData {
    id: i32,
    tags: Option<String>,
    scores: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = array_data)]
struct NewArrayData {
    tags: Option<String>,
    scores: Option<String>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = complex_defaults)]
struct ComplexDefaults {
    id: i32,
    negative_value: i32,
    calculated: f32,
    bool_default: i32,
    text_default: Option<String>,
}

#[derive(Queryable, Selectable, Insertable, Debug)]
#[diesel(table_name = upsert_test)]
struct UpsertRow {
    key: String,
    value: String,
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
            metadata: Some(r#"{"key": "value", "number": 42}"#.to_string()),
            settings: r#"{"theme": "dark"}"#.to_string(),
        })
        .execute(&mut connection)?;

    let row = json_data::table.select(JsonData::as_select()).first(&mut connection)?;
    assert_eq!(row.metadata, Some(r#"{"key": "value", "number": 42}"#.to_string()));
    assert_eq!(row.settings, r#"{"theme": "dark"}"#.to_string());

    Ok(())
}

/// Test JSONB default value works.
#[test]
fn test_jsonb_default_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert with only metadata (settings should default to '{}')
    diesel::sql_query("INSERT INTO json_data (metadata) VALUES ('{\"test\": true}')")
        .execute(&mut connection)?;

    let row = json_data::table.select(JsonData::as_select()).first(&mut connection)?;
    assert_eq!(row.settings, "{}");

    Ok(())
}

/// Test Array type works (stored as TEXT).
#[test]
fn test_array_type_stored_as_text() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert array data as JSON text (common serialization format)
    diesel::insert_into(array_data::table)
        .values(NewArrayData {
            tags: Some(r#"["rust", "sqlite", "postgresql"]"#.to_string()),
            scores: Some(r#"[100, 95, 87]"#.to_string()),
        })
        .execute(&mut connection)?;

    let row = array_data::table.select(ArrayData::as_select()).first(&mut connection)?;
    assert_eq!(row.tags, Some(r#"["rust", "sqlite", "postgresql"]"#.to_string()));
    assert_eq!(row.scores, Some(r#"[100, 95, 87]"#.to_string()));

    Ok(())
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
    let sql = r#"
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, counter INTEGER NOT NULL DEFAULT 0);
    "#;
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
    let sql = r#"
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, counter INTEGER NOT NULL DEFAULT 0);
    "#;
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
