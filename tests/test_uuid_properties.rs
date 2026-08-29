//! Test properties (uniqueness, sortability, correctness) of generated UUIDs
//! using Rust-based UUID functions.

use core::str::FromStr;

use diesel::{
    prelude::*,
    sql_types::{Blob, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::*;
use rosetta_uuid::Uuid;

// Schema definitions for test tables
diesel::table! {
    /// Test table for UUID v4 text representation.
    users_v4_text (id) {
        /// UUID v4 as text.
        id -> Text,
    }
}

diesel::table! {
    /// Test table for UUID v4 blob representation.
    users_v4_blob (id) {
        /// UUID v4 as blob.
        id -> Binary,
    }
}

diesel::table! {
    /// Test table for UUID v7 text representation.
    users_v7_text (id) {
        /// UUID v7 as text.
        id -> Text,
    }
}

diesel::table! {
    /// Test table for UUID v7 blob representation.
    users_v7_blob (id) {
        /// UUID v7 as blob.
        id -> Binary,
    }
}

/// UUID as text (for v4/v7 text tests).
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = users_v4_text)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct UuidText {
    /// UUID as text.
    id: String,
}

/// UUID as blob (for v4/v7 blob tests).
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = users_v4_blob)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct UuidBlob {
    /// UUID as blob.
    id: Vec<u8>,
}

#[derive(QueryableByName, Debug, Clone)]
struct IdText {
    #[diesel(sql_type = Text)]
    id: String,
}

#[derive(QueryableByName, Debug, Clone)]
struct IdBlob {
    #[diesel(sql_type = Blob)]
    id: Vec<u8>,
}

#[declare_sql_function]
extern "SQL" {
    /// Generates a UUID v4
    fn uuidv4() -> Binary;
    /// Generates a UUID v7
    fn uuidv7() -> Binary;
    /// Generates a textual UUID v4
    fn uuidv4_text() -> Text;
    /// Generates a textual UUID v7
    fn uuidv7_text() -> Text;
}

/// Returns a UUID v4 as a byte vector
fn uuidv4_impl() -> Vec<u8> {
    Uuid::new_v4().as_bytes().to_vec()
}

/// Returns a textual UUID v4
fn uuidv4_text_impl() -> String {
    Uuid::new_v4().to_string()
}

/// Returns a UUID v7 as a byte vector, using the UTC timestamp.
fn uuidv7_impl() -> Vec<u8> {
    let bytes: [u8; 16] = Uuid::utc_v7().into();
    bytes.to_vec()
}

/// Returns a textual UUID v7, using the UTC timestamp.
fn uuidv7_text_impl() -> String {
    Uuid::utc_v7().to_string()
}

fn establish_connection() -> SqliteConnection {
    let mut connection =
        SqliteConnection::establish(":memory:").expect("Error connecting to in-memory SQLite");
    // Enable foreign key constraints
    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .expect("Failed to enable foreign key constraints");

    // Enable recursive triggers
    diesel::sql_query("PRAGMA recursive_triggers = ON")
        .execute(&mut connection)
        .expect("Failed to enable recursive triggers");

    // Set journal mode to WAL for better performance
    diesel::sql_query("PRAGMA journal_mode = WAL")
        .execute(&mut connection)
        .expect("Failed to set journal mode to WAL");
    uuidv4_utils::register_impl(&mut connection, uuidv4_impl).expect("Failed to register uuidv4");
    uuidv7_utils::register_impl(&mut connection, uuidv7_impl).expect("Failed to register uuidv7");
    uuidv4_text_utils::register_impl(&mut connection, uuidv4_text_impl)
        .expect("Failed to register uuidv4_text");
    uuidv7_text_utils::register_impl(&mut connection, uuidv7_text_impl)
        .expect("Failed to register uuidv7_text");
    connection
}

fn get_create_table_sql(
    version: UuidVersion,
    repr: UuidRepresentation,
    table_name: &str,
) -> String {
    let func = match version {
        UuidVersion::V4 => "gen_random_uuid()",
        UuidVersion::V7 => "uuidv7()",
    };
    let sql = format!("CREATE TABLE {table_name} (id UUID PRIMARY KEY DEFAULT {func})");
    let translator = Pg2Sqlite::default().sql(&sql).unwrap();

    let uuid_func_name = match (version, repr) {
        (UuidVersion::V4, UuidRepresentation::Blob) => "uuidv4",
        (UuidVersion::V4, UuidRepresentation::Text) => "uuidv4_text",
        (UuidVersion::V7, UuidRepresentation::Blob) => "uuidv7",
        (UuidVersion::V7, UuidRepresentation::Text) => "uuidv7_text",
    };

    let options = Pg2SqliteOptions::default().with_uuid_representation(repr);
    // The two generators are named separately, so the version under test picks
    // which option carries the name.
    let options = match version {
        UuidVersion::V4 => options.with_uuid_function_name(uuid_func_name),
        UuidVersion::V7 => options.with_uuid_v7_function_name(uuid_func_name),
    };

    let translated = translator.translate(&options).unwrap();
    translated[0].to_string()
}

#[test]
fn test_v4_text_unique_and_valid() {
    let mut conn = establish_connection();

    let sql = get_create_table_sql(UuidVersion::V4, UuidRepresentation::Text, "users_v4_text");
    diesel::sql_query(sql).execute(&mut conn).unwrap();

    for _ in 0..100 {
        diesel::sql_query("INSERT INTO users_v4_text DEFAULT VALUES").execute(&mut conn).unwrap();
    }

    let results = users_v4_text::table.select(UuidText::as_select()).load(&mut conn).unwrap();

    let mut ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let total = ids.len();

    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "Collision detected in V4 Text UUIDs");
    assert_eq!(total, 100);

    for id in ids {
        // xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
        assert_eq!(id.len(), 36);
        let parsed = Uuid::from_str(&id).expect("Failed to parse Text V4 UUID");
        assert_eq!(parsed.get_version_num(), 4, "Incorrect UUID Version (expected V4)");
        // Check variant bits (should be 10xx for RFC4122)
        let variant_byte = id.as_bytes()[19]; // Position of variant nibble
        assert!(
            variant_byte == b'8'
                || variant_byte == b'9'
                || variant_byte == b'a'
                || variant_byte == b'b',
            "Incorrect UUID Variant"
        );

        assert_eq!(id, id.to_lowercase());
    }
}

#[test]
fn test_v4_blob_unique_and_valid() {
    let mut conn = establish_connection();
    let sql = get_create_table_sql(UuidVersion::V4, UuidRepresentation::Blob, "users_v4_blob");
    diesel::sql_query(sql).execute(&mut conn).unwrap();

    for _ in 0..100 {
        diesel::sql_query("INSERT INTO users_v4_blob DEFAULT VALUES").execute(&mut conn).unwrap();
    }

    let results = users_v4_blob::table.select(UuidBlob::as_select()).load(&mut conn).unwrap();

    let mut ids: Vec<Vec<u8>> = results.iter().map(|r| r.id.clone()).collect();
    let total = ids.len();

    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "Collision detected in V4 Blob UUIDs");
    assert_eq!(total, 100);

    for id in ids {
        assert_eq!(id.len(), 16);
        let bytes: [u8; 16] = id.clone().try_into().expect("Invalid UUID length");
        let parsed = Uuid::from(bytes);
        assert_eq!(parsed.get_version_num(), 4, "Incorrect UUID Version (expected V4)");
        // Check variant bits in byte 8 (should be 10xx xxxx = 0x80-0xBF)
        assert!(id[8] >= 0x80 && id[8] <= 0xBF, "Incorrect UUID Variant");
    }
}

#[test]
fn test_v7_text_sortable_and_valid() {
    let mut conn = establish_connection();
    let sql = get_create_table_sql(UuidVersion::V7, UuidRepresentation::Text, "users_v7_text");
    diesel::sql_query(sql).execute(&mut conn).unwrap();

    // Insert 1st item
    diesel::sql_query("INSERT INTO users_v7_text DEFAULT VALUES").execute(&mut conn).unwrap();

    // Wait > 1s for unixepoch('now') resolution
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Insert 2nd item
    diesel::sql_query("INSERT INTO users_v7_text DEFAULT VALUES").execute(&mut conn).unwrap();

    // Retrieve in approximate insertion order (using rowid)
    let results = diesel::sql_query("SELECT id FROM users_v7_text ORDER BY rowid")
        .load::<IdText>(&mut conn)
        .unwrap();

    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

    assert_eq!(ids.len(), 2);
    assert!(ids[0] < ids[1], "UUID V7 Text not time-ordered across 1s boundary");

    for id in ids {
        assert_eq!(id.len(), 36);
        let parsed = Uuid::from_str(&id).expect("Failed to parse Text V7 UUID");
        assert_eq!(parsed.get_version_num(), 7, "Incorrect UUID Version (expected V7)");
        // Check variant bits (should be 10xx for RFC4122)
        let variant_byte = id.as_bytes()[19];
        assert!(
            variant_byte == b'8'
                || variant_byte == b'9'
                || variant_byte == b'a'
                || variant_byte == b'b',
            "Incorrect UUID Variant"
        );
    }
}

#[test]
fn test_v7_blob_sortable_and_valid() {
    let mut conn = establish_connection();
    let sql = get_create_table_sql(UuidVersion::V7, UuidRepresentation::Blob, "users_v7_blob");
    diesel::sql_query(sql).execute(&mut conn).unwrap();

    // Insert 1st item
    diesel::sql_query("INSERT INTO users_v7_blob DEFAULT VALUES").execute(&mut conn).unwrap();

    // Wait > 1s
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Insert 2nd item
    diesel::sql_query("INSERT INTO users_v7_blob DEFAULT VALUES").execute(&mut conn).unwrap();

    let results = diesel::sql_query("SELECT id FROM users_v7_blob ORDER BY rowid")
        .load::<IdBlob>(&mut conn)
        .unwrap();

    let ids: Vec<Vec<u8>> = results.iter().map(|r| r.id.clone()).collect();

    assert_eq!(ids.len(), 2);
    // lexicographical comparison of bytes works for UUID v7
    assert!(ids[0] < ids[1], "UUID V7 Blob not time-ordered across 1s boundary");

    for id in ids {
        assert_eq!(id.len(), 16, "UUID Blob length incorrect");
        let bytes: [u8; 16] = id.clone().try_into().expect("Invalid UUID length");
        let parsed = Uuid::from(bytes);
        assert_eq!(parsed.get_version_num(), 7, "Incorrect UUID Version (expected V7)");
        // Check variant bits in byte 8 (should be 10xx xxxx = 0x80-0xBF)
        assert!(id[8] >= 0x80 && id[8] <= 0xBF, "Incorrect UUID Variant");
    }
}

/// The API must accept a string literal (&str), not require `.to_string()`.
#[test]
fn uuid_function_name_accepts_str_literal() -> Result<(), Box<dyn std::error::Error>> {
    // This should compile with a &str literal, not requiring String::from(...)
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Text)
        .with_uuid_function_name("my_uuid_fn"); // &str, not String
    // Use gen_random_uuid() as DEFAULT so the function name appears in output.
    let sql = "CREATE TABLE uuid_str_test (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), name TEXT NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(
        ddl.contains("my_uuid_fn"),
        "Custom UUID function name must appear in DEFAULT, got: {ddl}"
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{stmt}"));
    }
    Ok(())
}
