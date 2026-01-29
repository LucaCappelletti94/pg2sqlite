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

fn establish_connection() -> SqliteConnection {
    SqliteConnection::establish(":memory:").unwrap()
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
    let options =
        Pg2SqliteOptions::default().use_pure_sql_for_uuid(true).with_uuid_representation(repr);

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

#[test]
fn test_v4_blob_unique_and_valid() {
    let mut conn = establish_connection();
    let sql = get_create_table_sql(UuidVersion::V4, UuidRepresentation::Blob, "users_v4_blob");
    diesel::sql_query(sql).execute(&mut conn).unwrap();

    for _ in 0..100 {
        diesel::sql_query("INSERT INTO users_v4_blob DEFAULT VALUES").execute(&mut conn).unwrap();
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
        let parsed = Uuid::parse_str(&id).expect("Failed to parse Text V7 UUID");
        assert_eq!(
            parsed.get_version(),
            Some(uuid::Version::SortRand),
            "Incorrect UUID Version (expected V7)"
        );
        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");
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
        let parsed = Uuid::from_slice(&id).expect("Failed to parse Blob V7 UUID");
        assert_eq!(
            parsed.get_version(),
            Some(uuid::Version::SortRand),
            "Incorrect UUID Version (expected V7)"
        );
        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "Incorrect UUID Variant");
    }
}
