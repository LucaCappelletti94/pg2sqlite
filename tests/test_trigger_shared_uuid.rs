//! Test that trigger translation properly uses `last_insert_rowid()` pattern
//! to share UUIDs across multiple INSERTs in a single trigger.

mod helpers;

use diesel::prelude::*;
use helpers::{Count, IdResult, establish_connection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};

/// Test that the trigger translation generates the last_insert_rowid() pattern
/// for sharing UUIDs across multiple INSERTs.
#[test]
fn test_trigger_translation_uses_last_insert_rowid() {
    let pg_sql = r"
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE TABLE parent_relations (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            entity_id UUID NOT NULL REFERENCES entities(id),
            parent_name TEXT NOT NULL
        );

        CREATE OR REPLACE FUNCTION create_entity_trigger()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := gen_random_uuid();
            
            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, 'system');
            INSERT INTO parent_relations (entity_id, parent_name) VALUES (v_new_id, 'root');
            
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER trigger_create_entity
            BEFORE INSERT ON entities
            FOR EACH ROW
            EXECUTE FUNCTION create_entity_trigger();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());

    let translated = Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap();

    // Join all statements into one string for inspection
    let sqlite_sql: String =
        translated.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n");

    // Snapshot the translated SQL for consistency checking
    insta::assert_snapshot!("trigger_last_insert_rowid_pattern", &sqlite_sql);

    // The second INSERT (ownables) should use last_insert_rowid() to get the UUID
    // from the first INSERT (entities)
    assert!(
        sqlite_sql.contains("last_insert_rowid()"),
        "Expected the trigger to use last_insert_rowid() pattern for sharing UUIDs.\nGenerated SQL:\n{sqlite_sql}",
    );

    // The pattern should be: SELECT id FROM entities WHERE rowid =
    // last_insert_rowid()
    assert!(
        sqlite_sql.contains("SELECT id FROM entities WHERE rowid = last_insert_rowid()"),
        "Expected subquery pattern for retrieving UUID from first INSERT.\nGenerated SQL:\n{sqlite_sql}",
    );
}

/// Test that the translated trigger actually works in SQLite.
/// This validates that:
/// 1. The same UUID is used in both tables (entities and ownables)
/// 2. Foreign key constraints are satisfied
#[test]
fn test_trigger_shared_uuid_works_in_sqlite() {
    let pg_sql = r"
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE OR REPLACE FUNCTION create_entity_with_ownable()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := gen_random_uuid();
            
            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, NEW.owner);
            
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        -- This trigger runs on a staging table to create both entity and ownable
        CREATE TABLE entity_staging (
            name TEXT NOT NULL,
            owner TEXT NOT NULL
        );

        CREATE TRIGGER trigger_create_entity_ownable
            AFTER INSERT ON entity_staging
            FOR EACH ROW
            EXECUTE FUNCTION create_entity_with_ownable();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());

    let translated = Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap();

    // Snapshot the translated SQL for consistency checking
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_shared_uuid_works_in_sqlite", translated_sql);

    // Execute the translated schema
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .expect("Failed to execute statement");
    }

    // Insert via the staging table, which should trigger both INSERTs
    diesel::sql_query("INSERT INTO entity_staging (name, owner) VALUES ('test_entity', 'alice')")
        .execute(&mut conn)
        .expect("Failed to insert into staging table");

    // Verify both tables have a row
    let entity_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM entities").get_result(&mut conn).unwrap();
    assert_eq!(entity_count.count, 1, "Expected 1 entity");

    let ownable_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM ownables").get_result(&mut conn).unwrap();
    assert_eq!(ownable_count.count, 1, "Expected 1 ownable");

    // Most importantly: verify they have the SAME UUID
    let entity_id: IdResult =
        diesel::sql_query("SELECT id FROM entities").get_result(&mut conn).unwrap();
    let ownable_id: IdResult =
        diesel::sql_query("SELECT id FROM ownables").get_result(&mut conn).unwrap();

    assert_eq!(entity_id.id, ownable_id.id, "Entity and ownable should have the same UUID");
}

/// Test the three-table case: entities → ownables → metadata
/// where all three need to share the same UUID.
#[test]
fn test_trigger_three_tables_shared_uuid() {
    #[derive(QueryableByName, Debug)]
    struct EntityIdResult {
        #[diesel(sql_type = diesel::sql_types::Binary)]
        entity_id: Vec<u8>,
    }

    let pg_sql = r"
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE TABLE metadata (
            entity_id UUID PRIMARY KEY REFERENCES entities(id),
            created_at TEXT NOT NULL DEFAULT 'now'
        );

        CREATE OR REPLACE FUNCTION create_full_entity()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := gen_random_uuid();
            
            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, 'system');
            INSERT INTO metadata (entity_id) VALUES (v_new_id);
            
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TABLE entity_input (
            name TEXT NOT NULL
        );

        CREATE TRIGGER trigger_create_full_entity
            AFTER INSERT ON entity_input
            FOR EACH ROW
            EXECUTE FUNCTION create_full_entity();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());

    let translated = Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap();

    // Snapshot the translated SQL for consistency checking
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_three_tables_shared_uuid", translated_sql);

    // Execute the translated schema
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .expect("Failed to execute statement");
    }

    // Insert via the input table
    diesel::sql_query("INSERT INTO entity_input (name) VALUES ('my_entity')")
        .execute(&mut conn)
        .expect("Failed to insert into input table");

    // Verify all three tables have a row
    let entity_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM entities").get_result(&mut conn).unwrap();
    let ownable_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM ownables").get_result(&mut conn).unwrap();
    let metadata_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM metadata").get_result(&mut conn).unwrap();

    assert_eq!(entity_count.count, 1);
    assert_eq!(ownable_count.count, 1);
    assert_eq!(metadata_count.count, 1);

    let entity_id: IdResult =
        diesel::sql_query("SELECT id FROM entities").get_result(&mut conn).unwrap();
    let ownable_id: IdResult =
        diesel::sql_query("SELECT id FROM ownables").get_result(&mut conn).unwrap();
    let metadata_entity_id: EntityIdResult =
        diesel::sql_query("SELECT entity_id FROM metadata").get_result(&mut conn).unwrap();

    assert_eq!(entity_id.id, ownable_id.id, "Entity and ownable should have the same UUID");
    assert_eq!(
        entity_id.id, metadata_entity_id.entity_id,
        "Entity and metadata should reference the same UUID"
    );
}
