//! Test properties (uniqueness, sortability, correctness) of generated UUIDs
//! using pure SQL.

use diesel::{
    prelude::*,
    sql_types::{Blob, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::*;
use uuid::Uuid;

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
    let utc_now = chrono::Utc::now();
    let seconds = utc_now.timestamp() as u64;
    let subsec_nanos = utc_now.timestamp_subsec_nanos();
    let counter = 0;
    let usable_counter_bits = 12;
    let timestamp =
        uuid::Timestamp::from_unix_time(seconds, subsec_nanos, counter, usable_counter_bits);
    Uuid::new_v7(timestamp).as_bytes().to_vec()
}

/// Returns a textual UUID v7, using the UTC timestamp.
fn uuidv7_text_impl() -> String {
    let utc_now = chrono::Utc::now();
    let seconds = utc_now.timestamp() as u64;
    let subsec_nanos = utc_now.timestamp_subsec_nanos();
    let counter = 0;
    let usable_counter_bits = 12;
    let timestamp =
        uuid::Timestamp::from_unix_time(seconds, subsec_nanos, counter, usable_counter_bits);
    Uuid::new_v7(timestamp).to_string()
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
    uuidv4_utils::register_impl(&connection, uuidv4_impl).expect("Failed to register uuidv4");
    uuidv7_utils::register_impl(&connection, uuidv7_impl).expect("Failed to register uuidv7");
    uuidv4_text_utils::register_impl(&connection, uuidv4_text_impl)
        .expect("Failed to register uuidv4_text");
    uuidv7_text_utils::register_impl(&connection, uuidv7_text_impl)
        .expect("Failed to register uuidv7_text");
    connection
}

fn get_create_table_sql(
    version: UuidVersion,
    repr: UuidRepresentation,
    table_name: &str,
    use_sql: bool,
) -> String {
    let func = match version {
        UuidVersion::V4 => "gen_random_uuid()",
        UuidVersion::V7 => "uuidv7()",
    };
    let sql = format!("CREATE TABLE {table_name} (id UUID PRIMARY KEY DEFAULT {func})");
    let translator = Pg2Sqlite::default().sql(&sql).unwrap();
    let mut options = Pg2SqliteOptions::default().with_uuid_representation(repr);

    if use_sql {
        options = options.use_pure_sql_for_uuid();
    } else {
        match (version, repr) {
            (UuidVersion::V4, UuidRepresentation::Blob) => {
                options = options.with_uuid_function_name("uuidv4".to_string());
            }
            (UuidVersion::V4, UuidRepresentation::Text) => {
                options = options.with_uuid_function_name("uuidv4_text".to_string());
            }
            (UuidVersion::V7, UuidRepresentation::Blob) => {
                options = options.with_uuid_function_name("uuidv7".to_string());
            }
            (UuidVersion::V7, UuidRepresentation::Text) => {
                options = options.with_uuid_function_name("uuidv7_text".to_string());
            }
        }
    }

    let translated = translator.translate(&options).unwrap();
    translated[0].to_string()
}

#[test]
fn test_v4_text_unique_and_valid() {
    for use_sql in [false, true] {
        let mut conn = establish_connection();

        let sql = get_create_table_sql(
            UuidVersion::V4,
            UuidRepresentation::Text,
            "users_v4_text",
            use_sql,
        );
        diesel::sql_query(sql).execute(&mut conn).unwrap();

        for _ in 0..100 {
            diesel::sql_query("INSERT INTO users_v4_text DEFAULT VALUES")
                .execute(&mut conn)
                .unwrap();
        }

        let results =
            diesel::sql_query("SELECT id FROM users_v4_text").load::<IdText>(&mut conn).unwrap();

        let mut ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
        let total = ids.len();

        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "Collision detected in V4 Text UUIDs");
        assert_eq!(total, 100);

        for id in ids {
            // xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
            assert_eq!(id.len(), 36);
            let parsed = Uuid::parse_str(&id).expect("Failed to parse Text V4 UUID");
            assert_eq!(
                parsed.get_version(),
                Some(uuid::Version::Random),
                "Incorrect UUID Version (expected V4)"
            );
            assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");

            assert_eq!(id, id.to_lowercase());
        }
    }
}

#[test]
fn test_v4_blob_unique_and_valid() {
    for use_sql in [false, true] {
        let mut conn = establish_connection();
        let sql = get_create_table_sql(
            UuidVersion::V4,
            UuidRepresentation::Blob,
            "users_v4_blob",
            use_sql,
        );
        diesel::sql_query(sql).execute(&mut conn).unwrap();

        for _ in 0..100 {
            diesel::sql_query("INSERT INTO users_v4_blob DEFAULT VALUES")
                .execute(&mut conn)
                .unwrap();
        }

        let results =
            diesel::sql_query("SELECT id FROM users_v4_blob").load::<IdBlob>(&mut conn).unwrap();

        let mut ids: Vec<Vec<u8>> = results.iter().map(|r| r.id.clone()).collect();
        let total = ids.len();

        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "Collision detected in V4 Blob UUIDs");
        assert_eq!(total, 100);

        for id in ids {
            assert_eq!(id.len(), 16);
            let parsed = Uuid::from_slice(&id).expect("Failed to parse Blob V4 UUID");
            assert_eq!(
                parsed.get_version(),
                Some(uuid::Version::Random),
                "Incorrect UUID Version (expected V4)"
            );
            assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");
        }
    }
}

#[test]
fn test_v7_text_sortable_and_valid() {
    for use_sql in [false, true] {
        let mut conn = establish_connection();
        let sql = get_create_table_sql(
            UuidVersion::V7,
            UuidRepresentation::Text,
            "users_v7_text",
            use_sql,
        );
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
            let parsed = Uuid::parse_str(&id).expect("Failed to parse Text V7 UUID");
            assert_eq!(
                parsed.get_version(),
                Some(uuid::Version::SortRand),
                "Incorrect UUID Version (expected V7)"
            );
            assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");
        }
    }
}

#[test]
fn test_v7_blob_sortable_and_valid() {
    for use_sql in [false, true] {
        let mut conn = establish_connection();
        let sql = get_create_table_sql(
            UuidVersion::V7,
            UuidRepresentation::Blob,
            "users_v7_blob",
            use_sql,
        );
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
            let parsed = Uuid::from_slice(&id).expect("Failed to parse Blob V7 UUID");
            assert_eq!(
                parsed.get_version(),
                Some(uuid::Version::SortRand),
                "Incorrect UUID Version (expected V7)"
            );
            assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");
        }
    }
}
