//! Tests for vector (sqlite-vec) with Row Level Security (RLS).
//!
//! These tests verify that when a table has RLS enabled, vec0 synchronization
//! triggers are correctly attached to the backing table (e.g.,
//! `embeddings_rls`) instead of the view (e.g., `embeddings`).

#![allow(dead_code, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod helpers;
use helpers::establish_connection;

// Schema definitions for test tables
//
// NOTE: The `embeddings_rls` backing table schema is ONLY for testing.
// Real applications should only define the `embeddings` view schema.
// Backing tables are implementation details of RLS translation.

diesel::table! {
    /// Backing table for embeddings with RLS.
    ///
    /// **Testing only** - Real applications should use the `embeddings` view schema.
    embeddings_rls (id) {
        /// Embedding ID.
        id -> Nullable<Integer>,
        /// Document content.
        content -> Text,
        /// Vector embedding data.
        embedding -> Binary,
    }
}

diesel::table! {
    /// Embeddings view (RLS-filtered).
    ///
    /// **This is what real applications should use** for transparent RLS enforcement.
    embeddings (id) {
        /// Embedding ID.
        id -> Nullable<Integer>,
        /// Document content.
        content -> Text,
        /// Vector embedding data.
        embedding -> Binary,
    }
}

diesel::table! {
    /// Vec0 virtual table for vector similarity search.
    embeddings_embedding_vec (rowid) {
        /// Row ID.
        rowid -> Integer,
        /// Vector data.
        embedding -> Binary,
    }
}

/// An embedding record.
#[derive(Insertable)]
#[diesel(table_name = embeddings)]
struct Embedding<'a> {
    /// Document content.
    content: &'a str,
    /// Vector embedding data.
    embedding: &'a [u8],
}

/// Snapshot test: verify that vec0 triggers reference the backing table
/// (embeddings_rls) instead of the view (embeddings).
#[test]
fn test_vec0_rls_triggers_reference_backing_table() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = include_str!("fixtures/vector_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Find the INSERT trigger for vec0
    let insert_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_vec_ai"))
        .expect("Should have INSERT trigger for vec0");

    // Find the UPDATE trigger
    let update_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_vec_au"))
        .expect("Should have UPDATE trigger for vec0");

    // Find the DELETE trigger
    let delete_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_vec_ad"))
        .expect("Should have DELETE trigger for vec0");

    assert!(
        insert_trigger.contains("ON embeddings_rls"),
        "INSERT trigger should be ON embeddings_rls (backing table), got: {insert_trigger}"
    );

    assert!(
        update_trigger.contains("ON embeddings_rls"),
        "UPDATE trigger should be ON embeddings_rls (backing table), got: {update_trigger}"
    );

    assert!(
        delete_trigger.contains("ON embeddings_rls"),
        "DELETE trigger should be ON embeddings_rls (backing table), got: {delete_trigger}"
    );

    // Execute translated DDL (skip vec0 virtual tables that need the sqlite-vec
    // extension loaded, which is unavailable in the standard test runtime).
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for s in &translated_sql {
            if s.contains("USING vec0") {
                continue;
            }
            conn.execute_batch(&format!("{s};")).expect("translated SQL must execute in SQLite");
        }
    }
    Ok(())
}

/// Diesel functionality test: verify that vec0 sync actually works with RLS
/// tables.
///
/// Note: Ignored because sqlite-vec needs to be loaded via rusqlite.
/// See test_vector_semantic.rs for proper sqlite-vec testing patterns.
#[test]
#[ignore = "sqlite-vec requires rusqlite, see test_vector_semantic.rs"]
fn test_vec0_sync_works_with_rls() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    let fixture = include_str!("fixtures/vector_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    // Execute translated SQL
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert embedding via view (which has INSTEAD OF trigger)
    // The INSTEAD OF trigger should insert into embeddings_rls
    // Create a zeroblob of 1536 bytes for the embedding
    let zeroblob = vec![0u8; 1536];
    diesel::insert_into(embeddings::table)
        .values(&Embedding { content: "test document", embedding: &zeroblob })
        .execute(&mut conn)?;

    // Check that embedding was inserted into the backing table
    let count: i64 = embeddings_rls::table.count().get_result(&mut conn)?;

    assert_eq!(count, 1, "Should have 1 embedding in backing table");

    // Check that vec0 table was synchronized.
    let vec0_count: i64 = embeddings_embedding_vec::table.count().get_result(&mut conn)?;

    assert_eq!(vec0_count, 1, "Should have 1 entry in vec0 table");

    Ok(())
}

/// Regression test: role-filtered table translation must still emit vec0
/// artifacts for vector columns.
#[test]
fn test_role_filtered_vector_translation_keeps_vec0_artifacts()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE ROLE readonly_user;

        CREATE TABLE embeddings (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL,
            embedding vector(3)
        );

        ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY;
        CREATE POLICY embeddings_policy ON embeddings FOR ALL TO PUBLIC USING (true);

        GRANT SELECT ON embeddings TO readonly_user;
    "#;

    let options = Pg2SqliteOptions::default()
        .with_session_user_role("readonly_user".to_string())
        .with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        output.contains("CREATE VIRTUAL TABLE embeddings_embedding_vec USING vec0"),
        "Role-filtered translation should still emit vec0 table, got:\n{output}"
    );
    assert!(
        output.contains("AFTER INSERT ON embeddings_rls"),
        "Role-filtered translation should emit vec0 triggers on backing table, got:\n{output}"
    );

    // Execute translated DDL (skip vec0 virtual tables that need the sqlite-vec
    // extension loaded, which is unavailable in the standard test runtime).
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            let s = stmt.to_string();
            if s.contains("USING vec0") {
                continue;
            }
            conn.execute_batch(&format!("{s};")).expect("translated SQL must execute in SQLite");
        }
    }
    Ok(())
}

/// Snapshot test to verify the full translated SQL output.
#[test]
fn test_vector_rls_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = include_str!("fixtures/vector_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("vector_rls_translation", translated_sql);

    Ok(())
}
