//! Tests for FTS5 full-text search with Row Level Security (RLS).
//!
//! These tests verify that when a table has RLS enabled, FTS5 synchronization
//! triggers are correctly attached to the backing table (e.g., `documents_rls`)
//! instead of the view (e.g., `documents`).

#![allow(dead_code, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod helpers;
use helpers::establish_connection;

// Schema definitions for test tables
//
// NOTE: The `documents_rls` backing table schema is ONLY for testing.
// Real applications should only define the `documents` view schema.
// Backing tables are implementation details of RLS translation.

diesel::table! {
    /// Backing table for documents with RLS.
    ///
    /// **Testing only** - Real applications should use the `documents` view schema.
    documents_rls (id) {
        /// Document ID.
        id -> Nullable<Integer>,
        /// Document title.
        title -> Text,
        /// Document content.
        content -> Text,
    }
}

diesel::table! {
    /// Documents view (RLS-filtered).
    ///
    /// **This is what real applications should use** for transparent RLS enforcement.
    documents (id) {
        /// Document ID.
        id -> Nullable<Integer>,
        /// Document title.
        title -> Text,
        /// Document content.
        content -> Text,
    }
}

diesel::table! {
    /// FTS5 virtual table for full-text search.
    documents_fts (rowid) {
        /// Row ID.
        rowid -> Integer,
    }
}

/// A document record.
#[derive(Insertable)]
#[diesel(table_name = documents)]
struct Document<'a> {
    /// Document title.
    title: &'a str,
    /// Document content.
    content: &'a str,
}

/// Snapshot test: verify that FTS5 triggers reference the backing table
/// (documents_rls) instead of the view (documents).
#[test]
fn test_fts5_rls_triggers_reference_backing_table() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let fixture = include_str!("fixtures/fts5_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Find the INSERT trigger
    let insert_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_fts_ai"))
        .expect("Should have INSERT trigger for FTS5");

    // Find the UPDATE trigger
    let update_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_fts_au"))
        .expect("Should have UPDATE trigger for FTS5");

    // Find the DELETE trigger
    let delete_trigger = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TRIGGER") && s.contains("_fts_ad"))
        .expect("Should have DELETE trigger for FTS5");

    assert!(
        insert_trigger.contains("ON documents_rls"),
        "INSERT trigger should be ON documents_rls (backing table), got: {insert_trigger}"
    );

    assert!(
        update_trigger.contains("ON documents_rls"),
        "UPDATE trigger should be ON documents_rls (backing table), got: {update_trigger}"
    );

    assert!(
        delete_trigger.contains("ON documents_rls"),
        "DELETE trigger should be ON documents_rls (backing table), got: {delete_trigger}"
    );

    Ok(())
}

/// Diesel functionality test: verify that FTS5 search actually works with RLS
/// tables.
#[test]
fn test_fts5_search_works_with_rls() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let mut conn = establish_connection();
    let fixture = include_str!("fixtures/fts5_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    // Execute translated SQL
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert document via view (which has INSTEAD OF trigger)
    // The INSTEAD OF trigger should insert into documents_rls
    diesel::insert_into(documents::table)
        .values(&Document {
            title: "Rust Programming",
            content: "Learn about Rust systems programming language",
        })
        .execute(&mut conn)?;

    diesel::insert_into(documents::table)
        .values(&Document { title: "Python Tutorial", content: "Python for data science" })
        .execute(&mut conn)?;

    // Check that documents were inserted into the backing table
    let count: i64 = documents_rls::table.count().get_result(&mut conn)?;

    assert_eq!(count, 2, "Should have 2 documents in backing table");

    // Search using FTS5. Note: FTS5 queries require raw SQL since they use
    // the MATCH operator.
    let results: Vec<helpers::Count> = diesel::sql_query(
        "SELECT COUNT(*) as count FROM documents_fts WHERE documents_fts MATCH 'Rust'",
    )
    .load(&mut conn)?;

    assert!(!results.is_empty() && results[0].count > 0, "Should find search results for 'Rust'");

    // Verify we can find both documents with different search terms
    let rust_count: i64 = diesel::sql_query(
        "SELECT COUNT(*) as count FROM documents_fts WHERE documents_fts MATCH 'programming'",
    )
    .get_result::<helpers::Count>(&mut conn)?
    .count;

    assert_eq!(rust_count, 1, "Should find 1 result for 'programming'");

    let python_count: i64 = diesel::sql_query(
        "SELECT COUNT(*) as count FROM documents_fts WHERE documents_fts MATCH 'Python'",
    )
    .get_result::<helpers::Count>(&mut conn)?
    .count;

    assert_eq!(python_count, 1, "Should find 1 result for 'Python'");

    Ok(())
}

/// Snapshot test to verify the full translated SQL output.
#[test]
fn test_fts5_rls_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let fixture = include_str!("fixtures/fts5_rls.sql");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());
    let translated = Pg2Sqlite::default().sql(fixture)?.translate(&options)?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("fts5_rls_translation", translated_sql);

    Ok(())
}
